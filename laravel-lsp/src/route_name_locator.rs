//! Locate `->name('xxx')` (and the `->as('xxx')` alias) declaration sites
//! inside a Laravel route file via tree-sitter — column-accurate positions
//! suitable for building a rename `WorkspaceEdit`.
//!
//! The companion to `route_outline.rs`: that module extracts the *name string*
//! to label outline entries, this one extracts the *position of the name
//! string's content* so we can rewrite it in place. Same AST walk approach;
//! the divergence is that we need column ranges rather than concatenated
//! strings.
//!
//! Group prefixes are followed honestly: a route inside `Route::name('api.')`
//! whose own `->name('users')` resolves to declared name `api.users`, but the
//! *physical source position to rewrite* is just the `users` portion. Renaming
//! `api.users` to `api.accounts` therefore mutates only `users` → `accounts`
//! in source — the prefix lives in a separate declaration that the user can
//! rename independently if desired.

use tree_sitter::Node;

/// One declaration site that contributes to a full route name. The
/// `name_arg_line/start_column/end_column` identifies the physical source
/// position of the string literal content (between the quotes) — the precise
/// range a rename `TextEdit` should replace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteNameDeclaration {
    /// Combined full route name including any inherited group `name(...)`
    /// prefixes. This is what the user sees when they search for a route.
    pub full_name: String,
    /// The literal source-text content of this declaration's own
    /// `->name(...)` / `->as(...)` argument (no quotes, no prefix).
    pub local_segment: String,
    /// Row of the string content. 0-based.
    pub line: u32,
    /// Column of the first character of the string content (after the opening
    /// quote). 0-based.
    pub start_column: u32,
    /// Column one past the last character of the string content (before the
    /// closing quote). 0-based.
    pub end_column: u32,
}

/// Walk a route file and return every `->name(...)` / `->as(...)` declaration
/// in source order, regardless of whether they sit on leaf routes or on
/// `Route::group(...)` containers.
pub fn extract_route_name_declarations(content: &str) -> Vec<RouteNameDeclaration> {
    let Ok(tree) = crate::parser::parse_php(content) else {
        return Vec::new();
    };
    let source = content.as_bytes();
    let ctx = NameContext::default();
    let mut output = Vec::new();
    walk_statements(tree.root_node(), source, &ctx, &mut output);
    output
}

/// All declarations whose `full_name` equals `target`. Useful for rename:
/// returns every place that contributes to the same fully-qualified route
/// name (a route inside multiple nested `name('a.')` `name('b.')` groups has
/// one leaf and two group declarations).
pub fn find_declarations_named(content: &str, target: &str) -> Vec<RouteNameDeclaration> {
    extract_route_name_declarations(content)
        .into_iter()
        .filter(|d| d.full_name == target)
        .collect()
}

/// Inherited context from enclosing `Route::group(...)` blocks — only the
/// name prefix matters for our purpose (URI prefixes are irrelevant to route
/// *name* rename).
#[derive(Debug, Clone, Default)]
struct NameContext {
    name_prefix: String,
}

fn walk_statements(
    node: Node,
    source: &[u8],
    ctx: &NameContext,
    output: &mut Vec<RouteNameDeclaration>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "expression_statement" => {
                let mut inner = child.walk();
                for expr in child.children(&mut inner) {
                    if is_call_node(expr) {
                        process_chain(expr, source, ctx, output);
                    }
                }
            }
            _ => {
                walk_statements(child, source, ctx, output);
            }
        }
    }
}

fn process_chain(
    chain: Node,
    source: &[u8],
    ctx: &NameContext,
    output: &mut Vec<RouteNameDeclaration>,
) {
    let data = collect_chain(chain, source);
    if !data.is_route_chain {
        return;
    }

    // Borrow the name string by reference so we can both emit a declaration
    // for it AND use its segment when computing the recursive group context.
    if let Some(name_pos) = data.name_string.as_ref() {
        let full_name = format!("{}{}", ctx.name_prefix, name_pos.segment);
        output.push(RouteNameDeclaration {
            full_name,
            local_segment: name_pos.segment.clone(),
            line: name_pos.line,
            start_column: name_pos.start_column,
            end_column: name_pos.end_column,
        });
    }

    // Recurse into group closures, applying this group's name prefix.
    if let Some(closure_node) = data.group_closure {
        let extra_prefix = data
            .name_string
            .as_ref()
            .map(|n| n.segment.as_str())
            .unwrap_or("");
        let new_ctx = NameContext {
            name_prefix: format!("{}{}", ctx.name_prefix, extra_prefix),
        };
        if let Some(body) = closure_node.child_by_field_name("body") {
            walk_statements(body, source, &new_ctx, output);
        }
    }
}

/// Position-bearing record of a `->name(...)` / `->as(...)` argument string.
#[derive(Debug, Clone)]
struct NameString {
    segment: String,
    line: u32,
    start_column: u32,
    end_column: u32,
}

#[derive(Debug, Default)]
struct ChainData<'a> {
    is_route_chain: bool,
    /// The `->name('xxx')` / `->as('xxx')` argument captured with position.
    name_string: Option<NameString>,
    /// Closure node for `->group(...)`, so we can recurse for nested decls.
    group_closure: Option<Node<'a>>,
}

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
            _ => current = None,
        }
    }
    data
}

fn apply_method<'a>(data: &mut ChainData<'a>, name: &str, args: Node<'a>, source: &[u8]) {
    match name {
        "name" | "as" if data.name_string.is_none() => {
            data.name_string = first_string_arg_with_pos(args, source);
        }
        "group" => {
            data.group_closure = find_closure_node(args);
        }
        _ => {}
    }
}

fn method_name_and_args<'a>(node: Node<'a>, source: &[u8]) -> Option<(String, Node<'a>)> {
    let name = node
        .child_by_field_name("name")?
        .utf8_text(source)
        .ok()?
        .to_string();
    let args = node.child_by_field_name("arguments")?;
    Some((name, args))
}

fn first_string_arg_with_pos(args: Node, source: &[u8]) -> Option<NameString> {
    let mut cursor = args.walk();
    for arg in args.children(&mut cursor) {
        if arg.kind() != "argument" {
            continue;
        }
        let mut arg_cursor = arg.walk();
        for inner in arg.children(&mut arg_cursor) {
            if inner.kind() == "string" || inner.kind() == "encapsed_string" {
                return string_content_with_pos(inner, source);
            }
        }
    }
    None
}

/// Locate the `string_content` child node of a `string` / `encapsed_string`
/// and return its range. The content range excludes the surrounding quotes,
/// which is exactly what a rename `TextEdit` should target.
fn string_content_with_pos(node: Node, source: &[u8]) -> Option<NameString> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "string_content" {
            let segment = child.utf8_text(source).ok()?.to_string();
            let start = child.start_position();
            let end = child.end_position();
            // string_content always sits on a single line in tree-sitter-php
            // (multiline strings use distinct nodes). Defensive: clamp the
            // end column to the start line so a misparse doesn't produce
            // a multi-line range.
            return Some(NameString {
                segment,
                line: start.row as u32,
                start_column: start.column as u32,
                end_column: if end.row == start.row {
                    end.column as u32
                } else {
                    start.column as u32
                },
            });
        }
    }
    None
}

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

#[cfg(test)]
mod tests;
