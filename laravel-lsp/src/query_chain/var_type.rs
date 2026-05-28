//! Resolve a chain-receiver variable's declared PHP class.
//!
//! Used when a chain starts at an instance variable like `$user->newQuery()`.
//! We need to know `$user` is a `User` to power column / relation completion
//! against the right model. The resolution chain is layered:
//!
//! 1. **Latest assignment in scope** (flow-tracked) — `$user = User::find($id);`
//!    overrides any declared type for `$user`, because PHP allows free
//!    reassignment. Lives in [`super::flow`].
//!
//! 2. **Typed function/method parameter.** `function show(User $user)` →
//!    `$user`'s declared type is `User`. Covers the common case in
//!    controllers and services where action methods take typed args.
//!
//! 3. **`@var` docblock.** PHPDoc allows `/** @var User $u */` immediately
//!    above an assignment. Used in places where PHP's static type system
//!    isn't sufficient (e.g. `$u = $repo->findOne(...);` whose return type
//!    is too generic).
//!
//! All strategies resolve the bare class name through the file's `use`
//! aliases via [`crate::query_chain::use_aliases::resolve_class_name`], so a
//! parameter typed `Article` with `use App\Models\Post as Article;` returns
//! `App\Models\Post` — ready to feed into Composer autoload.
//!
//! The actual scope-walking, flow analysis, and `use ($var)` capture
//! traversal lives in [`super::flow`]. This module exposes the per-scope
//! declared-type helpers (`typed_param_in`, `docblock_in`) that the flow
//! resolver uses at each scope hop, plus the public `resolve` entry point
//! which is now a thin wrapper around `flow::resolve`.
//!
//! Misses (no type info, ambiguous type, unresolvable name) return `None`
//! rather than guessing. Better no completion than a wrong one.

use tree_sitter::Node;

use super::use_aliases::UseAliases;

/// Return the resolved FQCN for `var_name` at `variable_node`'s position,
/// or `None` if no strategy yields a type. Caller stores the result in
/// `EloquentReceiver::InstanceVar::php_type` for later use by the cursor
/// context resolver.
///
/// Delegates to [`super::flow::resolve`], which walks scope-by-scope
/// applying flow > typed param > docblock, recursing into outer scopes
/// via `use ($var)` capture.
pub fn resolve(
    variable_node: Node,
    bytes: &[u8],
    var_name: &str,
    aliases: &UseAliases,
) -> Option<String> {
    super::flow::resolve(variable_node, bytes, var_name, aliases)
}

/// Scan `scope`'s formal parameters for one named `var_name`. Returns the
/// raw type string as written in source — caller handles use-alias
/// resolution. `None` for untyped or missing parameters.
///
/// `scope` must be a function-like node (function_definition,
/// method_declaration, anonymous_function, arrow_function). Caller is
/// responsible for finding the right scope; this function does not walk
/// up the tree.
pub fn typed_param_in(scope: Node, bytes: &[u8], var_name: &str) -> Option<String> {
    if !matches!(
        scope.kind(),
        "method_declaration"
            | "function_definition"
            | "anonymous_function_creation_expression"
            | "anonymous_function"
            | "arrow_function"
    ) {
        return None;
    }
    scan_formal_parameters(scope, bytes, var_name)
}

/// Find a `@var $var_name <type>` (or `@var <type> $var_name`) docblock
/// anywhere inside `scope`. `scope` is typically a function-like or
/// `program` node. First match wins; we don't try to pick the "closest"
/// docblock to a specific assignment — by convention PHP docblocks
/// declare a variable once per scope.
pub fn docblock_in(scope: Node, bytes: &[u8], var_name: &str) -> Option<String> {
    scan_comments_for_var(scope, bytes, var_name)
}

/// Look at every direct child of the function's `parameters` (or
/// `formal_parameters`) node. A typed `simple_parameter` contains both a
/// type child (e.g. `named_type`, `union_type`) and a `variable_name`.
fn scan_formal_parameters(fn_node: Node, bytes: &[u8], var_name: &str) -> Option<String> {
    // tree-sitter-php names this field "parameters" for most function-like
    // nodes. Falling back to a child-kind scan covers the variants where the
    // field name differs.
    let params = fn_node
        .child_by_field_name("parameters")
        .or_else(|| first_child_of_kind(fn_node, "formal_parameters"))?;
    let mut cursor = params.walk();
    for p in params.children(&mut cursor) {
        if let Some(t) = param_type_if_matches(p, bytes, var_name) {
            return Some(t);
        }
    }
    None
}

fn first_child_of_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    let mut found = None;
    for c in node.children(&mut cursor) {
        if c.kind() == kind {
            found = Some(c);
            break;
        }
    }
    found
}

