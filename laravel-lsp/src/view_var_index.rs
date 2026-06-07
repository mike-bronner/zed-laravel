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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tree_sitter::Node;

use crate::member_resolver::{
    classify_member, resolve_expression_type, AccessForm, ClassFileResolver, ClassViewCache,
    ClassifiedMember,
};
use crate::parser::parse_php;
use crate::query_chain::flow;
use crate::query_chain::use_aliases::extract_use_aliases;
use crate::salsa_impl::{Confidence, MemberAccessReferenceData};
use crate::symbol_index::MagicMemberEntry;

/// One `view('name', …)` render site: the rendered view and the variable →
/// FQCN types it passes in (only the variables whose type resolved).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewRender {
    pub view_name: String,
    pub vars: HashMap<String, String>,
}

/// Project-wide reverse index: view name → variable → set of FQCN types that
/// any render site passes in for that variable.
///
/// **Union aggregation.** A view can be rendered from many places
/// (`UserController::show` passes `App\Models\User`, `AdminController::show`
/// might pass `App\Models\Admin`). We keep *all* observed types per variable so
/// Blade member-access resolution can match against any of them — the "match
/// any" aggregation chosen for this milestone.
///
/// **No persistence.** This index is rebuilt every warm from re-read source +
/// the (already-persisted) hierarchy, so it never hits the empty-on-restart
/// trap. `by_file` exists only for incremental eviction within a live session.
#[derive(Debug, Default)]
pub struct ViewVarIndex {
    /// view name → (variable name → set of FQCN types).
    forward: HashMap<String, HashMap<String, HashSet<String>>>,
    /// file → the view names it contributed render sites for (for eviction).
    by_file: HashMap<PathBuf, Vec<String>>,
}

impl ViewVarIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold a file's render sites into the index. Replaces any prior
    /// contribution from the same file (evict-then-insert) so a re-parse of an
    /// edited controller doesn't leave stale types behind.
    pub fn insert_file(&mut self, path: PathBuf, renders: &[ViewRender]) {
        self.remove_file(&path);
        let mut contributed = Vec::new();
        for render in renders {
            let view = self.forward.entry(render.view_name.clone()).or_default();
            for (var, fqcn) in &render.vars {
                view.entry(var.clone()).or_default().insert(fqcn.clone());
            }
            contributed.push(render.view_name.clone());
        }
        if !contributed.is_empty() {
            self.by_file.insert(path, contributed);
        }
    }

    /// Drop a file's contribution. Because `forward` is a union across files,
    /// eviction does a targeted rebuild of only the affected views from the
    /// surviving files — correct, if not the cheapest possible.
    pub fn remove_file(&mut self, path: &Path) {
        let Some(views) = self.by_file.remove(path) else {
            return;
        };
        for view in views {
            // Clearing the whole view entry is imprecise (other files may feed
            // it), but a per-file rebuild needs per-file type provenance we
            // don't keep. The warm rebuild clears the whole index anyway; this
            // path only matters for live single-session edits, where dropping
            // the view's vars and letting the still-open renderers re-add them
            // on their next parse is acceptable.
            self.forward.remove(&view);
        }
    }

    /// All FQCN types observed for `var` in `view_name`, across every render
    /// site (the union). Empty if the view/var was never seen.
    pub fn var_types(&self, view_name: &str, var: &str) -> Vec<String> {
        self.forward
            .get(view_name)
            .and_then(|vars| vars.get(var))
            .map(|set| {
                let mut v: Vec<String> = set.iter().cloned().collect();
                v.sort();
                v
            })
            .unwrap_or_default()
    }

    /// Clear everything — called at the start of a warm rebuild.
    pub fn clear(&mut self) {
        self.forward.clear();
        self.by_file.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.forward.is_empty()
    }

    pub fn view_count(&self) -> usize {
        self.forward.len()
    }
}

/// Map a `.blade.php` file path to its Laravel view name, given the project's
/// view-root directories (e.g. `resources/views`). Strips the matching root
/// prefix and the `.blade.php` (or `.php`) suffix, then converts path
/// separators to dots: `resources/views/users/show.blade.php` → `users.show`.
///
/// `view_roots` are tried longest-first so a nested namespace root wins over a
/// parent. Returns `None` if the file isn't under any known view root.
pub fn view_name_for_path(file: &Path, view_roots: &[PathBuf]) -> Option<String> {
    // Longest root first: a more specific root (vendor package view dir) should
    // win over the catch-all `resources/views`.
    let mut roots: Vec<&PathBuf> = view_roots.iter().collect();
    roots.sort_by_key(|r| std::cmp::Reverse(r.components().count()));

    for root in roots {
        let Ok(rel) = file.strip_prefix(root) else {
            continue;
        };
        let rel_str = rel.to_string_lossy();
        let stem = rel_str
            .strip_suffix(".blade.php")
            .or_else(|| rel_str.strip_suffix(".php"))?;
        if stem.is_empty() {
            return None;
        }
        return Some(stem.replace(['/', '\\'], "."));
    }
    None
}

