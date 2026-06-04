//! Shared tree-sitter route-chain walker.
//!
//! Both [`crate::route_name_locator`] (rename / find-references) and
//! [`crate::route_outline`] (document symbols) need to walk the SAME AST shape:
//! traverse statement lists, recognize `Route::*(...)->...` fluent chains,
//! pull the `->name`/`->as`/`prefix`/verb arguments off each chain, and recurse
//! into `->group(closure)` bodies. This module owns that traversal exactly once
//! and hands back a structured intermediate tree — [`RouteChainNode`] — that
//! each consumer folds into its own output type (applying its own prefix
//! accumulation and emitting only the pieces it cares about).
//!
//! ## Why a tree, not a visitor
//!
//! The outline consumer builds a hierarchical result where each group node owns
//! its children, so it needs structured nesting with per-group data. A flat
//! visitor would force the outline builder to reconstruct that hierarchy from a
//! callback stream. Returning a tree keeps both consumers as a simple
//! `nodes.iter().map(fold)` over plain data — no shared mutable accumulator and
//! no lifetime entanglement with the tree-sitter `Tree` (positions are copied
//! out as `u32`, strings as owned `String`).
//!
//! ## Behavior parity
//!
//! The traversal reproduces the previous (independently duplicated) walks:
//! - Only chains whose innermost call is scoped to `Route` (`Route::` or
//!   `…\Route::`) are reported (`is_route_chain`).
//! - The first `->name(...)`/`->as(...)` string argument is captured with its
//!   `string_content` range (between the quotes) — the exact rename target.
//! - `->group(...)` closures are recursed into; their bodies' chains become
//!   `children`.
//! - Statement recursion descends into `namespace_definition` and
//!   `compound_statement` wrappers (group closure bodies are reached only via
//!   the explicit group recursion, never twice).

use tree_sitter::Node;

/// One `Route::...` chain, captured as plain data (no tree-sitter lifetimes).
///
/// A node is emitted for every chain whose innermost call is `Route::*`. The
/// same node can be both a route definition (has `verb`) and a group container
/// (has `group_children`); in practice a chain is one or the other, but callers
/// should not assume mutual exclusivity beyond what their own logic enforces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteChainNode {
    /// The route verb / method that opened the chain (`get`, `post`, `view`,
    /// `resource`, `livewire`, …), lowercased as written in source. `None` when
    /// the chain is a group container or a non-definition chain (e.g. a bare
    /// `Route::prefix(...)->group(...)`).
    pub verb: Option<String>,
    /// First string argument of the verb call (the URI). `None` when absent or
    /// not a string literal.
    pub uri: Option<String>,
    /// `->prefix('...')` argument from anywhere in the chain. `None` if unset.
    pub prefix_arg: Option<String>,
    /// First `->name('...')`/`->as('...')` string argument with its source
    /// position (the `string_content` range, between the quotes).
    pub name: Option<RouteNameArg>,
    /// Range of the `->group(...)` closure expression itself (not its body),
    /// when this chain is a group. Used by the outline to position the group
    /// symbol so it overlaps the editor's tree-sitter "Closure" symbol.
    pub group_closure_range: Option<ChainRange>,
    /// Child chains found inside this chain's `->group(closure)` body, in source
    /// order. Empty for leaf routes and for groups with empty bodies.
    pub group_children: Vec<RouteChainNode>,
    /// Range of the entire chain expression. Used by the outline to position
    /// leaf-route symbols.
    pub chain_range: ChainRange,
}

impl RouteChainNode {
    /// True when this chain is a `->group(...)` container (it had a closure
    /// argument), regardless of whether the body produced any children.
    pub fn is_group(&self) -> bool {
        self.group_closure_range.is_some()
    }
}

/// A `->name('...')`/`->as('...')` argument string captured with the source
/// position of its content (between the quotes). All fields 0-based.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteNameArg {
    /// Literal inner text of the argument (no quotes, no prefix).
    pub segment: String,
    /// Row of the string content.
    pub line: u32,
    /// Column of the first character of the content (after the opening quote).
    pub start_column: u32,
    /// Column one past the last character of the content (before the closing
    /// quote).
    pub end_column: u32,
}

/// A 0-based `(line, column)` span of a chain or closure expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChainRange {
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

/// Parse `content` as PHP and return the top-level `Route::...` chains as a
/// tree. Returns an empty vec on parse failure. This is the single entry point
/// both route consumers use; they fold the resulting tree into their own types.
pub fn extract_route_chains(content: &str) -> Vec<RouteChainNode> {
    let Ok(tree) = crate::parser::parse_php(content) else {
        return Vec::new();
    };
    let source = content.as_bytes();
    let mut output = Vec::new();
    walk_statements(tree.root_node(), source, &mut output);
    output
}

/// Walk a statement list (file root, namespace body, or group closure body)
/// looking for `Route::*(...)` expression statements, appending one
/// [`RouteChainNode`] per route chain to `output`.
fn walk_statements(node: Node, source: &[u8], output: &mut Vec<RouteChainNode>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "expression_statement" => {
                let mut inner = child.walk();
                for expr in child.children(&mut inner) {
                    if is_call_node(expr) {
                        if let Some(chain) = build_chain_node(expr, source) {
                            output.push(chain);
                        }
                    }
                }
            }
            // Recurse into wrappers that can contain expression statements.
            // Group closure bodies are reached only via the explicit group
            // recursion below, never here, so chains aren't double-counted.
            "namespace_definition" | "compound_statement" => {
                walk_statements(child, source, output);
            }
            _ => {}
        }
    }
}

