//! Magic-member resolve + classify engine.
//!
//! M3 of the semantic-index plan. Given a member access — property-form
//! (`$user->email`) or call-form (`User::active()`, `$q->active()`) — resolve
//! the receiver to a class and classify the member against that class's
//! inheritance-resolved Laravel surfaces (scopes, accessors, relationships,
//! columns) plus dynamic finders.
//!
//! This module is the **engine**: pure functions over a [`ClassView`] (and,
//! for the orchestrator added later, the class-hierarchy index + a ClassView
//! cache). M3 ships the engine + fixtures; M4 wires it into the reverse
//! reference index and find-references.
//!
//! Classification is inheritance/trait resolved: the `declaring_fqcn` is the
//! class or trait that actually declares the member (via [`ClassView`]'s
//! `source_class` provenance), so a trait-shared scope keys once and downstream
//! rename/lens can attribute every inheriting model correctly.

use crate::class_hierarchy_index::ClassHierarchyIndex;
use crate::laravel_introspector::chain::{analyze, ClassView};
use crate::laravel_introspector::model_metadata::pascal_to_snake;
use crate::parser::parse_php;
use crate::query_chain::flow;
use crate::query_chain::use_aliases::{extract_use_aliases, resolve_class_name, UseAliases};
use crate::salsa_impl::{Confidence, MagicMemberKind, MemberAccessReferenceData};
use crate::symbol_index::MagicMemberEntry;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tree_sitter::Node;

/// Maps a class FQCN to the file that declares it — the only thing receiver
/// resolution needs from the class graph. Implemented by the actor-owned
/// [`ClassHierarchyIndex`] (used at query time) and by a plain
/// `HashMap<String, PathBuf>` snapshot (used by the parallel index-build pass,
/// which can't borrow the actor-owned index). Decoupling here means resolution
/// works the same on either.
pub trait ClassFileResolver {
    fn class_file(&self, fqcn: &str) -> Option<PathBuf>;
}

impl ClassFileResolver for ClassHierarchyIndex {
    fn class_file(&self, fqcn: &str) -> Option<PathBuf> {
        self.get(fqcn).map(|node| node.file_path.clone())
    }
}

impl ClassFileResolver for HashMap<String, PathBuf> {
    fn class_file(&self, fqcn: &str) -> Option<PathBuf> {
        self.get(fqcn).cloned()
    }
}

// `AccessForm` moved to `salsa_impl` (it now travels inside
// `MemberAccessReferenceData` through the pattern cache); re-exported here so
// the engine's callers keep their `member_resolver::AccessForm` paths.
pub use crate::salsa_impl::AccessForm;

/// The classification of a resolved member: which declaring class owns it
/// (inheritance/trait resolved) and what magic kind it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedMember {
    /// FQCN of the class/trait that declares the member.
    pub declaring_fqcn: String,
    pub kind: MagicMemberKind,
}

/// Classify `member` accessed via `form` against `view`'s resolved surfaces.
///
/// Returns `None` when the member matches nothing known on the class — this is
/// what prunes the M2 capture firehose: an arbitrary `$x->whatever` whose
/// receiver resolves to a class without a matching member is simply dropped.
///
/// **Precedence** (first match wins, mirroring how Eloquent's magic resolves):
/// - property read: accessor → relationship → column → plain property
/// - call: scope → dynamic finder → relationship → plain method
///
/// Collisions between these are rare in real models; the order is fixed and
/// documented so classification is deterministic.
pub fn classify_member(
    view: &ClassView,
    member: &str,
    form: AccessForm,
) -> Option<ClassifiedMember> {
    if form.is_call() {
        classify_call(view, member)
    } else {
        classify_property(view, member)
    }
}

fn classify_property(view: &ClassView, member: &str) -> Option<ClassifiedMember> {
    // Accessor — explicit `get*Attribute` / `Attribute`-returning method.
    // Shadows a raw column of the same name (Laravel returns the accessor).
    if let Some(a) = view.accessors.iter().find(|a| a.property_name == member) {
        return Some(ClassifiedMember {
            declaring_fqcn: a.source_class.clone(),
            kind: MagicMemberKind::Accessor,
        });
    }
    // Relationship read as a property (`$user->posts` → Collection/Model).
    if let Some(r) = view.relationships.iter().find(|r| r.method_name == member) {
        return Some(ClassifiedMember {
            declaring_fqcn: r.source_class.clone(),
            kind: MagicMemberKind::Relationship,
        });
    }
    // Database column surfaced as a model attribute.
    if view.column_surface.iter().any(|c| c.name == member) {
        return Some(ClassifiedMember {
            declaring_fqcn: view.fqcn.clone(),
            kind: MagicMemberKind::Column,
        });
    }
    // Plain (non-magic) property declared somewhere in the hierarchy.
    if let Some(p) = view.all_properties.iter().find(|p| p.value.name == member) {
        return Some(ClassifiedMember {
            declaring_fqcn: p.source_class.clone(),
            kind: MagicMemberKind::PlainMember,
        });
    }
    None
}

fn classify_call(view: &ClassView, member: &str) -> Option<ClassifiedMember> {
    // Local scope (`scopeActive` → `->active()` / `Model::active()`).
    if let Some(s) = view.scopes.iter().find(|s| s.name == member) {
        return Some(ClassifiedMember {
            declaring_fqcn: s.source_class.clone(),
            kind: MagicMemberKind::Scope,
        });
    }
    // Dynamic finder (`User::whereEmail(...)`).
    if let Some(classified) = classify_dynamic_finder(view, member) {
        return Some(classified);
    }
    // Relationship called as a method (`$user->posts()` → Builder).
    if let Some(r) = view.relationships.iter().find(|r| r.method_name == member) {
        return Some(ClassifiedMember {
            declaring_fqcn: r.source_class.clone(),
            kind: MagicMemberKind::Relationship,
        });
    }
    // Plain (non-magic) method declared somewhere in the hierarchy.
    if let Some(m) = view.all_methods.iter().find(|m| m.value.name == member) {
        return Some(ClassifiedMember {
            declaring_fqcn: m.source_class.clone(),
            kind: MagicMemberKind::PlainMember,
        });
    }
    None
}

