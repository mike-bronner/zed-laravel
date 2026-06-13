//! Function-local, scope-aware rename for plain PHP variables (`$user`).
//!
//! The Laravel rename engine in [`crate::rename`] handles string-keyed and
//! class-backed Laravel symbols (routes, config keys, views, components, …).
//! It deliberately never touches plain PHP local variables — that needs real
//! lexical-scope analysis, not pattern classification. This module is that
//! analysis.
//!
//! ## What it does
//!
//! Renaming `$user` rewrites *every* occurrence of `$user` that resolves to the
//! same binding — and nothing else. The unit of a rename is one **binding
//! scope**: the nearest enclosing function-like node that owns the variable.
//!
//! ## Scope model
//!
//! Every variable occurrence resolves to a binding scope by walking up through
//! its function-like ancestors:
//!
//! - **`function_definition` / `method_declaration`** — a hard boundary. Params
//!   and locals live here; an identically-named variable in a sibling function
//!   is a different binding.
//! - **`anonymous_function` (`function () { … }`)** — also a hard boundary. A
//!   closure does *not* auto-capture: a `$user` used inside is a fresh local
//!   unless the closure declares `use ($user)`. When it captures via `use`, the
//!   captured variable is bound to the *outer* `$user`, so a rename cascades
//!   across the `use` clause and the closure body in lockstep.
//! - **`arrow_function` (`fn () => …`)** — transparent for captures. Arrow
//!   functions auto-capture outer variables by value, so `fn () => $user`
//!   shares `$user`'s binding with the enclosing scope. Its *own parameters*
//!   still shadow: `fn ($user) => $user` is a separate binding.
//! - **`program`** — the top-level (global) script scope.
//!
//! The same resolver runs for the cursor variable and for every candidate
//! occurrence; an occurrence is part of the rename iff it resolves to the
//! *same* binding scope (compared by node id). That single rule yields nested
//! closure isolation, arrow-capture inclusion, and correct shadowing without
//! any special cases.
//!
//! ## Explicitly excluded
//!
//! - **`$this`** — never renameable.
//! - **Static properties** (`self::$bar`, `Foo::$bar`) — the `$bar` token is a
//!   class property, not a local variable, even though tree-sitter spells it as
//!   a `variable_name`. Object properties (`$this->foo`, `$obj->prop`) are
//!   `name` nodes, not `variable_name`, so they fall out naturally.
//!
//! Constructor-promoted parameters (`__construct(private $name)`) are treated
//! as ordinary locals within the constructor — the property side of promotion
//! belongs to the deferred class-property rename pass, not here.

use std::path::Path;
use tree_sitter::Node;

use crate::rename::EditTarget;

/// Function-like node kinds that bound a lexical scope. `program` is the
/// top-level script scope and always terminates the upward walk.
fn is_function_like(node: Node) -> bool {
    matches!(
        node.kind(),
        "function_definition"
            | "method_declaration"
            | "anonymous_function"
            | "anonymous_function_creation_expression"
            | "arrow_function"
            | "program"
    )
}

/// The text of a `variable_name` node with the leading `$` stripped, i.e. the
/// bare identifier (`$user` → `user`). `None` if the bytes aren't valid UTF-8.
fn var_ident<'a>(node: Node<'_>, bytes: &'a [u8]) -> Option<&'a str> {
    let text = std::str::from_utf8(bytes.get(node.start_byte()..node.end_byte())?).ok()?;
    Some(text.trim_start_matches('$'))
}

/// The nearest function-like ancestor *above* `node` (never `node` itself).
/// Always returns `Some` for a node in a parsed tree — `program` is the
/// backstop at the root.
fn enclosing_function_like(node: Node) -> Option<Node> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if is_function_like(n) {
            return Some(n);
        }
        cur = n.parent();
    }
    None
}