/// If `param` is a typed parameter binding `$var_name`, return its type
/// string. Returns `None` for the no-type case (just `$user` with no
/// annotation) — we can't help an untyped parameter.
fn param_type_if_matches(param: Node, bytes: &[u8], var_name: &str) -> Option<String> {
    let mut cursor = param.walk();
    let mut type_text: Option<String> = None;
    let mut matches_var = false;
    for c in param.children(&mut cursor) {
        match c.kind() {
            // Tree-sitter-php uses several node kinds for type annotations
            // depending on the shape: `named_type` for class refs,
            // `primitive_type` for builtin (int/string/etc.), `union_type`,
            // `intersection_type`, `optional_type` for `?Foo`. We capture
            // the first one we see — `simplify_union` then trims it to a
            // single class name.
            "named_type" | "primitive_type" | "type_list" | "optional_type" | "union_type"
            | "intersection_type" | "qualified_name"
                if type_text.is_none() =>
            {
                type_text = node_text(c, bytes).map(|s| s.trim().to_string());
            }
            "variable_name" => {
                if let Some(text) = node_text(c, bytes) {
                    if text.trim_start_matches('$') == var_name {
                        matches_var = true;
                    }
                }
            }
            _ => {}
        }
    }
    if !matches_var {
        return None;
    }
    type_text.and_then(simplify_type)
}

/// Reduce a type expression to a single class name we can search for.
///
/// - `?User` → `User` (PHP nullable shorthand)
/// - `User|null` → `User` (union with null)
/// - `User|Post` → `User` (pick the first branch; rare in practice)
/// - `int` / `string` etc. → `None` (primitives have no class file)
/// - empty → `None`
fn simplify_type(raw: String) -> Option<String> {
    let cleaned = raw.trim_start_matches('?').trim();
    let first = cleaned.split('|').next()?.trim();
    if first.is_empty() || first.eq_ignore_ascii_case("null") {
        return None;
    }
    // Primitives have no class file — skip them so the caller doesn't try
    // to autoload `int.php`.
    if matches!(
        first.to_ascii_lowercase().as_str(),
        "int"
            | "integer"
            | "float"
            | "double"
            | "string"
            | "bool"
            | "boolean"
            | "array"
            | "object"
            | "mixed"
            | "void"
            | "never"
            | "callable"
            | "iterable"
            | "self"
            | "static"
            | "parent"
    ) {
        return None;
    }
    Some(first.to_string())
}

/// DFS the scope's subtree for `comment` nodes containing a matching
/// `@var ... $var_name` annotation. First match wins; we don't try to
/// pick the "closest" docblock to the cursor — by convention PHP
/// docblocks declare a variable once per scope.
fn scan_comments_for_var(scope: Node, bytes: &[u8], var_name: &str) -> Option<String> {
    let mut stack = vec![scope];
    while let Some(n) = stack.pop() {
        if n.kind() == "comment" {
            if let Some(text) = node_text(n, bytes) {
                if let Some(t) = parse_var_docblock(text, var_name) {
                    return Some(t);
                }
            }
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
    None
}

/// Parse a single comment string for `@var Type $var` (type-first, common)
/// or `@var $var Type` (var-first, also valid PHPDoc). Returns the raw
/// type string — caller does namespace resolution.
fn parse_var_docblock(comment_text: &str, var_name: &str) -> Option<String> {
    use once_cell::sync::Lazy;
    use regex::Regex;

    static RE_TYPE_FIRST: Lazy<Regex> = Lazy::new(|| {
        // `@var <type> $<name>` — type may contain `\\` (FQCN) and `|`
        // (union) but stops at whitespace. The name capture is the
        // variable identifier.
        Regex::new(r"@var\s+([^\s\$][^\s]*)\s+\$([A-Za-z_][A-Za-z0-9_]*)").unwrap()
    });
    static RE_VAR_FIRST: Lazy<Regex> = Lazy::new(|| {
        // `@var $<name> <type>` — less common but valid.
        Regex::new(r"@var\s+\$([A-Za-z_][A-Za-z0-9_]*)\s+([^\s\*\/][^\s\*\/]*)").unwrap()
    });

    for caps in RE_TYPE_FIRST.captures_iter(comment_text) {
        if &caps[2] == var_name {
            return Some(caps[1].to_string());
        }
    }
    for caps in RE_VAR_FIRST.captures_iter(comment_text) {
        if &caps[1] == var_name {
            return Some(caps[2].to_string());
        }
    }
    None
}

fn node_text<'a>(node: Node<'_>, bytes: &'a [u8]) -> Option<&'a str> {
    let start = node.start_byte();
    let end = node.end_byte();
    std::str::from_utf8(bytes.get(start..end)?).ok()
}

#[cfg(test)]
mod tests;
