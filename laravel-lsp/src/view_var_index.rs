//! Controller → Blade view-variable type inference.
//!
//! Blade member access (`{{ $user->email }}`) can't be resolved from the
//! `.blade.php` alone — `$user` is a view variable passed in by whatever
//! renders the view. This module infers those variable types by finding the
//! render sites (`view('users.index', ['user' => $u])`, `compact('user')`,
//! `view(...)->with('user', $u)`) and resolving each passed expression's type
//! in the *controller's* scope via the magic-member resolver.
//!
//! Phase 2–3 of the Blade view-variable inference: this module produces the
//! per-file [`ViewRender`]s; the project-wide reverse index (view → vars) and
//! the Blade resolution that consumes it are wired on top.

use std::collections::HashMap;
use std::path::Path;

use tree_sitter::Node;

use crate::member_resolver::{resolve_expression_type, ClassFileResolver, ClassViewCache};
use crate::parser::parse_php;
use crate::query_chain::flow;
use crate::query_chain::use_aliases::extract_use_aliases;

/// One `view('name', …)` render site: the rendered view and the variable →
/// FQCN types it passes in (only the variables whose type resolved).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewRender {
    pub view_name: String,
    pub vars: HashMap<String, String>,
}

/// Extract every `view('name', data)` render site in `source`, resolving each
/// passed variable's type in the file's scope. Handles the data forms:
/// `['user' => $expr]`, `compact('user', …)`, and `view(...)->with('user', $expr)`.
pub fn view_renders_in_file(
    source: &str,
    resolver: &impl ClassFileResolver,
    classviews: &mut ClassViewCache,
    project_root: &Path,
) -> Vec<ViewRender> {
    let Ok(tree) = parse_php(source) else {
        return Vec::new();
    };
    let bytes = source.as_bytes();
    let aliases = extract_use_aliases(&tree, source);

    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        // `view('name', <data>)`
        if n.kind() == "function_call_expression" && call_function_name(n, bytes) == Some("view") {
            if let Some(render) =
                render_from_view_call(n, bytes, &aliases, resolver, classviews, project_root)
            {
                out.push(render);
            }
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    out
}

/// Build a [`ViewRender`] from a `view('name', data)` call, also folding in any
/// chained `->with(...)` on the same expression.
fn render_from_view_call(
    call: Node,
    bytes: &[u8],
    aliases: &crate::query_chain::use_aliases::UseAliases,
    resolver: &impl ClassFileResolver,
    classviews: &mut ClassViewCache,
    project_root: &Path,
) -> Option<ViewRender> {
    let args = call.child_by_field_name("arguments")?;
    let arg_exprs = positional_args(args);
    let view_name = string_literal_value(*arg_exprs.first()?, bytes)?;

    let mut vars = HashMap::new();
    if let Some(data) = arg_exprs.get(1) {
        collect_vars(
            *data,
            bytes,
            aliases,
            resolver,
            classviews,
            project_root,
            &mut vars,
        );
    }

    // Fold chained `->with('k', $v)` / `->with(['k' => $v])` calls. The `call`
    // is the `view(...)` node; its parents may be member calls building a chain.
    collect_with_chain(
        call,
        bytes,
        aliases,
        resolver,
        classviews,
        project_root,
        &mut vars,
    );

    Some(ViewRender { view_name, vars })
}

/// Resolve the variable types in a `view()` data argument — an array literal
/// (`['user' => $u]`) or a `compact('user', …)` call — into `vars`.
#[allow(clippy::too_many_arguments)]
fn collect_vars(
    data: Node,
    bytes: &[u8],
    aliases: &crate::query_chain::use_aliases::UseAliases,
    resolver: &impl ClassFileResolver,
    classviews: &mut ClassViewCache,
    project_root: &Path,
    vars: &mut HashMap<String, String>,
) {
    match data.kind() {
        "array_creation_expression" => {
            let mut c = data.walk();
            for el in data.named_children(&mut c) {
                if el.kind() != "array_element_initializer" {
                    continue;
                }
                let mut ec = el.walk();
                let kids: Vec<_> = el.named_children(&mut ec).collect();
                if kids.len() != 2 {
                    continue;
                }
                let Some(key) = string_literal_value(kids[0], bytes) else {
                    continue;
                };
                if let Some((fqcn, _)) = resolve_expression_type(
                    kids[1],
                    bytes,
                    aliases,
                    resolver,
                    classviews,
                    project_root,
                ) {
                    vars.insert(key, fqcn);
                }
            }
        }
        // `compact('user', 'post')` — each named local resolved in this scope.
        "function_call_expression" if call_function_name(data, bytes) == Some("compact") => {
            if let Some(args) = data.child_by_field_name("arguments") {
                for arg in positional_args(args) {
                    let Some(name) = string_literal_value(arg, bytes) else {
                        continue;
                    };
                    // Resolve `$name` at the compact() call site (right scope).
                    if let Some(fqcn) = flow::resolve(arg, bytes, &name, aliases) {
                        vars.insert(name, fqcn);
                    }
                }
            }
        }
        _ => {}
    }
}

/// Walk up from a `view(...)` node through chained `->with(...)` member calls,
/// folding each into `vars`.
#[allow(clippy::too_many_arguments)]
fn collect_with_chain(
    view_call: Node,
    bytes: &[u8],
    aliases: &crate::query_chain::use_aliases::UseAliases,
    resolver: &impl ClassFileResolver,
    classviews: &mut ClassViewCache,
    project_root: &Path,
    vars: &mut HashMap<String, String>,
) {
    let mut node = view_call;
    while let Some(parent) = node.parent() {
        if parent.kind() != "member_call_expression"
            || parent.child_by_field_name("object").map(|o| o.id()) != Some(node.id())
        {
            break;
        }
        if parent
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(bytes).ok())
            == Some("with")
        {
            if let Some(args) = parent.child_by_field_name("arguments") {
                let exprs = positional_args(args);
                match exprs.as_slice() {
                    // ->with('key', $value)
                    [key, value] => {
                        if let Some(name) = string_literal_value(*key, bytes) {
                            if let Some((fqcn, _)) = resolve_expression_type(
                                *value,
                                bytes,
                                aliases,
                                resolver,
                                classviews,
                                project_root,
                            ) {
                                vars.insert(name, fqcn);
                            }
                        }
                    }
                    // ->with(['key' => $value])
                    [data] => collect_vars(
                        *data,
                        bytes,
                        aliases,
                        resolver,
                        classviews,
                        project_root,
                        vars,
                    ),
                    _ => {}
                }
            }
        }
        node = parent;
    }
}