/// A fully resolved + classified member access: the inheritance-resolved
/// declaring class, the magic kind, and the confidence with which the
/// receiver was resolved. M4 maps this into a [`MagicMemberEntry`] for the
/// reverse reference index (it does not persist back into the per-file
/// `ParsedPatternsData` cache, whose scaffold fields stay the typed contract).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMemberAccess {
    pub declaring_fqcn: String,
    pub kind: MagicMemberKind,
    pub confidence: Confidence,
}

/// Per-FQCN [`ClassView`] memo so resolving a project's member-access firehose
/// analyzes each model file once, not once per access site. Caches misses too
/// (a `None`) so an unreadable / class-less file isn't re-analyzed repeatedly.
#[derive(Default)]
pub struct ClassViewCache {
    cache: HashMap<String, Option<Arc<ClassView>>>,
}

impl ClassViewCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the cached `ClassView` for `fqcn`, building it from `file_path`
    /// on first request.
    pub fn get_or_build(
        &mut self,
        fqcn: &str,
        file_path: &Path,
        project_root: &Path,
    ) -> Option<Arc<ClassView>> {
        if let Some(cached) = self.cache.get(fqcn) {
            return cached.clone();
        }
        let view = analyze(file_path, project_root).map(Arc::new);
        self.cache.insert(fqcn.to_string(), view.clone());
        view
    }
}

/// Resolve + classify every property-form member access captured in one file
/// into ingestible [`MagicMemberEntry`]s for the reverse reference index (M4).
///
/// Parses `source` once, then for each captured `member_access_ref` locates the
/// receiver node by its byte range and runs [`resolve_and_classify`]. Only
/// sites that resolve at HIGH or MEDIUM confidence are kept — the find-
/// references threshold — which also prunes the M2 capture firehose down to
/// real, classifiable usages. Unresolvable receivers and unknown members are
/// silently dropped.
///
/// `classviews` is reused across files by the caller so each model is analyzed
/// once per build pass.
///
/// `deps`, when provided, accumulates every receiver FQCN resolution
/// *attempted* — including accesses whose member classification fails — for
/// the magic dependency index (see `magic_dependency_index`). Recording
/// attempts rather than successes is what lets a later "member added to
/// class" save re-resolve the files that were waiting on it.
pub fn resolve_member_access_entries(
    source: &str,
    member_refs: &[Arc<MemberAccessReferenceData>],
    resolver: &impl ClassFileResolver,
    classviews: &mut ClassViewCache,
    project_root: &Path,
    mut deps: Option<&mut HashSet<String>>,
) -> Vec<MagicMemberEntry> {
    if member_refs.is_empty() {
        return Vec::new();
    }
    let Ok(tree) = parse_php(source) else {
        return Vec::new();
    };
    let bytes = source.as_bytes();
    let aliases = extract_use_aliases(&tree, source);
    let root = tree.root_node();

    let mut out = Vec::new();
    for m in member_refs {
        let Some(receiver) =
            root.descendant_for_byte_range(m.receiver_byte_start, m.receiver_byte_end)
        else {
            continue;
        };
        let Some(resolved) = resolve_and_classify(
            receiver,
            &m.member,
            m.form,
            bytes,
            &aliases,
            resolver,
            classviews,
            project_root,
            deps.as_deref_mut(),
        ) else {
            continue;
        };
        // find-references gate: HIGH + MEDIUM (rename will gate to HIGH later).
        if !matches!(resolved.confidence, Confidence::High | Confidence::Medium) {
            continue;
        }
        // Call-form plain methods are every `->get()` / `->save()` in the
        // codebase — Intelephense's territory and pure index bloat. Only the
        // magic kinds (scope / finder / relationship) index from calls.
        // Property-form plain members stay (bounded: declared properties on
        // resolved classes).
        if m.form.is_call() && resolved.kind == MagicMemberKind::PlainMember {
            continue;
        }
        out.push(MagicMemberEntry {
            fqcn: resolved.declaring_fqcn,
            member: m.member.clone(),
            line: m.line,
            column: m.column,
            end_column: m.end_column,
        });
    }
    out
}