/// Build a [`RouteChainNode`] from a single `Route::method()->method()->...`
/// chain expression. Returns `None` when the chain isn't scoped to `Route`.
fn build_chain_node(chain: Node, source: &[u8]) -> Option<RouteChainNode> {
    let data = collect_chain(chain, source);
    if !data.is_route_chain {
        return None;
    }

    let mut group_children = Vec::new();
    let mut group_closure_range = None;
    if let Some(closure) = data.group_closure {
        group_closure_range = Some(range_of(closure));
        if let Some(body) = closure.child_by_field_name("body") {
            walk_statements(body, source, &mut group_children);
        }
    }

    Some(RouteChainNode {
        verb: data.verb,
        uri: data.uri,
        prefix_arg: data.prefix_arg,
        name: data.name,
        group_closure_range,
        group_children,
        chain_range: range_of(chain),
    })
}

/// Mutable accumulator while walking one chain from outermost call inward.
#[derive(Default)]
struct ChainData<'a> {
    is_route_chain: bool,
    verb: Option<String>,
    uri: Option<String>,
    prefix_arg: Option<String>,
    name: Option<RouteNameArg>,
    group_closure: Option<Node<'a>>,
}

/// Route definition verbs / methods. Anything in this set, when it opens the
/// chain, is treated as a leaf-route definition whose first string argument is
/// the URI. Matches the previous outline allow-list exactly.
const ROUTE_VERBS: &[&str] = &[
    "get",
    "post",
    "put",
    "patch",
    "delete",
    "options",
    "any",
    "match",
    "view",
    "redirect",
    "fallback",
    "livewire",
    "resource",
    "apiResource",
    "singleton",
    "apiSingleton",
    "permanentRedirect",
];

/// Walk a method chain from outermost call inward, collecting each method's
/// name and relevant arguments.
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
            _ => current = None,
        }
    }

    data
}

/// Apply one method call from the chain to the accumulated data.
fn apply_method<'a>(data: &mut ChainData<'a>, name: &str, args: Node<'a>, source: &[u8]) {
    if ROUTE_VERBS.contains(&name) {
        // First verb wins (outermost call inward, the innermost `Route::verb`
        // is visited last; route definitions only have one verb so order is
        // immaterial). The URI is the first string argument.
        data.verb = Some(name.to_string());
        data.uri = first_string_arg(args, source);
        return;
    }
    match name {
        "group" => data.group_closure = find_closure_node(args),
        "prefix" => data.prefix_arg = first_string_arg(args, source),
        // `as()` is Laravel's alias for `name()`. First captured wins.
        "name" | "as" if data.name.is_none() => {
            data.name = first_string_arg_with_pos(args, source);
        }
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

/// Extract the first string-literal argument's inner text (no position).
fn first_string_arg(args: Node, source: &[u8]) -> Option<String> {
    let mut cursor = args.walk();
    for arg in args.children(&mut cursor) {
        if arg.kind() != "argument" {
            continue;
        }
        let mut arg_cursor = arg.walk();
        for inner in arg.children(&mut arg_cursor) {
            if inner.kind() == "string" || inner.kind() == "encapsed_string" {
                return read_string_content(inner, source);
            }
        }
    }
    None
}

/// Extract the first string-literal argument with its `string_content` range.
fn first_string_arg_with_pos(args: Node, source: &[u8]) -> Option<RouteNameArg> {
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

/// Read the inner content of a `string`/`encapsed_string` node, stripping the
/// surrounding quotes. Falls back to trimming the raw text when no
/// `string_content` child is present.
fn read_string_content(node: Node, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "string_content" {
            return child.utf8_text(source).ok().map(|s| s.to_string());
        }
    }
    node.utf8_text(source).ok().map(|s| {
        s.trim_start_matches(['\'', '"'])
            .trim_end_matches(['\'', '"'])
            .to_string()
    })
}

/// Locate the `string_content` child node of a `string`/`encapsed_string` and
/// return its inner text plus 0-based content range (between the quotes).
fn string_content_with_pos(node: Node, source: &[u8]) -> Option<RouteNameArg> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "string_content" {
            let segment = child.utf8_text(source).ok()?.to_string();
            let start = child.start_position();
            let end = child.end_position();
            // `string_content` always sits on a single line in tree-sitter-php.
            // Defensive: clamp the end column to the start line so a misparse
            // doesn't produce a multi-line range.
            return Some(RouteNameArg {
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

/// Find the closure expression node inside an arguments list. Handles the
/// modern `Route::group(function () { ... })`, the legacy
/// `Route::group(['prefix' => '/x'], function () { ... })`, and arrow-function
/// forms. Returns the closure expression itself (not its body).
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

fn range_of(node: Node) -> ChainRange {
    let start = node.start_position();
    let end = node.end_position();
    ChainRange {
        start_line: start.row as u32,
        start_column: start.column as u32,
        end_line: end.row as u32,
        end_column: end.column as u32,
    }
}