/// The bare function name of a `function_call_expression` (`view`, `compact`),
/// or `None` for dynamic / namespaced calls.
fn call_function_name<'a>(call: Node, bytes: &'a [u8]) -> Option<&'a str> {
    let f = call.child_by_field_name("function")?;
    if f.kind() == "name" {
        f.utf8_text(bytes).ok()
    } else {
        None
    }
}

/// Positional argument expressions of an `arguments` node (skips the `argument`
/// wrapper tree-sitter inserts; ignores named args).
fn positional_args(arguments: Node) -> Vec<Node> {
    let mut out = Vec::new();
    let mut c = arguments.walk();
    for arg in arguments.named_children(&mut c) {
        if arg.kind() == "argument" {
            // The wrapped expression is the argument's last named child.
            let mut ac = arg.walk();
            if let Some(expr) = arg.named_children(&mut ac).last() {
                out.push(expr);
            }
        } else {
            out.push(arg);
        }
    }
    out
}

/// The content of a single/double-quoted string literal node, or `None`.
fn string_literal_value(node: Node, bytes: &[u8]) -> Option<String> {
    if !matches!(node.kind(), "string" | "encapsed_string") {
        return None;
    }
    Some(
        node.utf8_text(bytes)
            .ok()?
            .trim_matches(['\'', '"'])
            .to_string(),
    )
}

#[cfg(test)]
mod tests;