/// Resolve a property-form receiver to its class, then classify `member`
/// against that class's resolved surfaces.
///
/// The pipeline: `receiver → FQCN + confidence` (via [`flow`] / `$this`) →
/// `FQCN → file_path` (via the class-hierarchy index) → `ClassView` (cached) →
/// [`classify_member`]. Returns `None` whenever any step can't proceed — an
/// unresolvable receiver, a class not in the index, or a member that matches
/// nothing — which is what prunes the M2 capture firehose down to real sites.
///
/// This is the M3 engine; M4 calls it during the reverse-index build and
/// writes the result into each site's reserved scaffold.
///
/// `deps`, when provided, records the receiver FQCN(s) this access resolves
/// to — *before* member classification, so failed lookups still register a
/// dependency on the receiver's class.
#[allow(clippy::too_many_arguments)]
pub fn resolve_and_classify(
    receiver: Node,
    member: &str,
    form: AccessForm,
    bytes: &[u8],
    aliases: &UseAliases,
    resolver: &impl ClassFileResolver,
    classviews: &mut ClassViewCache,
    project_root: &Path,
    mut deps: Option<&mut HashSet<String>>,
) -> Option<ResolvedMemberAccess> {
    let (fqcn, confidence) =
        match resolve_receiver(receiver, bytes, aliases, resolver, classviews, project_root) {
            Some(r) => r,
            // Call-form receivers are frequently builder CHAINS
            // (`User::query()->active()`, `User::where(…)->active()`) whose
            // links the direct resolver can't type. The chain's subject is its
            // root — resolve that instead (#77 review). Property-form chains
            // (`User::first()->full_name`) deliberately stay chain-blind: the
            // column surface makes property terminals a far bigger
            // false-positive net than the call surfaces gated below.
            None if form.is_call() => resolve_call_chain_receiver(
                receiver,
                bytes,
                aliases,
                resolver,
                classviews,
                project_root,
            )?,
            None => return None,
        };

    if let Some(d) = deps.as_deref_mut() {
        d.insert(fqcn.clone());
    }

    if let Some(resolved) = classify_against(
        &fqcn,
        member,
        form,
        confidence,
        resolver,
        classviews,
        project_root,
    ) {
        return Some(resolved);
    }

    // Builder-typed receiver retry: `$query->active()` inside a scope body
    // types as the Eloquent Builder, which declares no scopes — retry against
    // the lexically enclosing class (the model whose scope body this is), the
    // same enclosing-model convention column rename uses. Gated to receivers
    // rooted in the enclosing `scope*` method's own parameter: a Builder
    // param inside a `whereHas` CLOSURE belongs to the related model, and
    // retrying it against the enclosing class would misattribute same-named
    // scopes. (Trade-off: correct attributions inside global-scope closures
    // are dropped too.) MEDIUM confidence — informational (every consumer
    // gate accepts High|Medium); the scope-param gate and classification are
    // the actual safety here.
    if form.is_call() && is_eloquent_builder(&fqcn) && is_scope_param_receiver(receiver, bytes) {
        let model = enclosing_class_fqcn(receiver, bytes)?;
        if let Some(d) = deps {
            d.insert(model.clone());
        }
        let resolved = classify_against(
            &model,
            member,
            form,
            Confidence::Medium,
            resolver,
            classviews,
            project_root,
        )?;
        return Some(resolved);
    }
    None
}

/// Classify `member` against `fqcn`'s resolved surfaces — the shared tail of
/// [`resolve_and_classify`]'s direct path and its builder retry.
fn classify_against(
    fqcn: &str,
    member: &str,
    form: AccessForm,
    confidence: Confidence,
    resolver: &impl ClassFileResolver,
    classviews: &mut ClassViewCache,
    project_root: &Path,
) -> Option<ResolvedMemberAccess> {
    let file_path = resolver.class_file(fqcn)?;
    let view = classviews.get_or_build(fqcn, &file_path, project_root)?;
    let classified = classify_member(&view, member, form)?;
    Some(ResolvedMemberAccess {
        declaring_fqcn: classified.declaring_fqcn,
        kind: classified.kind,
        confidence,
    })
}

/// Resolve a call-chain receiver by its ROOT (#77 review). The chain's
/// subject stays its root only for genuine BUILDER chains, so the static
/// branch is gated twice:
///
/// - **First-link gate**: the root's static method must actually forward to
///   the query builder — an Eloquent chain starter (`query`, `where`,
///   `find`, …; the same list receiver detection uses) or one of the class's
///   own scopes / dynamic finders. Declared statics returning non-builder
///   objects (`factory()`, `fake()`, bespoke constructors) must NOT resolve:
///   Factory states routinely share names with scopes, and a scope rename
///   would rewrite `User::factory()->active()`.
/// - **Relation-hop bail**: a relationship link re-targets the chain's
///   subject to the related model (`$user->posts()->active()` is Post's
///   scope, not User's) — conservatively drop rather than misattribute.
///
/// Static roots resolve at HIGH (explicit class name; `static::` late-binds,
/// so it gets MEDIUM via the enclosing class as a lower bound). A variable
/// root re-enters the direct resolver capped at MEDIUM — informational
/// (consumer gates accept High|Medium alike); the gates above plus
/// classification are the real safety.
fn resolve_call_chain_receiver(
    receiver: Node,
    bytes: &[u8],
    aliases: &UseAliases,
    resolver: &impl ClassFileResolver,
    classviews: &mut ClassViewCache,
    project_root: &Path,
) -> Option<(String, Confidence)> {
    let root = chain_root(receiver);
    if root.kind() == "scoped_call_expression" {
        let scope = root.child_by_field_name("scope")?;
        let (fqcn, confidence) = match scope.kind() {
            "name" | "qualified_name" => {
                let raw = scope.utf8_text(bytes).ok()?;
                (
                    qualify_fqcn(resolve_class_name(raw, aliases), scope, bytes),
                    Confidence::High,
                )
            }
            // `self::query()->…` / `static::query()->…` — the enclosing
            // class. `self` binds statically; `static` late-binds to the
            // runtime subclass, so the enclosing class is a lower bound.
            // `parent::` would need the parent FQCN from the hierarchy —
            // drops conservatively.
            "relative_scope" => {
                let raw = scope.utf8_text(bytes).ok()?;
                let fqcn = enclosing_class_fqcn(receiver, bytes)?;
                match raw {
                    "self" => (fqcn, Confidence::High),
                    "static" => (fqcn, Confidence::Medium),
                    _ => return None,
                }
            }
            _ => return None,
        };
        let file = resolver.class_file(&fqcn)?;
        let view = classviews.get_or_build(&fqcn, &file, project_root)?;
        let first = root
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(bytes).ok())?;
        let first_is_forwarding = crate::query_chain::methods::is_eloquent_static_starter(first)
            || matches!(
                classify_call(&view, first).map(|c| c.kind),
                Some(MagicMemberKind::Scope) | Some(MagicMemberKind::DynamicFinder)
            );
        if !first_is_forwarding {
            return None;
        }
        if has_relationship_link(receiver, bytes, &view) {
            return None;
        }
        return Some((fqcn, confidence));
    }
    if root.id() != receiver.id() {
        let (fqcn, confidence) =
            resolve_receiver(root, bytes, aliases, resolver, classviews, project_root)?;
        // Relation-hop bail, same reasoning as the static branch. A view
        // that can't build (e.g. the vendor Builder outside the fixture
        // graph) skips the check — classification downstream still gates.
        if let Some(file) = resolver.class_file(&fqcn) {
            if let Some(view) = classviews.get_or_build(&fqcn, &file, project_root) {
                if has_relationship_link(receiver, bytes, &view) {
                    return None;
                }
            }
        }
        let capped = match confidence {
            Confidence::High => Confidence::Medium,
            other => other,
        };
        return Some((fqcn, capped));
    }
    None
}

