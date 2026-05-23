//! Tree-sitter walker for Laravel route files.
//!
//! Produces a hierarchical outline of routes that preserves group nesting,
//! computes combined URIs (a route inside `prefix('/api')` shows `/api/users`,
//! not `/users`), and combines route name prefixes (a route inside
//! `name('api.')` shows `api.users`, not `users`).
//!
//! Unlike a regex-based extractor, this walks the PHP AST and matches every
//! `Route::*` method by structure — so `Route::livewire`, `Route::resource`,
//! `Route::apiResource`, and any future custom route method surface
//! automatically without us maintaining a verb allow-list.
//!
//! The output is plain data (`RouteOutline`) so it can be memoized through
//! Salsa and converted to `SymbolEntry` by `document_symbols.rs`.

use tree_sitter::Node;

/// One entry in the route outline tree. Leaf entries are route definitions
/// (HTTP verbs, `livewire`, `resource`, etc.); container entries are
/// `Route::group(...)` blocks with their nested routes as children.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RouteOutline {
    /// HTTP verb or Laravel route method, uppercased: GET, POST, RESOURCE,
    /// LIVEWIRE, REDIRECT, VIEW, etc. "GROUP" for container entries.
    pub method: String,
    /// Full URI including any inherited group prefixes. For groups, the
    /// group's accumulated prefix (e.g. "/api/admin" for a nested group).
    pub uri: String,
    /// Full route name including any inherited group name prefixes, if the
    /// route is named. For groups, the accumulated name prefix if any.
    pub name: Option<String>,
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
    /// For group containers, the nested routes/groups in source order.
    /// For leaf routes, always empty.
    pub children: Vec<RouteOutline>,
    /// True for `Route::group(...)` containers.
    pub is_group: bool,
}

/// Parse a route file's content and return the outline tree. Returns an
/// empty vec on parse failure.
pub fn extract_route_outline(content: &str) -> Vec<RouteOutline> {
    let Ok(tree) = crate::parser::parse_php(content) else {
        return Vec::new();
    };
    let source = content.as_bytes();
    let ctx = GroupContext::default();
    let mut output = Vec::new();
    walk_statements(tree.root_node(), source, &ctx, &mut output);
    output
}

/// Inherited context from enclosing `Route::group(...)` blocks. Accumulated
/// as we descend; the empty default is the file's top level.
#[derive(Debug, Clone, Default)]
struct GroupContext {
    /// URI prefix (combined `prefix('...')` from all enclosing groups).
    prefix: String,
    /// Route name prefix (combined `name('...')` from all enclosing groups).
    name_prefix: String,
}

/// Walk a statement list (file root, namespace body, or group closure body)
/// looking for `Route::*(...)` expression statements.
fn walk_statements(node: Node, source: &[u8], ctx: &GroupContext, output: &mut Vec<RouteOutline>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "expression_statement" => {
                // The expression statement wraps a single expression — find it.
                let mut inner = child.walk();
                for expr in child.children(&mut inner) {
                    if is_call_node(expr) {
                        process_chain(expr, source, ctx, output);
                    }
                }
            }
            // Recurse into wrappers that can contain expression statements
            // (namespaces, compound statements). Closures are handled
            // separately via `walk_statements` from inside group processing.
            "namespace_definition" | "compound_statement" => {
                walk_statements(child, source, ctx, output);
            }
            _ => {}
        }
    }
}