/// Whether `scope` declares a parameter named `name` (without `$`). Scans the
/// scope's direct `formal_parameters` child — covers normal, variadic, and
/// constructor-promoted parameters.
fn scope_declares_param(scope: Node, name: &str, bytes: &[u8]) -> bool {
    let mut c = scope.walk();
    for child in scope.children(&mut c) {
        if child.kind() != "formal_parameters" {
            continue;
        }
        let mut pc = child.walk();
        for param in child.children(&mut pc) {
            if matches!(
                param.kind(),
                "simple_parameter" | "variadic_parameter" | "property_promotion_parameter"
            ) {
                if let Some(name_node) = param.child_by_field_name("name") {
                    if var_ident(name_node, bytes) == Some(name) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// The `variable_name` captured by one child of an
/// `anonymous_function_use_clause`, unwrapping the `by_ref` wrapper that the
/// reference form `use (&$x)` introduces. Returns `None` for children that
/// aren't a simple variable capture (e.g. punctuation, or a `by_ref` over a
/// non-variable expression). In tree-sitter-php the value form `use ($x)`
/// places the `variable_name` as a direct child, while the reference form
/// `use (&$x)` wraps it in a `by_ref` node — both must be recognised so that a
/// by-reference capture isn't mistaken for a fresh closure-local.
fn use_clause_capture<'t>(child: Node<'t>) -> Option<Node<'t>> {
    match child.kind() {
        "variable_name" => Some(child),
        // `by_ref` wraps exactly one expression; for `use (&$x)` that's the
        // `variable_name` (the `&` is an anonymous token). Other by-ref targets
        // aren't simple variable captures, so filter to `variable_name`.
        "by_ref" => child.named_child(0).filter(|n| n.kind() == "variable_name"),
        _ => None,
    }
}

/// Whether `scope` (an anonymous function) captures `name` via a `use (…)`
/// clause — both the value form `use ($x)` and the reference form `use (&$x)`.
fn scope_captures_use(scope: Node, name: &str, bytes: &[u8]) -> bool {
    let mut c = scope.walk();
    for child in scope.children(&mut c) {
        if child.kind() == "anonymous_function_use_clause" {
            let mut inner = child.walk();
            for v in child.children(&mut inner) {
                if let Some(var) = use_clause_capture(v) {
                    if var_ident(var, bytes) == Some(name) {
                        return true;
                    }
                }
            }
            return false;
        }
    }
    false
}

/// Resolve the binding scope that owns the variable `name` at `var_node`.
///
/// Walks up through function-like ancestors applying the scope model
/// documented at the module level: hard boundaries (function/method/closure)
/// stop the walk; arrow functions and `use`-captured closures are transparent
/// (the binding lives in the enclosing scope).
fn resolve_binding_scope<'t>(var_node: Node<'t>, name: &str, bytes: &[u8]) -> Node<'t> {
    let mut probe = var_node;
    loop {
        let Some(scope) = enclosing_function_like(probe) else {
            // No function-like ancestor at all — the node is at the root.
            return probe;
        };
        match scope.kind() {
            "arrow_function" => {
                if scope_declares_param(scope, name, bytes) {
                    return scope;
                }
                // Auto-capture by value: look in the enclosing scope.
                probe = scope;
            }
            "anonymous_function" | "anonymous_function_creation_expression" => {
                if scope_declares_param(scope, name, bytes) {
                    return scope;
                }
                if scope_captures_use(scope, name, bytes) {
                    // Bound to the outer variable via `use (…)`.
                    probe = scope;
                } else {
                    // Fresh local inside the closure (closures don't auto-capture).
                    return scope;
                }
            }
            // `function_definition`, `method_declaration`, `program`: hard scope.
            _ => return scope,
        }
    }
}

/// Whether a matching `variable_name` node should participate in the rename.
/// Excludes `$this` and static-property positions (`self::$bar`), which are
/// spelled as `variable_name` but are not local variables.
fn is_collectible(node: Node, bytes: &[u8]) -> bool {
    if var_ident(node, bytes) == Some("this") {
        return false;
    }
    if let Some(parent) = node.parent() {
        if parent.kind() == "scoped_property_access_expression"
            && parent.child_by_field_name("name").map(|n| n.id()) == Some(node.id())
        {
            // `self::$bar` / `Foo::$bar` — a class property, not a local.
            return false;
        }
    }
    true
}

/// Locate the renameable local-variable `variable_name` node at `cursor_byte`.
/// Handles the cursor landing on the inner identifier or the `$` token (both
/// children of `variable_name`). Returns `None` when the cursor isn't on a
/// renameable local variable (e.g. `$this`, a static property, or a non-variable).
fn renameable_variable_at<'t>(
    root: Node<'t>,
    bytes: &[u8],
    cursor_byte: usize,
) -> Option<Node<'t>> {
    let hit = root.descendant_for_byte_range(cursor_byte, cursor_byte)?;
    let var_node = if hit.kind() == "variable_name" {
        hit
    } else if hit.parent().map(|p| p.kind()) == Some("variable_name") {
        hit.parent()?
    } else {
        return None;
    };
    if !is_collectible(var_node, bytes) {
        return None;
    }
    Some(var_node)
}