/// Does any link between the cursor's member and the chain root (exclusive)
/// name a relationship on `view`? For `$user->posts()->active()` with the
/// cursor on `active`, the receiver is the `posts()` call → links =
/// `["posts"]` → true when `posts` is a relationship.
fn has_relationship_link(
    receiver: Node,
    bytes: &[u8],
    view: &crate::laravel_introspector::chain::ClassView,
) -> bool {
    let mut cur = receiver;
    while matches!(
        cur.kind(),
        "member_call_expression"
            | "nullsafe_member_call_expression"
            | "member_access_expression"
            | "nullsafe_member_access_expression"
    ) {
        if let Some(name) = cur.child_by_field_name("name") {
            if let Ok(text) = name.utf8_text(bytes) {
                if view.relationships.iter().any(|r| r.method_name == text) {
                    return true;
                }
            }
        }
        match cur.child_by_field_name("object") {
            Some(o) => cur = o,
            None => break,
        }
    }
    false
}

/// Does this receiver chain root back to a parameter of the enclosing
/// `scope*` method? Gates the builder→enclosing-model retry to canonical
/// scope bodies (`scopeRecent(Builder $query) { $query->active() }`). A
/// Builder param belonging to an intervening closure (`whereHas('posts',
/// fn (Builder $q) => $q->published())`) is the related model's builder, not
/// the enclosing model's — those must not retry. (A closure param that
/// SHADOWS the scope's param name still slips through; accepted edge.)
fn is_scope_param_receiver(receiver: Node, bytes: &[u8]) -> bool {
    let root = chain_root(receiver);
    if root.kind() != "variable_name" {
        return false;
    }
    let Some(var) = root
        .utf8_text(bytes)
        .ok()
        .map(|t| t.trim_start_matches('$'))
    else {
        return false;
    };
    let mut cur = root.parent();
    while let Some(n) = cur {
        if n.kind() == "method_declaration" {
            let is_scope = n
                .child_by_field_name("name")
                .and_then(|x| x.utf8_text(bytes).ok())
                .is_some_and(|name| name.len() > "scope".len() && name.starts_with("scope"));
            return is_scope && method_has_param(n, bytes, var);
        }
        cur = n.parent();
    }
    false
}