/// Process a single `Route::method()->method()->...` chain. Decides whether
/// it's a leaf route definition or a group container, and emits accordingly.
fn process_chain(chain: Node, source: &[u8], ctx: &GroupContext, output: &mut Vec<RouteOutline>) {
    let data = collect_chain(chain, source);

    // Bail if the chain doesn't start with `Route::` — we don't want to
    // emit entries for unrelated method calls that happen to live in a
    // route file.
    if !data.is_route_chain {
        return;
    }

    if let Some(method) = data.route_method {
        // Leaf route definition.
        let route_uri = data.route_uri.unwrap_or_default();
        let combined_uri = format!("{}{}", ctx.prefix, route_uri);
        // For leaf routes, `name_arg` is the route's own `->name('foo')` call.
        let combined_name = data.name_arg.map(|n| format!("{}{}", ctx.name_prefix, n));

        let (start_line, start_column) = pos(chain.start_position());
        let (end_line, end_column) = pos(chain.end_position());

        output.push(RouteOutline {
            method: method.to_uppercase(),
            uri: combined_uri,
            name: combined_name,
            start_line,
            start_column,
            end_line,
            end_column,
            children: Vec::new(),
            is_group: false,
        });
    } else if let Some(closure_node) = data.group_closure {
        // Group container. Apply this group's modifiers to the inherited
        // context, then recurse into the closure body with the new context.
        let prefix_arg = data.prefix_arg.unwrap_or_default();
        let name_arg = data.name_arg.unwrap_or_default();
        let new_ctx = GroupContext {
            prefix: format!("{}{}", ctx.prefix, prefix_arg),
            name_prefix: format!("{}{}", ctx.name_prefix, name_arg),
        };

        let mut children = Vec::new();
        if let Some(body) = closure_node.child_by_field_name("body") {
            walk_statements(body, source, &new_ctx, &mut children);
        }

        // Position the group entry at the closure expression itself (not
        // the wider chain) so it overlaps Zed's tree-sitter "Closure" symbol
        // at the exact same range — gives the outline UI a chance to dedupe.
        let (start_line, start_column) = pos(closure_node.start_position());
        let (end_line, end_column) = pos(closure_node.end_position());

        // Skip empty groups — they're noise.
        if children.is_empty() {
            return;
        }

        output.push(RouteOutline {
            method: "GROUP".to_string(),
            uri: new_ctx.prefix,
            name: if new_ctx.name_prefix.is_empty() {
                None
            } else {
                Some(new_ctx.name_prefix)
            },
            start_line,
            start_column,
            end_line,
            end_column,
            children,
            is_group: true,
        });
    }
    // Other chains (e.g. `Route::pattern(...)` config calls) are ignored.
}

/// Data collected from one chain. All fields are populated by walking the
/// chain from outermost call inward.
#[derive(Default)]
struct ChainData<'a> {
    /// True if the chain's innermost call is `Route::*` (scoped to `Route`).
    is_route_chain: bool,
    /// Set when we encounter a route definition method like `get`/`post`.
    route_method: Option<String>,
    /// The first string argument to the route method (the URI pattern).
    route_uri: Option<String>,
    /// Combined `->name('...')` argument from anywhere in the chain. For
    /// leaf routes this is the route name; for groups it's the name prefix.
    name_arg: Option<String>,
    /// `->prefix('...')` argument (only meaningful for groups).
    prefix_arg: Option<String>,
    /// The `anonymous_function_creation_expression` node passed to
    /// `->group(...)`, if the chain is a group. Tracked instead of just the
    /// body so the consumer can position the group symbol at the closure's
    /// own range — this lets the outline merge cleanly with Zed's tree-sitter
    /// "Closure" symbol that would otherwise appear alongside ours.
    group_closure: Option<Node<'a>>,
}

/// Walk a method chain from outermost call inward, collecting each method's
/// name and relevant arguments into `ChainData`.
fn collect_chain<'a>(chain: Node<'a>, source: &[u8]) -> ChainData<'a> {
    let mut data = ChainData::default();
    let mut current = Some(chain);

    while let Some(node) = current {
        match node.kind() {
            "member_call_expression" => {
                if let Some((name, args)) = method_name_and_args(node, source) {
                    apply_method(&mut data, &name, args, source);
                }
                current = node.child_by_field_name("object");
            }
            "scoped_call_expression" => {
                // Innermost call (`Route::method(...)`). Verify the scope is
                // `Route` so we don't claim chains like `Foo::bar()->baz()`.
                if let Some(scope) = node.child_by_field_name("scope") {
                    if let Ok(scope_text) = scope.utf8_text(source) {
                        if scope_text == "Route" || scope_text.ends_with("\\Route") {
                            data.is_route_chain = true;
                        }
                    }
                }
                if let Some((name, args)) = method_name_and_args(node, source) {
                    apply_method(&mut data, &name, args, source);
                }
                current = None;
            }
            _ => {
                current = None;
            }
        }
    }

    data
}