/// Collect every `variable_name` node in `scope`'s subtree that names `name`
/// and resolves to `scope` as its binding scope. Descends into nested scopes
/// on purpose — arrow-function captures and `use`-clause variables live there,
/// and the per-node binding resolution filters out anything that belongs to a
/// different scope.
fn collect_occurrences<'t>(scope: Node<'t>, name: &str, bytes: &[u8]) -> Vec<Node<'t>> {
    let mut out = Vec::new();
    let mut stack = vec![scope];
    while let Some(n) = stack.pop() {
        if n.kind() == "variable_name"
            && var_ident(n, bytes) == Some(name)
            && is_collectible(n, bytes)
            && resolve_binding_scope(n, name, bytes).id() == scope.id()
        {
            out.push(n);
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    out
}

/// Validate and normalize a user-supplied new variable name to the bare PHP
/// identifier (leading `$` optional and stripped). `None` if it isn't a legal
/// PHP variable name.
fn normalize_new_var_name(new_name: &str) -> Option<String> {
    let ident = new_name.trim().trim_start_matches('$');
    let mut chars = ident.chars();
    let first = chars.next()?;
    // PHP identifiers: first char a letter, `_`, or any byte >= 0x80.
    if !(first.is_ascii_alphabetic() || first == '_' || (first as u32) >= 0x80) {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || (c as u32) >= 0x80) {
        return None;
    }
    Some(ident.to_string())
}

/// The 0-based `(line, start_column, end_column)` of the renameable local
/// variable under `cursor_byte`, or `None` if the cursor isn't on one. Backs
/// `textDocument/prepareRename` — columns span the whole `$name` token.
pub fn variable_at_cursor(source: &str, cursor_byte: usize) -> Option<(u32, u32, u32)> {
    let tree = crate::parser::parse_php(source).ok()?;
    let bytes = source.as_bytes();
    let var_node = renameable_variable_at(tree.root_node(), bytes, cursor_byte)?;
    let start = var_node.start_position();
    let end = var_node.end_position();
    Some((start.row as u32, start.column as u32, end.column as u32))
}

/// Build the [`EditTarget`]s renaming the local variable under `cursor_byte`
/// to `new_name`, across its whole binding scope.
///
/// Returns `Err(message)` when the cursor *is* on a renameable variable but
/// `new_name` isn't a legal PHP identifier — the message surfaces to the user.
/// Returns `Ok(vec![])` (a no-op) when the cursor isn't on a local variable or
/// the new name equals the old; callers treat an empty result as "no edit".
pub fn variable_rename_targets(
    source: &str,
    file_path: &Path,
    cursor_byte: usize,
    new_name: &str,
) -> Result<Vec<EditTarget>, String> {
    let tree = crate::parser::parse_php(source).map_err(|e| format!("could not parse PHP: {e}"))?;
    let bytes = source.as_bytes();

    let Some(var_node) = renameable_variable_at(tree.root_node(), bytes, cursor_byte) else {
        return Ok(Vec::new());
    };
    let Some(old_ident) = var_ident(var_node, bytes) else {
        return Ok(Vec::new());
    };

    let new_ident = normalize_new_var_name(new_name).ok_or_else(|| {
        "Rename a variable to a valid PHP name (letters, digits or '_', not starting with a digit)."
            .to_string()
    })?;
    if new_ident == old_ident {
        return Ok(Vec::new());
    }

    let scope = resolve_binding_scope(var_node, old_ident, bytes);
    let new_text = format!("${new_ident}");
    let targets = collect_occurrences(scope, old_ident, bytes)
        .into_iter()
        .map(|n| {
            let start = n.start_position();
            let end = n.end_position();
            EditTarget {
                file_path: file_path.to_path_buf(),
                line: start.row as u32,
                start_column: start.column as u32,
                end_column: end.column as u32,
                new_text: new_text.clone(),
            }
        })
        .collect();
    Ok(targets)
}

#[cfg(test)]
mod tests;
