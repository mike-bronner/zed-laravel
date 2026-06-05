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
use crate::query_chain::flow;
use crate::query_chain::use_aliases::{resolve_class_name, UseAliases};
use crate::salsa_impl::{Confidence, MagicMemberKind};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tree_sitter::Node;

/// How a member was syntactically accessed. Drives which magic kinds are even
/// possible: a scope is only reachable via a call, an accessor only via a
/// property read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessForm {
    /// `$user->email` — property read (no call parens).
    Property,
    /// `User::active()` — static call (`::`).
    StaticCall,
    /// `$user->active()` / `$user->posts()` — instance method call (`->m()`).
    InstanceCall,
}

impl AccessForm {
    /// Call-form (`::m()` or `->m()`) vs property read.
    fn is_call(self) -> bool {
        matches!(self, AccessForm::StaticCall | AccessForm::InstanceCall)
    }
}

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
/// receiver was resolved. This is what M4 writes into the M2 scaffold.
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
#[allow(clippy::too_many_arguments)]
pub fn resolve_and_classify(
    receiver: Node,
    member: &str,
    form: AccessForm,
    bytes: &[u8],
    aliases: &UseAliases,
    index: &ClassHierarchyIndex,
    classviews: &mut ClassViewCache,
    project_root: &Path,
) -> Option<ResolvedMemberAccess> {
    let (fqcn, confidence) = resolve_receiver(receiver, bytes, aliases)?;
    let file_path = index.get(&fqcn)?.file_path.clone();
    let view = classviews.get_or_build(&fqcn, &file_path, project_root)?;
    let classified = classify_member(&view, member, form)?;
    Some(ResolvedMemberAccess {
        declaring_fqcn: classified.declaring_fqcn,
        kind: classified.kind,
        confidence,
    })
}

/// Resolve a receiver expression node to `(FQCN, confidence)`.
///
/// Handles bare variables (`$user`, via flow tracking) and `$this` (the
/// enclosing class). Later M3 commits widen this to typed properties
/// (`$this->prop`), `foreach` iterator vars, and method return-type chains.
fn resolve_receiver(
    receiver: Node,
    bytes: &[u8],
    aliases: &UseAliases,
) -> Option<(String, Confidence)> {
    match receiver.kind() {
        "variable_name" => {
            let raw = receiver.utf8_text(bytes).ok()?;
            let var = raw.trim_start_matches('$');
            if var == "this" {
                // `$this` is the enclosing class — a certain resolution.
                enclosing_class_fqcn(receiver, bytes).map(|fqcn| (fqcn, Confidence::High))
            } else {
                flow::resolve_with_confidence(receiver, bytes, var, aliases)
            }
        }
        // `$this->prop` — a typed property on the enclosing class.
        "member_access_expression" | "nullsafe_member_access_expression" => {
            resolve_typed_property(receiver, bytes, aliases)
        }
        _ => None,
    }
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

/// The `class_declaration` node lexically enclosing `node`, if any.
fn enclosing_class_node(node: Node) -> Option<Node> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if n.kind() == "class_declaration" {
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

/// Recognize Eloquent dynamic finders: `where{Column}` / `orWhere{Column}`
/// where `{Column}` is a StudlyCase column in the model's column surface.
///
/// Multi-segment finders (`whereEmailAndStatus`) are not handled — only the
/// single-column form, which covers the overwhelming majority of real usage.
fn classify_dynamic_finder(view: &ClassView, member: &str) -> Option<ClassifiedMember> {
    let rest = member
        .strip_prefix("where")
        .or_else(|| member.strip_prefix("orWhere"))?;
    // Must have a StudlyCase remainder — guards against `where`/`whereabouts`.
    if !rest.chars().next()?.is_ascii_uppercase() {
        return None;
    }
    let column = pascal_to_snake(rest);
    if view.column_surface.iter().any(|c| c.name == column) {
        return Some(ClassifiedMember {
            declaring_fqcn: view.fqcn.clone(),
            kind: MagicMemberKind::DynamicFinder,
        });
    }
    None
}

#[cfg(test)]
mod tests;
