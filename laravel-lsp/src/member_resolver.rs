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

use crate::laravel_introspector::chain::ClassView;
use crate::laravel_introspector::model_metadata::pascal_to_snake;
use crate::salsa_impl::MagicMemberKind;

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