/// Is `$var` one of `method`'s declared parameters?
fn method_has_param(method: Node, bytes: &[u8], var: &str) -> bool {
    let Some(params) = method.child_by_field_name("parameters") else {
        return false;
    };
    let mut stack = vec![params];
    while let Some(n) = stack.pop() {
        if n.kind() == "variable_name" {
            if let Ok(text) = n.utf8_text(bytes) {
                if text.trim_start_matches('$') == var {
                    return true;
                }
            }
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    false
}

/// The root expression of a call/access chain: descend through the `object`
/// field of member calls/accesses. `User::query()->where(…)->active()` → the
/// `User::query()` scoped call; `$q->where(…)->active()` → `$q`.
fn chain_root(receiver: Node) -> Node {
    let mut cur = receiver;
    while matches!(
        cur.kind(),
        "member_call_expression"
            | "nullsafe_member_call_expression"
            | "member_access_expression"
            | "nullsafe_member_access_expression"
    ) {
        match cur.child_by_field_name("object") {
            Some(o) => cur = o,
            None => break,
        }
    }
    cur
}

/// Is `fqcn` the Eloquent query builder (the type of a scope's `$query`
/// param)? The base `Query\Builder` is deliberately excluded — scopes don't
/// exist on it, and `DB::table(…)` chains must not retry against an
/// enclosing model they have nothing to do with.
fn is_eloquent_builder(fqcn: &str) -> bool {
    matches!(
        fqcn,
        "Illuminate\\Database\\Eloquent\\Builder"
            | "Illuminate\\Contracts\\Database\\Eloquent\\Builder"
    )
}

/// Resolve an arbitrary expression node to its class `(FQCN, confidence)` —
/// the public entry the view-variable inference uses to type a controller's
/// `view('x', ['user' => $expr])` values (and Volt `state`/`with`/`computed`).
///
/// First tries the flow chain classifier for inline Eloquent-producing
/// expressions (`User::all()`, `User::query()->first()`, `new User`) — the
/// dominant render-data shape. Falls back to the member-access receiver
/// resolution (bare variable via flow, `$this`, typed props, method returns,
/// auth helpers, …) for everything else.
pub fn resolve_expression_type(
    expr: Node,
    bytes: &[u8],
    aliases: &UseAliases,
    resolver: &impl ClassFileResolver,
    classviews: &mut ClassViewCache,
    project_root: &Path,
) -> Option<(String, Confidence)> {
    flow::resolve_expression(expr, bytes, aliases)
        .or_else(|| resolve_receiver(expr, bytes, aliases, resolver, classviews, project_root))
}

/// Resolve a receiver expression node to `(FQCN, confidence)`.
///
/// Handles bare variables (`$user`, via flow tracking, with a `foreach`
/// fallback), `$this` (the enclosing class), typed properties (`$this->prop`),
/// and method-call results via the method's `self`/`static` return type
/// (`$user->fresh()->…`). The index + ClassView cache are threaded through for
/// the return-type case, which has to read the called method's declared type.
fn resolve_receiver(
    receiver: Node,
    bytes: &[u8],
    aliases: &UseAliases,
    resolver: &impl ClassFileResolver,
    classviews: &mut ClassViewCache,
    project_root: &Path,
) -> Option<(String, Confidence)> {
    // Auth-helper receivers (`auth()->user()`, `Auth::user()`, `request()->
    // user()`) resolve to the configured auth user model — checked first
    // because they're specific call shapes the generic branches below would
    // otherwise mis-handle.
    if let Some(resolved) = resolve_auth_user_receiver(receiver, bytes, project_root) {
        return Some(resolved);
    }

    match receiver.kind() {
        "variable_name" => {
            let raw = receiver.utf8_text(bytes).ok()?;
            let var = raw.trim_start_matches('$');
            if var == "this" {
                // `$this` is the enclosing class — a certain resolution.
                enclosing_class_fqcn(receiver, bytes).map(|fqcn| (fqcn, Confidence::High))
            } else {
                // Flow tracking first (assignments / typed params / `@var`);
                // then a `foreach` element type; then a Gate-ability closure's
                // first param (the authenticatable, untyped by convention).
                flow::resolve_with_confidence(receiver, bytes, var, aliases)
                    .or_else(|| resolve_foreach_var(receiver, bytes, var, aliases))
                    .or_else(|| resolve_gate_closure_user(receiver, bytes, project_root))
            }
        }
        // `$this->prop` — a typed property on the enclosing class.
        "member_access_expression" | "nullsafe_member_access_expression" => {
            resolve_typed_property(receiver, bytes, aliases)
        }
        // `$obj->method()` — resolve via the method's return type.
        "member_call_expression" | "nullsafe_member_call_expression" => {
            resolve_method_return(receiver, bytes, aliases, resolver, classviews, project_root)
        }
        // `User::…` — an explicit class-name receiver (static call). Resolve
        // through the file's use-aliases, then qualify a bare same-namespace
        // name with the file's namespace (PHP name-resolution semantics — a
        // sibling model needs no import). A name the class graph still doesn't
        // know (an unimported alias, a facade proxying elsewhere) stays
        // unresolved rather than guessed.
        "name" | "qualified_name" => {
            let raw = receiver.utf8_text(bytes).ok()?;
            let fqcn = qualify_fqcn(resolve_class_name(raw, aliases), receiver, bytes);
            if resolver.class_file(&fqcn).is_some() {
                Some((fqcn, Confidence::High))
            } else {
                None
            }
        }
        // `self::…` / `static::…` — the enclosing class. `self` binds
        // statically (HIGH); `static` late-binds to the runtime subclass, so
        // the enclosing class is a lower bound (MEDIUM). `parent::` would
        // need the parent's FQCN from the hierarchy — drops conservatively.
        // The keyword kinds are matched alongside `relative_scope` because a
        // byte-range receiver lookup lands on the anonymous keyword TOKEN
        // inside the `relative_scope` node, not the node itself.
        "relative_scope" | "self" | "static" | "parent" => {
            let raw = receiver.utf8_text(bytes).ok()?;
            let fqcn = enclosing_class_fqcn(receiver, bytes)?;
            match raw {
                "self" => Some((fqcn, Confidence::High)),
                "static" => Some((fqcn, Confidence::Medium)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Resolve a `$user`-style receiver that is the first parameter of a Gate
/// ability closure (`Gate::define('x', function ($user) { … })`,
/// `Gate::before`/`after`, or the `gate()` helper form) to the auth user model.
///
/// Laravel contractually passes the authenticatable as the first argument to
/// these closures, so an *untyped* first param resolves with HIGH confidence.
/// This is the common `HorizonServiceProvider::gate()` shape that flow tracking
/// can't reach (no type hint, no assignment).
fn resolve_gate_closure_user(
    var_node: Node,
    bytes: &[u8],
    project_root: &Path,
) -> Option<(String, Confidence)> {
    let var = var_node.utf8_text(bytes).ok()?.trim_start_matches('$');
    let closure = enclosing_closure(var_node)?;
    if !is_first_param(closure, bytes, var) {
        return None;
    }
    if !is_gate_ability_closure(closure, bytes) {
        return None;
    }
    auth_model_fqcn(project_root).map(|m| (m, Confidence::High))
}

/// Nearest enclosing closure (`function () {}` / `fn () =>`) of `node`.
fn enclosing_closure(node: Node) -> Option<Node> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if matches!(
            n.kind(),
            "anonymous_function" | "anonymous_function_creation_expression" | "arrow_function"
        ) {
            return Some(n);
        }
        cur = n.parent();
    }
    None
}

/// Whether `var` names the first formal parameter of `closure`.
fn is_first_param(closure: Node, bytes: &[u8], var: &str) -> bool {
    let Some(params) = closure.child_by_field_name("parameters") else {
        return false;
    };
    let mut c = params.walk();
    let Some(first) = params
        .named_children(&mut c)
        .find(|p| p.kind() == "simple_parameter")
    else {
        return false;
    };
    first
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(bytes).ok())
        .map(|t| t.trim_start_matches('$') == var)
        .unwrap_or(false)
}

/// Whether `closure` is an argument to `Gate::define` / `before` / `after`
/// (facade) or the `gate()->define(...)` helper — the ability-definition calls
/// whose first closure param is the authenticatable.
fn is_gate_ability_closure(closure: Node, bytes: &[u8]) -> bool {
    // Step out through any argument / arguments wrappers to the enclosing call.
    let mut node = closure;
    let call = loop {
        let Some(p) = node.parent() else {
            return false;
        };
        if matches!(p.kind(), "argument" | "arguments") {
            node = p;
            continue;
        }
        break p;
    };
    let name = call
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(bytes).ok());
    if !matches!(name, Some("define" | "before" | "after")) {
        return false;
    }
    match call.kind() {
        "scoped_call_expression" => call
            .child_by_field_name("scope")
            .and_then(|s| s.utf8_text(bytes).ok())
            .map(|s| s.rsplit('\\').next().unwrap_or(s) == "Gate")
            .unwrap_or(false),
        "member_call_expression" | "nullsafe_member_call_expression" => call
            .child_by_field_name("object")
            .map(|o| {
                o.kind() == "function_call_expression"
                    && o.child_by_field_name("function")
                        .and_then(|f| f.utf8_text(bytes).ok())
                        == Some("gate")
            })
            .unwrap_or(false),
        _ => false,
    }
}

/// Resolve an auth-helper receiver to the configured auth user model:
/// `auth()->user()`, `request()->user()` (member calls on the `auth()` /
/// `request()` helpers) and `Auth::user()` (the facade). These are the
/// dominant way authenticated-user attributes are reached in real code, and
/// the user model is well-known, so they resolve at HIGH confidence.
fn resolve_auth_user_receiver(
    receiver: Node,
    bytes: &[u8],
    project_root: &Path,
) -> Option<(String, Confidence)> {
    match receiver.kind() {
        // `Auth::user()` / `\Illuminate\Support\Facades\Auth::user()`
        "scoped_call_expression" => {
            let name = receiver
                .child_by_field_name("name")?
                .utf8_text(bytes)
                .ok()?;
            if name != "user" {
                return None;
            }
            let scope = receiver
                .child_by_field_name("scope")?
                .utf8_text(bytes)
                .ok()?;
            let base = scope.rsplit('\\').next().unwrap_or(scope);
            if base != "Auth" {
                return None;
            }
            auth_model_fqcn(project_root).map(|m| (m, Confidence::High))
        }
        // `auth()->user()` / `request()->user()`
        "member_call_expression" | "nullsafe_member_call_expression" => {
            let name = receiver
                .child_by_field_name("name")?
                .utf8_text(bytes)
                .ok()?;
            if name != "user" {
                return None;
            }
            let object = receiver.child_by_field_name("object")?;
            if object.kind() != "function_call_expression" {
                return None;
            }
            let func = object
                .child_by_field_name("function")?
                .utf8_text(bytes)
                .ok()?;
            if func == "auth" || func == "request" {
                auth_model_fqcn(project_root).map(|m| (m, Confidence::High))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Resolve a `$obj->method()` receiver via the called method's return type.
///
/// Scoped to `self` / `static` return types — the canonical fluent /
/// return-`$this` shape (`$user->activated()->email`) — which resolve to the
/// object's own class. Arbitrary class return types are not resolved here:
/// the return type is written in the *declaring* file's namespace, which this
/// caller-side context can't reliably re-qualify. Vendor methods (`fresh`,
/// `refresh`) aren't covered either — the ClassView walk stops at
/// `Eloquent\Model`. Return-type inference is indirect → [`Confidence::Medium`].
fn resolve_method_return(
    call: Node,
    bytes: &[u8],
    aliases: &UseAliases,
    resolver: &impl ClassFileResolver,
    classviews: &mut ClassViewCache,
    project_root: &Path,
) -> Option<(String, Confidence)> {
    let object = call.child_by_field_name("object")?;
    let name_node = call.child_by_field_name("name")?;
    if name_node.kind() != "name" {
        return None;
    }
    let method = name_node.utf8_text(bytes).ok()?;
    let (obj_fqcn, _) =
        resolve_receiver(object, bytes, aliases, resolver, classviews, project_root)?;
    let file_path = resolver.class_file(&obj_fqcn)?;
    let view = classviews.get_or_build(&obj_fqcn, &file_path, project_root)?;
    let ret = method_return_type(&view, method)?;
    match normalize_type(&ret)?.as_str() {
        "self" | "static" => Some((obj_fqcn, Confidence::Medium)),
        _ => None,
    }
}

/// The declared return type of `method` on `view` (raw form preferred so
/// `self`/`static` survive), searching the inheritance-resolved method set.
fn method_return_type(view: &ClassView, method: &str) -> Option<String> {
    view.all_methods
        .iter()
        .find(|m| m.value.name == method)
        .and_then(|m| {
            m.value
                .return_type_raw
                .clone()
                .or_else(|| m.value.return_type.clone())
        })
}

/// Resolve a `$this->prop` receiver via the declared type of `prop` on the
/// enclosing class — both ordinary typed properties (`private User $prop;`) and
/// constructor-promoted ones (`public function __construct(private User $prop)`).
///
/// An explicitly declared type is as certain as a typed parameter, so this is
/// [`Confidence::High`]. Only `$this->prop` is handled (the runtime class of
/// `$other->prop` would itself need resolving first); union / intersection
/// types are skipped as ambiguous.
fn resolve_typed_property(
    receiver: Node,
    bytes: &[u8],
    aliases: &UseAliases,
) -> Option<(String, Confidence)> {
    let object = receiver.child_by_field_name("object")?;
    if object.kind() != "variable_name" || object.utf8_text(bytes).ok()? != "$this" {
        return None;
    }
    let name_node = receiver.child_by_field_name("name")?;
    if name_node.kind() != "name" {
        return None;
    }
    let prop = name_node.utf8_text(bytes).ok()?;
    let class = enclosing_class_node(receiver)?;
    let raw_type = property_type_in_class(class, bytes, prop)?;
    let normalized = normalize_type(&raw_type)?;
    let resolved = resolve_class_name(&normalized, aliases);
    Some((qualify_fqcn(resolved, receiver, bytes), Confidence::High))
}

/// Turn a resolved type name into a fully-qualified one. `resolve_class_name`
/// expands `use`-aliases and absolute (`\Foo`) names, but leaves a bare
/// same-namespace name unqualified — so qualify those with the file's
/// namespace (matching how the class-hierarchy index keys its FQCNs).
///
/// TODO: a namespace-RELATIVE qualified name (`Models\User` inside
/// `namespace App;`) is treated as already-qualified and won't resolve to
/// `App\Models\User` — a false negative both the static-receiver arm and the
/// chain-root arm inherit.
fn qualify_fqcn(name: String, node: Node, bytes: &[u8]) -> String {
    let trimmed = name.trim_start_matches('\\').to_string();
    if trimmed.contains('\\') {
        // Already namespaced (alias-resolved or absolute).
        trimmed
    } else if let Some(ns) = file_namespace(node, bytes) {
        format!("{ns}\\{trimmed}")
    } else {
        trimmed
    }
}

/// Find the declared type of property `$prop` on `class` — scanning both
/// `property_declaration`s and constructor-promoted parameters. Does not
/// descend into nested (anonymous) classes.
fn property_type_in_class(class: Node, bytes: &[u8], prop: &str) -> Option<String> {
    let mut stack = vec![class];
    while let Some(n) = stack.pop() {
        // Don't leak into a nested class's members.
        if n.id() != class.id() && matches!(n.kind(), "class_declaration" | "anonymous_class") {
            continue;
        }

        if n.kind() == "property_declaration" {
            if let Some(ty) = n.child_by_field_name("type") {
                let mut c = n.walk();
                let matches_name = n.children(&mut c).any(|child| {
                    child.kind() == "property_element"
                        && child
                            .child_by_field_name("name")
                            .and_then(|nm| nm.utf8_text(bytes).ok())
                            .map(|t| t.trim_start_matches('$') == prop)
                            .unwrap_or(false)
                });
                if matches_name {
                    return ty.utf8_text(bytes).ok().map(str::to_string);
                }
            }
        }

        if n.kind() == "property_promotion_parameter" {
            let is_match = n
                .child_by_field_name("name")
                .and_then(|nm| nm.utf8_text(bytes).ok())
                .map(|t| t.trim_start_matches('$') == prop)
                .unwrap_or(false);
            if is_match {
                if let Some(ty) = n.child_by_field_name("type") {
                    return ty.utf8_text(bytes).ok().map(str::to_string);
                }
            }
        }

        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    None
}

/// Normalize a declared type to a single resolvable class name: strip a
/// leading `?` (nullable), reject union / intersection types as ambiguous.
fn normalize_type(raw: &str) -> Option<String> {
    let t = raw.trim().trim_start_matches('?').trim();
    if t.is_empty() || t.contains('|') || t.contains('&') {
        return None;
    }
    Some(t.to_string())
}

/// Resolve a `foreach ($coll as $var)` value variable to its element type.
///
/// The element type is the model the collection operates on — `flow::resolve`
/// already gives that for a collection variable (`$users = User::all()` →
/// `User`), and the element of that collection is a `User`. Inferring an
/// element from a collection is indirect, so this is [`Confidence::Medium`].
/// (A `@var User $var` on the loop is found by flow directly, before this
/// fallback runs.)
fn resolve_foreach_var(
    use_site: Node,
    bytes: &[u8],
    var: &str,
    aliases: &UseAliases,
) -> Option<(String, Confidence)> {
    let mut cur = use_site.parent();
    while let Some(n) = cur {
        if n.kind() == "foreach_statement" {
            if let Some((collection, value_var)) = foreach_parts(n, bytes) {
                if value_var == var {
                    // Only a collection *variable* is resolvable here; flow
                    // tracks its model type.
                    if collection.kind() == "variable_name" {
                        let cvar = collection.utf8_text(bytes).ok()?.trim_start_matches('$');
                        if let Some(fqcn) = flow::resolve(collection, bytes, cvar, aliases) {
                            return Some((fqcn, Confidence::Medium));
                        }
                    }
                    // Matched the binding but couldn't resolve the collection.
                    return None;
                }
            }
        }
        cur = n.parent();
    }
    None
}

/// Extract `(collection_expr, value_var_name)` from a `foreach_statement`.
/// Handles `foreach ($c as $v)` and `foreach ($c as $k => $v)`; list
/// destructuring (`as [$a, $b]`) is not handled.
fn foreach_parts<'t>(foreach: Node<'t>, bytes: &[u8]) -> Option<(Node<'t>, String)> {
    let body_id = foreach.child_by_field_name("body").map(|b| b.id());
    let mut named = Vec::new();
    let mut c = foreach.walk();
    for ch in foreach.named_children(&mut c) {
        if Some(ch.id()) == body_id {
            continue;
        }
        named.push(ch);
    }
    if named.len() < 2 {
        return None;
    }
    let collection = named[0];
    let binding = named[named.len() - 1];
    let value_var = match binding.kind() {
        "variable_name" => binding
            .utf8_text(bytes)
            .ok()?
            .trim_start_matches('$')
            .to_string(),
        "pair" => {
            // `$key => $value` — the value is the pair's last named child.
            let mut pc = binding.walk();
            let kids: Vec<_> = binding.named_children(&mut pc).collect();
            let last = kids.last()?;
            if last.kind() != "variable_name" {
                return None;
            }
            last.utf8_text(bytes)
                .ok()?
                .trim_start_matches('$')
                .to_string()
        }
        _ => return None,
    };
    Some((collection, value_var))
}

/// FQCN of the class lexically enclosing `node`, or `None` when `node` isn't
/// inside a class (e.g. a free function, or a `$this` inside a trait — whose
/// runtime class is unknowable statically).
fn enclosing_class_fqcn(node: Node, bytes: &[u8]) -> Option<String> {
    let class = enclosing_class_node(node)?;
    let class_name = class
        .child_by_field_name("name")
        .and_then(|x| x.utf8_text(bytes).ok())?;
    match file_namespace(node, bytes) {
        Some(ns) => Some(format!("{ns}\\{class_name}")),
        None => Some(class_name.to_string()),
    }
}

/// The class-like node lexically enclosing `node`, if any — a named
/// `class_declaration` or an `anonymous_class` (Volt SFC `new class extends
/// Component`). Matching anonymous classes lets `$this->prop` typed-property
/// resolution work inside Volt components (the class has no FQCN, but its
/// property declarations carry types just the same).
fn enclosing_class_node(node: Node) -> Option<Node> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if matches!(n.kind(), "class_declaration" | "anonymous_class") {
            return Some(n);
        }
        cur = n.parent();
    }
    None
}

/// The file's `namespace ...;` declaration, if any. Walks to the tree root and
/// finds the first `namespace_definition`.
fn file_namespace(node: Node, bytes: &[u8]) -> Option<String> {
    let mut root = node;
    while let Some(p) = root.parent() {
        root = p;
    }
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "namespace_definition" {
            if let Some(nn) = n.child_by_field_name("name") {
                return nn.utf8_text(bytes).ok().map(str::to_string);
            }
            // Fallback: the first `namespace_name` child (field name varies
            // across grammar versions).
            let mut c = n.walk();
            let name_node = n.children(&mut c).find(|ch| ch.kind() == "namespace_name");
            if let Some(nn) = name_node {
                return nn.utf8_text(bytes).ok().map(str::to_string);
            }
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    None
}

/// The configured auth user model FQCN for `project_root`, parsed from
/// `config/auth.php`'s `providers.users.model`. Memoized per project root for
/// the process lifetime — the auth model effectively never changes during a
/// session, so we don't re-read the file on every receiver resolution.
///
/// (If `config/auth.php` is edited mid-session the cached value goes stale
/// until restart — an acceptable tradeoff for a value this stable.)
fn auth_model_fqcn(project_root: &Path) -> Option<String> {
    static MEMO: once_cell::sync::Lazy<std::sync::Mutex<HashMap<PathBuf, Option<String>>>> =
        once_cell::sync::Lazy::new(|| std::sync::Mutex::new(HashMap::new()));

    if let Ok(memo) = MEMO.lock() {
        if let Some(cached) = memo.get(project_root) {
            return cached.clone();
        }
    }
    let resolved = std::fs::read_to_string(project_root.join("config/auth.php"))
        .ok()
        .and_then(|content| parse_auth_model(&content));
    if let Ok(mut memo) = MEMO.lock() {
        memo.insert(project_root.to_path_buf(), resolved.clone());
    }
    resolved
}

/// Extract `providers.users.model` from `config/auth.php` source. Resolves the
/// class reference (`User::class`, `env('AUTH_MODEL', User::class)`, or a
/// fully-qualified `\App\Models\User::class`) through the file's `use` aliases.
/// Tree-sitter parsing means commented-out providers are ignored. Returns the
/// first `'model'` entry in source order — the default user provider.
fn parse_auth_model(content: &str) -> Option<String> {
    let tree = parse_php(content).ok()?;
    let bytes = content.as_bytes();
    let aliases = extract_use_aliases(&tree, content);

    let mut best: Option<(usize, String)> = None;
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if n.kind() == "array_element_initializer" {
            let mut c = n.walk();
            let kids: Vec<_> = n.named_children(&mut c).collect();
            if kids.len() == 2 {
                let key = kids[0].utf8_text(bytes).ok()?.trim_matches(['\'', '"']);
                if key == "model" {
                    if let Some(class_ref) = first_class_const(kids[1], bytes) {
                        let fqcn = resolve_class_name(&class_ref, &aliases)
                            .trim_start_matches('\\')
                            .to_string();
                        let pos = n.start_byte();
                        if best.as_ref().is_none_or(|(p, _)| pos < *p) {
                            best = Some((pos, fqcn));
                        }
                    }
                }
            }
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    best.map(|(_, fqcn)| fqcn)
}

/// First `X::class` class reference in `node`'s subtree → the class name `X`
/// (handles both a bare `User::class` and one wrapped in `env(..., User::class)`).
fn first_class_const(node: Node, bytes: &[u8]) -> Option<String> {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == "class_constant_access_expression" {
            let scope = n.named_child(0)?;
            return scope.utf8_text(bytes).ok().map(str::to_string);
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    None
}

/// Recognize Eloquent dynamic finders: `where{Column}` / `orWhere{Column}`
/// where `{Column}` is a StudlyCase column in the model's column surface.
///
/// Multi-segment finders (`whereEmailAndStatus`) are not handled — only the
/// single-column form, which covers the overwhelming majority of real usage.
fn classify_dynamic_finder(view: &ClassView, member: &str) -> Option<ClassifiedMember> {
    let column = dynamic_finder_column(member)?;
    if view.column_surface.iter().any(|c| c.name == column) {
        return Some(ClassifiedMember {
            declaring_fqcn: view.fqcn.clone(),
            kind: MagicMemberKind::DynamicFinder,
        });
    }
    None
}

/// The column a dynamic finder name targets: `whereEmail` / `orWhereEmail` →
/// `email`. `None` when the name isn't finder-shaped (`where`, `whereabouts`).
/// Shared by classification and the goto/hover dispatch (a finder has no
/// declaring method — its definition site is the column's migration line).
pub fn dynamic_finder_column(member: &str) -> Option<String> {
    let rest = member
        .strip_prefix("where")
        .or_else(|| member.strip_prefix("orWhere"))?;
    // Must have a StudlyCase remainder — guards against `where`/`whereabouts`.
    if !rest.chars().next()?.is_ascii_uppercase() {
        return None;
    }
    Some(pascal_to_snake(rest))
}

#[cfg(test)]
mod tests;