/// Apply one method call from the chain to the accumulated data.
fn apply_method<'a>(data: &mut ChainData<'a>, name: &str, args: Node<'a>, source: &[u8]) {
    match name {
        // Route definition methods. The first string argument is the URI.
        // We don't allow-list these specifically — anything that's not a
        // known modifier or `group` is treated as a definition. But to keep
        // false positives low, we still match against the common methods.
        "get" | "post" | "put" | "patch" | "delete" | "options" | "any" | "match" | "view"
        | "redirect" | "fallback" | "livewire" | "resource" | "apiResource" | "singleton"
        | "apiSingleton" | "permanentRedirect" => {
            data.route_method = Some(name.to_string());
            data.route_uri = first_string_arg(args, source);
        }

        // Group container. The closure argument is the group body.
        "group" => {
            data.group_closure = find_closure_node(args);
        }

        // Modifiers — meaning depends on whether we end up as route or group.
        "prefix" => {
            data.prefix_arg = first_string_arg(args, source);
        }
        // `as()` is an alias for `name()` in Laravel.
        "name" | "as" => {
            data.name_arg = first_string_arg(args, source);
        }

        // Modifiers we don't currently visualize: middleware, withoutMiddleware,
        // domain, where, whereNumber, whereAlpha, controller, etc.
        _ => {}
    }
}

/// Get the method `name:` and `arguments:` fields from a call node.
fn method_name_and_args<'a>(node: Node<'a>, source: &[u8]) -> Option<(String, Node<'a>)> {
    let name = node
        .child_by_field_name("name")?
        .utf8_text(source)
        .ok()?
        .to_string();
    let args = node.child_by_field_name("arguments")?;
    Some((name, args))
}

/// Extract the first string literal from an arguments list. Handles both
/// `'single-quoted'` and `"double-quoted"` forms (tree-sitter-php names them
/// `string` and `encapsed_string` respectively).
fn first_string_arg(args: Node, source: &[u8]) -> Option<String> {
    let mut cursor = args.walk();
    for arg in args.children(&mut cursor) {
        if arg.kind() != "argument" {
            continue;
        }
        let mut arg_cursor = arg.walk();
        for inner in arg.children(&mut arg_cursor) {
            match inner.kind() {
                "string" | "encapsed_string" => {
                    return read_string_content(inner, source);
                }
                _ => {}
            }
        }
    }
    None
}

/// Read the inner content of a `string` / `encapsed_string` node, stripping
/// the surrounding quotes. tree-sitter exposes a `string_content` child for
/// the body; if it's missing (rare), fall back to trimming the raw text.
fn read_string_content(node: Node, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "string_content" {
            return child.utf8_text(source).ok().map(|s| s.to_string());
        }
    }
    // Fallback: strip the leading/trailing quote chars from the raw node.
    node.utf8_text(source).ok().map(|s| {
        s.trim_start_matches(['\'', '"'])
            .trim_end_matches(['\'', '"'])
            .to_string()
    })
}

/// Find the closure expression node inside an arguments list. Handles both
/// the modern `Route::group(function () { ... })` form and the legacy
/// `Route::group(['prefix' => '/x'], function () { ... })` form by scanning
/// every argument for an anonymous-function node. Returns the closure
/// expression itself (not its body) so callers can use its range.
fn find_closure_node(args: Node) -> Option<Node> {
    let mut cursor = args.walk();
    for arg in args.children(&mut cursor) {
        if arg.kind() != "argument" {
            continue;
        }
        let mut arg_cursor = arg.walk();
        for inner in arg.children(&mut arg_cursor) {
            if inner.kind() == "anonymous_function_creation_expression"
                || inner.kind() == "anonymous_function"
                || inner.kind() == "arrow_function"
            {
                return Some(inner);
            }
        }
    }
    None
}

fn is_call_node(node: Node) -> bool {
    matches!(
        node.kind(),
        "member_call_expression" | "scoped_call_expression"
    )
}

fn pos(point: tree_sitter::Point) -> (u32, u32) {
    (point.row as u32, point.column as u32)
}

#[cfg(test)]
mod tests;