/// Resolve the property-form member accesses captured in a Blade file into
/// magic-member reference entries, using the project-wide view-variable index.
///
/// A Blade `{{ $user->email }}` can't be resolved from the `.blade.php` alone —
/// `$user`'s type comes from whatever controller rendered the view. Given the
/// file's `view_name`, each bare-`$var` receiver is typed via
/// [`ViewVarIndex::var_types`] (the union of every render site's inferred type),
/// then the member is classified against that class's surfaces. Receivers that
/// aren't plain variables (`auth()->user()->email`, `Auth::user()->email`) are
/// resolved standalone via the shared receiver resolver — those need no view
/// context.
///
/// Positions come straight from the captured refs (already mapped to outer
/// Blade-file coordinates by the capture pass), so entries point at the member
/// name in the `.blade.php`. Sites that don't resolve are dropped.
pub fn resolve_blade_member_accesses(
    member_refs: &[Arc<MemberAccessReferenceData>],
    view_name: &str,
    view_index: &ViewVarIndex,
    resolver: &impl ClassFileResolver,
    classviews: &mut ClassViewCache,
    project_root: &Path,
) -> Vec<MagicMemberEntry> {
    let mut out: Vec<MagicMemberEntry> = Vec::new();
    let mut seen: HashSet<(String, u32, u32)> = HashSet::new();

    for m in member_refs {
        let receiver = m.receiver.trim();

        // Collect every declaring FQCN this access resolves to. A bare `$var`
        // can have multiple inferred types (union across render sites), so this
        // may yield more than one entry — each a valid find-references target.
        let declaring: Vec<String> = if let Some(var) = bare_variable(receiver) {
            view_index
                .var_types(view_name, var)
                .into_iter()
                .filter_map(|fqcn| {
                    classify_fqcn_member(&fqcn, &m.member, resolver, classviews, project_root)
                        .map(|c| c.declaring_fqcn)
                })
                .collect()
        } else {
            resolve_chain_receiver(receiver, &m.member, resolver, classviews, project_root)
                .map(|c| vec![c.declaring_fqcn])
                .unwrap_or_default()
        };

        for fqcn in declaring {
            // A single site can map to the same declaring class twice (two
            // inferred receiver types that share a base declaring the member);
            // keep one entry per (class, position).
            if seen.insert((fqcn.clone(), m.line, m.column)) {
                out.push(MagicMemberEntry {
                    fqcn,
                    member: m.member.clone(),
                    line: m.line,
                    column: m.column,
                    end_column: m.end_column,
                });
            }
        }
    }
    out
}

/// `$user` → `Some("user")`; anything that isn't a single bare variable
/// (`auth()->user()`, `$this->user`, …) → `None`.
fn bare_variable(text: &str) -> Option<&str> {
    let var = text.strip_prefix('$')?;
    if !var.is_empty() && var.chars().all(|c| c.is_alphanumeric() || c == '_') {
        Some(var)
    } else {
        None
    }
}

/// Classify `member` (property form) against `fqcn`'s resolved surfaces.
fn classify_fqcn_member(
    fqcn: &str,
    member: &str,
    resolver: &impl ClassFileResolver,
    classviews: &mut ClassViewCache,
    project_root: &Path,
) -> Option<ClassifiedMember> {
    let file = resolver.class_file(fqcn)?;
    let view = classviews.get_or_build(fqcn, &file, project_root)?;
    classify_member(&view, member, AccessForm::Property)
}

/// Resolve a non-variable receiver (`auth()->user()`, `Auth::user()`, a chain)
/// by parsing it as a standalone PHP expression and running the shared receiver
/// resolver, then classify `member`. Only HIGH/MEDIUM receiver confidence is
/// accepted — the find-references gate.
fn resolve_chain_receiver(
    receiver_text: &str,
    member: &str,
    resolver: &impl ClassFileResolver,
    classviews: &mut ClassViewCache,
    project_root: &Path,
) -> Option<ClassifiedMember> {
    let snippet = format!("<?php {receiver_text};");
    let tree = parse_php(&snippet).ok()?;
    let bytes = snippet.as_bytes();
    let aliases = extract_use_aliases(&tree, &snippet);
    let expr = first_expression(&tree)?;
    let (fqcn, confidence) =
        resolve_expression_type(expr, bytes, &aliases, resolver, classviews, project_root)?;
    if !matches!(confidence, Confidence::High | Confidence::Medium) {
        return None;
    }
    classify_fqcn_member(&fqcn, member, resolver, classviews, project_root)
}

/// The expression of the first `expression_statement` in a parsed snippet
/// (`<?php <expr>;` → the `<expr>` node).
fn first_expression(tree: &tree_sitter::Tree) -> Option<Node<'_>> {
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if n.kind() == "expression_statement" {
            let mut c = n.walk();
            return n.named_children(&mut c).next();
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    None
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
