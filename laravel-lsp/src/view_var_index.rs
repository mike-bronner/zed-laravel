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
use crate::query_chain::use_aliases::{extract_use_aliases, resolve_class_name, UseAliases};
use crate::salsa_impl::{BladeLoopVar, Confidence, MemberAccessReferenceData};
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
    blade_loops: &[BladeLoopVar],
    resolver: &impl ClassFileResolver,
    classviews: &mut ClassViewCache,
    project_root: &Path,
) -> Vec<MagicMemberEntry> {
    let mut out: Vec<MagicMemberEntry> = Vec::new();
    let mut seen: HashSet<(String, u32, u32)> = HashSet::new();

    // Type a bare `$var` against the view-variable index, falling back to a
    // `@foreach($collection as $var)` loop where the collection is a view var.
    let var_types = |var: &str, line: u32| -> Vec<String> {
        let direct = view_index.var_types(view_name, var);
        if !direct.is_empty() {
            return direct;
        }
        match enclosing_loop_iterable(blade_loops, var, line) {
            // `@foreach($users as $user)` — element type is the iterable view
            // var's type (the flow classifier already yields the model, not
            // `Collection<T>`).
            Some(iter) => bare_variable(iter)
                .map(|iv| view_index.var_types(view_name, iv))
                .unwrap_or_default(),
            None => Vec::new(),
        }
    };

    for m in member_refs {
        let receiver = m.receiver.trim();

        // Collect every declaring FQCN this access resolves to. A bare `$var`
        // can have multiple inferred types (union across render sites), so this
        // may yield more than one entry — each a valid find-references target.
        let declaring: Vec<String> = if let Some(var) = bare_variable(receiver) {
            var_types(var, m.line)
                .into_iter()
                .filter_map(|fqcn| {
                    classify_fqcn_member(
                        &fqcn,
                        &m.member,
                        m.form,
                        resolver,
                        classviews,
                        project_root,
                    )
                    .map(|c| c.declaring_fqcn)
                })
                .collect()
        } else {
            resolve_chain_receiver(
                receiver,
                &m.member,
                m.form,
                resolver,
                classviews,
                project_root,
            )
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

/// Classify `member` against `fqcn`'s resolved surfaces.
fn classify_fqcn_member(
    fqcn: &str,
    member: &str,
    form: AccessForm,
    resolver: &impl ClassFileResolver,
    classviews: &mut ClassViewCache,
    project_root: &Path,
) -> Option<ClassifiedMember> {
    let file = resolver.class_file(fqcn)?;
    let view = classviews.get_or_build(fqcn, &file, project_root)?;
    classify_member(&view, member, form)
}

/// Resolve a non-variable receiver (`auth()->user()`, `Auth::user()`, a chain)
/// by parsing it as a standalone PHP expression and running the shared receiver
/// resolver, then classify `member`. Only HIGH/MEDIUM receiver confidence is
/// accepted — the find-references gate.
fn resolve_chain_receiver(
    receiver_text: &str,
    member: &str,
    form: AccessForm,
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
    classify_fqcn_member(&fqcn, member, form, resolver, classviews, project_root)
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

// ── Livewire/Volt component member references ─────────────────────────────
//
// A Volt SFC is an anonymous class (`new class extends Component`) with no
// FQCN, so `$this->entities` (a reference to the component's own property /
// `#[Computed]` method) can't key into the magic-member index the way a model
// member does. We give each component a synthetic, stable identity derived from
// its file — shared across the `.php` class and its `.blade.php` template — and
// key every `$this->member` read under it, so find-references on a component
// member works within the component (both files).

/// The synthetic reverse-index FQCN for the Livewire/Volt component `path`
/// belongs to, or `None` if it isn't a component file. One component → one key,
/// shared between its `.php` class and `.blade.php` template (an MFC template
/// resolves to its sibling `.php`'s key).
pub fn volt_component_key(path: &Path, source: &str) -> Option<String> {
    let is_blade = path.to_string_lossy().ends_with(".blade.php");
    if is_blade {
        if crate::livewire_resolver::source_contains_volt_signature(source) {
            // Single-file Volt component: the Blade file is the component.
            Some(format!("volt::{}", path.display()))
        } else {
            // MFC template: identity is the sibling `.php` class file.
            crate::livewire_resolver::mfc_sibling(path)
                .map(|sib| format!("volt::{}", sib.display()))
        }
    } else if crate::php_class::detect_inline_livewire_class(source) {
        Some(format!("volt::{}", path.display()))
    } else {
        None
    }
}

/// The declared member names (properties + methods, any visibility) of the
/// component class in `class_source`. Used to gate which `$this->member` reads
/// are real component members worth indexing (vs. framework calls like
/// `$this->dispatch`).
fn volt_component_member_names(class_source: &str) -> HashSet<String> {
    let Ok(tree) = parse_php(class_source) else {
        return HashSet::new();
    };
    let bytes = class_source.as_bytes();
    let mut names = HashSet::new();
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "property_declaration" => {
                let mut c = n.walk();
                for ch in n.children(&mut c) {
                    if ch.kind() == "property_element" {
                        if let Some(nm) = ch
                            .child_by_field_name("name")
                            .and_then(|x| x.utf8_text(bytes).ok())
                        {
                            names.insert(nm.trim_start_matches('$').to_string());
                        }
                    }
                }
            }
            "property_promotion_parameter" => {
                if let Some(nm) = n
                    .child_by_field_name("name")
                    .and_then(|x| x.utf8_text(bytes).ok())
                {
                    names.insert(nm.trim_start_matches('$').to_string());
                }
            }
            "method_declaration" => {
                if let Some(nm) = n
                    .child_by_field_name("name")
                    .and_then(|x| x.utf8_text(bytes).ok())
                {
                    names.insert(nm.to_string());
                }
            }
            _ => {}
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    names
}

/// Index `$this->member` reads in a Livewire/Volt component file under the
/// component's synthetic key, so find-references on a component property /
/// `#[Computed]` method works across the `.php` and `.blade.php`. Only members
/// actually declared on the component class are keyed. Returns empty for
/// non-component files.
pub fn resolve_component_member_accesses(
    path: &Path,
    source: &str,
    member_refs: &[Arc<MemberAccessReferenceData>],
) -> Vec<MagicMemberEntry> {
    let Some(key) = volt_component_key(path, source) else {
        return Vec::new();
    };

    // The component class source: the `.php` itself, a Blade SFC's front-matter,
    // or — for an MFC template — the sibling `.php`.
    let is_blade = path.to_string_lossy().ends_with(".blade.php");
    let class_source: String = if !is_blade {
        source.to_string()
    } else if crate::livewire_resolver::source_contains_volt_signature(source) {
        volt_frontmatter(source).unwrap_or_default().to_string()
    } else {
        match crate::livewire_resolver::mfc_sibling(path) {
            Some(sib) => std::fs::read_to_string(&sib).unwrap_or_default(),
            None => return Vec::new(),
        }
    };
    if class_source.is_empty() {
        return Vec::new();
    }
    let members = volt_component_member_names(&class_source);
    if members.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    let mut seen: HashSet<(u32, u32)> = HashSet::new();
    for m in member_refs {
        if m.receiver.trim() != "$this" || !members.contains(&m.member) {
            continue;
        }
        if seen.insert((m.line, m.column)) {
            out.push(MagicMemberEntry {
                fqcn: key.clone(),
                member: m.member.clone(),
                line: m.line,
                column: m.column,
                end_column: m.end_column,
            });
        }
    }
    out
}

// ── Volt component view variables (phase 5) ───────────────────────────────
//
// A Volt page (`resources/views/livewire/users.blade.php`) declares its own
// template variables in a leading `<?php … ?>` front-matter block — there is no
// external controller. The template reads them as `$this->prop` (and bare
// `$prop` for public properties / state). We infer types from every reliable
// shape:
//   - typed public properties — `public User $user;` (authoritative)
//   - `mount()` typed-param → `$this->prop` assignment (functional + class)
//   - `state(['user' => User::first()])` initial-value types
//   - `$user = computed(fn (): User => …)` / inferred-from-body return type
//   - `with(fn () => ['posts' => …])` and the class-API `with(): array`
//   - the class-API `render()` returning `view('…', ['extra' => …])`
// Typed public properties win over everything inferred. Untyped `state(['x'])`
// and unresolvable values are simply omitted.

/// Extract a Volt component's typed view variables (prop name → FQCN) from its
/// front-matter PHP block. Resolver-aware: value expressions (`User::first()`,
/// computed bodies, render data) are typed via [`resolve_expression_type`]; the
/// block's own `use` imports qualify bare type names.
pub fn volt_property_types(
    source: &str,
    resolver: &impl ClassFileResolver,
    classviews: &mut ClassViewCache,
    project_root: &Path,
) -> HashMap<String, String> {
    let Some(front) = volt_frontmatter(source) else {
        return HashMap::new();
    };
    let Ok(tree) = parse_php(front) else {
        return HashMap::new();
    };
    let bytes = front.as_bytes();
    let aliases = extract_use_aliases(&tree, front);
    let mut out: HashMap<String, String> = HashMap::new();

    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        match n.kind() {
            // Typed public property — authoritative, so plain `insert`.
            "property_declaration" if is_public(n, bytes) => {
                if let (Some(ty), Some(name)) = (
                    n.child_by_field_name("type"),
                    property_element_name(n, bytes),
                ) {
                    if let Some(fqcn) = clean_type(ty.utf8_text(bytes).unwrap_or("")) {
                        out.insert(name, resolve_class_name(&fqcn, &aliases));
                    }
                }
            }
            // Class-API `public function mount(User $u) { $this->u = $u; }`.
            "method_declaration" if method_name_is(n, bytes, "mount") => {
                collect_mount_assignments(n, bytes, &aliases, &mut out);
            }
            // Class-API `public function with(): array { return [...]; }`.
            "method_declaration" if method_name_is(n, bytes, "with") => {
                if let Some(ret) = function_return_expr(n) {
                    let mut temp = HashMap::new();
                    collect_vars(
                        ret,
                        bytes,
                        &aliases,
                        resolver,
                        classviews,
                        project_root,
                        &mut temp,
                    );
                    fold_or_insert(&mut out, temp);
                }
            }
            // Class-API `public function render() { return view('…', [...]); }`.
            "method_declaration" if method_name_is(n, bytes, "render") => {
                let temp =
                    render_method_vars(n, bytes, &aliases, resolver, classviews, project_root);
                fold_or_insert(&mut out, temp);
            }
            // `#[Computed] public function users(): Collection { return User::…->get(); }`
            // A computed property is read in the template as `$this->users`. The
            // declared return type is often a bare `Collection`, so prefer the
            // body's inferred type (the flow chain classifier returns the model
            // for a collection-producing chain — exactly the element type a
            // `@foreach($this->users as $user)` loop needs); fall back to a
            // resolvable (non-collection) return type for scalar computeds.
            "method_declaration" if method_has_attribute(n, bytes, "Computed") => {
                if let Some(name) = n
                    .child_by_field_name("name")
                    .and_then(|nm| nm.utf8_text(bytes).ok())
                {
                    let fqcn = function_return_expr(n)
                        .and_then(|ret| {
                            resolve_expression_type(
                                ret,
                                bytes,
                                &aliases,
                                resolver,
                                classviews,
                                project_root,
                            )
                            .map(|(f, _)| f)
                        })
                        .or_else(|| {
                            n.child_by_field_name("return_type")
                                .and_then(|rt| clean_type(rt.utf8_text(bytes).ok()?))
                                .map(|t| resolve_class_name(&t, &aliases))
                        });
                    if let Some(fqcn) = fqcn {
                        out.entry(name.to_string()).or_insert(fqcn);
                    }
                }
            }
            // Functional-API `mount(function (User $u) { $this->u = $u; });`.
            "function_call_expression" if call_function_name(n, bytes) == Some("mount") => {
                if let Some(closure) = first_closure_arg(n) {
                    collect_mount_assignments(closure, bytes, &aliases, &mut out);
                }
            }
            // Functional-API `state(['user' => User::first()]);`.
            "function_call_expression" if call_function_name(n, bytes) == Some("state") => {
                if let Some(args) = n.child_by_field_name("arguments") {
                    if let Some(data) = positional_args(args).first() {
                        let mut temp = HashMap::new();
                        collect_vars(
                            *data,
                            bytes,
                            &aliases,
                            resolver,
                            classviews,
                            project_root,
                            &mut temp,
                        );
                        fold_or_insert(&mut out, temp);
                    }
                }
            }
            // Functional-API `with(fn () => ['posts' => Post::all()]);`.
            "function_call_expression" if call_function_name(n, bytes) == Some("with") => {
                if let Some(closure) = first_closure_arg(n) {
                    if let Some(ret) = function_return_expr(closure) {
                        let mut temp = HashMap::new();
                        collect_vars(
                            ret,
                            bytes,
                            &aliases,
                            resolver,
                            classviews,
                            project_root,
                            &mut temp,
                        );
                        fold_or_insert(&mut out, temp);
                    }
                }
            }
            // Functional-API `$user = computed(fn (): User => …);`.
            "assignment_expression" => {
                if let Some((var, fqcn)) =
                    computed_assignment(n, bytes, &aliases, resolver, classviews, project_root)
                {
                    out.entry(var).or_insert(fqcn);
                }
            }
            _ => {}
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    out
}

/// Fold `temp` into `out` without overwriting — keeps authoritative typed
/// properties ahead of inferred state/computed/with/render types.
fn fold_or_insert(out: &mut HashMap<String, String>, temp: HashMap<String, String>) {
    for (k, v) in temp {
        out.entry(k).or_insert(v);
    }
}

/// `$var = computed(fn (): T => …)` → `(var, FQCN)`. Prefers an explicit closure
/// return type; otherwise infers from the body's returned expression.
fn computed_assignment(
    assign: Node,
    bytes: &[u8],
    aliases: &UseAliases,
    resolver: &impl ClassFileResolver,
    classviews: &mut ClassViewCache,
    project_root: &Path,
) -> Option<(String, String)> {
    let var = plain_variable_name(assign.child_by_field_name("left")?, bytes)?;
    let right = assign.child_by_field_name("right")?;
    if right.kind() != "function_call_expression"
        || call_function_name(right, bytes) != Some("computed")
    {
        return None;
    }
    let closure = first_closure_arg(right)?;
    // Explicit return type wins.
    if let Some(rt) = closure.child_by_field_name("return_type") {
        if let Some(t) = rt.utf8_text(bytes).ok().and_then(clean_type) {
            return Some((var, resolve_class_name(&t, aliases)));
        }
    }
    // Otherwise infer from the returned expression.
    let ret = function_return_expr(closure)?;
    let (fqcn, _) =
        resolve_expression_type(ret, bytes, aliases, resolver, classviews, project_root)?;
    Some((var, fqcn))
}

/// The view-data variables of a class-API `render()` returning a `view('…', […])`
/// call (also folding any chained `->with(...)`).
fn render_method_vars(
    method: Node,
    bytes: &[u8],
    aliases: &UseAliases,
    resolver: &impl ClassFileResolver,
    classviews: &mut ClassViewCache,
    project_root: &Path,
) -> HashMap<String, String> {
    let Some(ret) = function_return_expr(method) else {
        return HashMap::new();
    };
    let Some(view_call) = find_view_call(ret, bytes) else {
        return HashMap::new();
    };
    render_from_view_call(
        view_call,
        bytes,
        aliases,
        resolver,
        classviews,
        project_root,
    )
    .map(|r| r.vars)
    .unwrap_or_default()
}

/// The `view('…', …)` call within an expression subtree (the `render()` return),
/// or `None`.
fn find_view_call<'t>(node: Node<'t>, bytes: &[u8]) -> Option<Node<'t>> {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == "function_call_expression" && call_function_name(n, bytes) == Some("view") {
            return Some(n);
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    None
}

/// The expression returned by a closure/method `func`: the arrow body directly,
/// or the first `return <expr>;` in a block body (not descending into nested
/// functions).
fn function_return_expr(func: Node) -> Option<Node> {
    let body = func.child_by_field_name("body")?;
    if body.kind() != "compound_statement" {
        return Some(body); // arrow `fn () => <expr>`
    }
    let mut stack = vec![body];
    while let Some(n) = stack.pop() {
        // Don't leak into a nested closure's `return`.
        if n.id() != body.id()
            && matches!(
                n.kind(),
                "anonymous_function" | "arrow_function" | "method_declaration"
            )
        {
            continue;
        }
        if n.kind() == "return_statement" {
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

/// If `blade_path` is a multi-file Volt component — a `.blade.php` template with
/// no own Volt front-matter but an inline-Livewire-class `.php` sibling (e.g.
/// `users.blade.php` + `users.php`) — extract the sibling class's component
/// property types so the template's `$this->prop` and loop variables resolve.
/// `None` for plain Blade with no Volt sibling. Reads the sibling once.
pub fn mfc_volt_property_types(
    blade_path: &Path,
    resolver: &impl ClassFileResolver,
    classviews: &mut ClassViewCache,
    project_root: &Path,
) -> Option<HashMap<String, String>> {
    let sibling = crate::livewire_resolver::mfc_sibling(blade_path)?;
    let source = std::fs::read_to_string(&sibling).ok()?;
    Some(volt_property_types(
        &source,
        resolver,
        classviews,
        project_root,
    ))
}

/// Resolve a Volt file's captured member accesses against its inferred property
/// types. Receivers `$this->prop` and bare `$prop` are typed from `prop_types`;
/// other shapes (`auth()->user()->email`) fall back to standalone resolution.
/// Entries land in the same reverse index as PHP/Blade accesses.
pub fn resolve_volt_member_accesses(
    member_refs: &[Arc<MemberAccessReferenceData>],
    prop_types: &HashMap<String, String>,
    blade_loops: &[BladeLoopVar],
    resolver: &impl ClassFileResolver,
    classviews: &mut ClassViewCache,
    project_root: &Path,
) -> Vec<MagicMemberEntry> {
    let mut out: Vec<MagicMemberEntry> = Vec::new();
    let mut seen: HashSet<(String, u32, u32)> = HashSet::new();

    for m in member_refs {
        let receiver = m.receiver.trim();
        let declaring: Option<String> = if let Some(prop) = volt_base_prop(receiver) {
            // Direct prop read (`$this->user`, or a bare public-prop/state read).
            let direct = prop_types.get(prop).and_then(|fqcn| {
                classify_fqcn_member(fqcn, &m.member, m.form, resolver, classviews, project_root)
                    .map(|c| c.declaring_fqcn)
            });
            // Loop fallback: a bare `$user` that isn't a prop but is the item of
            // `@foreach($this->users as $user)` — type it from the iterable
            // prop's element type (`users` computed → `User`).
            direct.or_else(|| {
                let var = bare_variable(receiver)?;
                let iter = enclosing_loop_iterable(blade_loops, var, m.line)?;
                let iter_prop = volt_base_prop(iter)?;
                prop_types.get(iter_prop).and_then(|fqcn| {
                    classify_fqcn_member(
                        fqcn,
                        &m.member,
                        m.form,
                        resolver,
                        classviews,
                        project_root,
                    )
                    .map(|c| c.declaring_fqcn)
                })
            })
        } else {
            resolve_chain_receiver(
                receiver,
                &m.member,
                m.form,
                resolver,
                classviews,
                project_root,
            )
            .map(|c| c.declaring_fqcn)
        };

        if let Some(fqcn) = declaring {
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

/// The iterable expression of the innermost `@foreach` whose item variable is
/// `var` and whose body contains `line`. Returns the iterable as written
/// (`$users`, `$this->users`) for the caller to type.
fn enclosing_loop_iterable<'a>(loops: &'a [BladeLoopVar], var: &str, line: u32) -> Option<&'a str> {
    loops
        .iter()
        .filter(|l| l.item_var == var && line >= l.start_line && line <= l.end_line)
        .max_by_key(|l| l.start_line) // innermost enclosing loop wins
        .map(|l| l.iterable.as_str())
}

/// `$this->user` or bare `$user` → `Some("user")`; deeper chains
/// (`$this->user->profile`) and other shapes → `None`.
fn volt_base_prop(receiver: &str) -> Option<&str> {
    if let Some(rest) = receiver.strip_prefix("$this->") {
        return is_ident(rest).then_some(rest);
    }
    bare_variable(receiver)
}

fn is_ident(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// The leading `<?php … ?>` front-matter block (closing tag included), or the
/// rest of the file from `<?php` if there is no closing tag.
fn volt_frontmatter(source: &str) -> Option<&str> {
    let start = source.find("<?php")?;
    let after = &source[start..];
    match after.find("?>") {
        Some(end) => Some(&after[..end + 2]),
        None => Some(after),
    }
}

/// True if a `property_declaration` / `method_declaration` carries a `public`
/// visibility modifier.
fn is_public(node: Node, bytes: &[u8]) -> bool {
    let mut c = node.walk();
    let public = node.children(&mut c).any(|child| {
        child.kind() == "visibility_modifier"
            && child
                .utf8_text(bytes)
                .map(|t| t == "public")
                .unwrap_or(false)
    });
    public
}

/// The `$name` (stripped) of a `property_declaration`'s first `property_element`.
fn property_element_name(node: Node, bytes: &[u8]) -> Option<String> {
    let mut c = node.walk();
    for child in node.children(&mut c) {
        if child.kind() == "property_element" {
            return child
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(bytes).ok())
                .map(|t| t.trim_start_matches('$').to_string());
        }
    }
    None
}

fn method_name_is(node: Node, bytes: &[u8], name: &str) -> bool {
    node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(bytes).ok())
        == Some(name)
}

/// Whether a `method_declaration` carries an attribute whose name contains
/// `name` (e.g. `#[Computed]`, `#[Computed(persist: true)]`,
/// `#[\Livewire\Attributes\Computed]`). A substring check on the attribute list
/// text is robust to the FQCN and argument variants.
fn method_has_attribute(node: Node, bytes: &[u8], name: &str) -> bool {
    let mut c = node.walk();
    let found = node.children(&mut c).any(|ch| {
        ch.kind() == "attribute_list"
            && ch
                .utf8_text(bytes)
                .map(|t| t.contains(name))
                .unwrap_or(false)
    });
    found
}

/// The first `function (...) { … }` / `fn (...) => …` argument of a call.
fn first_closure_arg(call: Node) -> Option<Node> {
    let args = call.child_by_field_name("arguments")?;
    let mut c = args.walk();
    for arg in args.named_children(&mut c) {
        let inner = if arg.kind() == "argument" {
            arg.named_child(0)?
        } else {
            arg
        };
        if matches!(inner.kind(), "anonymous_function" | "arrow_function") {
            return Some(inner);
        }
    }
    None
}

/// Read a `mount` closure/method's typed params, then map every
/// `$this->prop = $param` assignment in its body to the param's type.
/// Uses `or_insert` so a typed public property already recorded wins.
fn collect_mount_assignments(
    func: Node,
    bytes: &[u8],
    aliases: &UseAliases,
    out: &mut HashMap<String, String>,
) {
    // param name → FQCN
    let mut param_types: HashMap<String, String> = HashMap::new();
    if let Some(params) = func.child_by_field_name("parameters") {
        let mut c = params.walk();
        for p in params.named_children(&mut c) {
            if p.kind() != "simple_parameter" {
                continue;
            }
            let (Some(ty), Some(nm)) =
                (p.child_by_field_name("type"), p.child_by_field_name("name"))
            else {
                continue;
            };
            let Some(fqcn) = clean_type(ty.utf8_text(bytes).unwrap_or("")) else {
                continue;
            };
            if let Ok(name) = nm.utf8_text(bytes) {
                param_types.insert(
                    name.trim_start_matches('$').to_string(),
                    resolve_class_name(&fqcn, aliases),
                );
            }
        }
    }
    if param_types.is_empty() {
        return;
    }

    let Some(body) = func.child_by_field_name("body") else {
        return;
    };
    let mut stack = vec![body];
    while let Some(n) = stack.pop() {
        if n.kind() == "assignment_expression" {
            if let (Some(left), Some(right)) = (
                n.child_by_field_name("left"),
                n.child_by_field_name("right"),
            ) {
                if let (Some(prop), Some(param)) = (
                    this_property_name(left, bytes),
                    plain_variable_name(right, bytes),
                ) {
                    if let Some(fqcn) = param_types.get(&param) {
                        out.entry(prop).or_insert_with(|| fqcn.clone());
                    }
                }
            }
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
}

/// `$this->prop` member access → `Some("prop")`.
fn this_property_name(node: Node, bytes: &[u8]) -> Option<String> {
    if node.kind() != "member_access_expression" {
        return None;
    }
    let object = node.child_by_field_name("object")?;
    if object.kind() != "variable_name" || object.utf8_text(bytes).ok()? != "$this" {
        return None;
    }
    node.child_by_field_name("name")?
        .utf8_text(bytes)
        .ok()
        .map(|t| t.trim_start_matches('$').to_string())
}

/// A bare `$param` variable node → `Some("param")`.
fn plain_variable_name(node: Node, bytes: &[u8]) -> Option<String> {
    (node.kind() == "variable_name")
        .then(|| node.utf8_text(bytes).ok())
        .flatten()
        .map(|t| t.trim_start_matches('$').to_string())
}

/// Normalize a declared type to a single resolvable class name: strip a leading
/// `?` (nullable), reject union/intersection (ambiguous), drop the leading `\`,
/// and reject built-in/pseudo types (which name no class).
fn clean_type(raw: &str) -> Option<String> {
    let t = raw.trim().trim_start_matches('?').trim();
    if t.is_empty() || t.contains('|') || t.contains('&') {
        return None;
    }
    let t = t.trim_start_matches('\\');
    const BUILTINS: &[&str] = &[
        "int", "float", "string", "bool", "array", "object", "mixed", "void", "null", "false",
        "true", "iterable", "callable", "self", "static", "parent", "never",
    ];
    if BUILTINS.iter().any(|b| b.eq_ignore_ascii_case(t)) {
        return None;
    }
    Some(t.to_string())
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
