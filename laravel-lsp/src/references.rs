//! Cross-file reference finding for Laravel patterns.
//!
//! [`Backend::references`](super) classifies the parser-tagged pattern at the
//! cursor into a [`SymbolRef`], then asks the Salsa actor for every other
//! position the parser tagged with the same kind + name. Random PHP string
//! literals that happen to share the shape are never returned — this is the
//! "follow the instance chain" rule that distinguishes this from a global
//! grep-and-rename.

use crate::salsa_impl::{ParsedPatternsData, PatternAtPosition, SymbolRefData};

/// Classified symbol under the cursor. Mirrors [`SymbolRefData`] but stays on
/// the LSP-handler side — converted into the data variant only when crossing
/// the Salsa actor boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SymbolRef {
    View(String),
    Route(String),
    Config(String),
    Translation(String),
    Env(String),
    Component(String),
    Livewire(String),
    Middleware(String),
    Binding(String),
}

impl SymbolRef {
    /// The display name of the symbol, used in test assertions and logs.
    pub fn name(&self) -> &str {
        match self {
            SymbolRef::View(n)
            | SymbolRef::Route(n)
            | SymbolRef::Config(n)
            | SymbolRef::Translation(n)
            | SymbolRef::Env(n)
            | SymbolRef::Component(n)
            | SymbolRef::Livewire(n)
            | SymbolRef::Middleware(n)
            | SymbolRef::Binding(n) => n,
        }
    }

    /// Convert into the Salsa-side data-transfer variant.
    pub fn to_data(&self) -> SymbolRefData {
        match self {
            SymbolRef::View(n) => SymbolRefData::View(n.clone()),
            SymbolRef::Route(n) => SymbolRefData::Route(n.clone()),
            SymbolRef::Config(n) => SymbolRefData::Config(n.clone()),
            SymbolRef::Translation(n) => SymbolRefData::Translation(n.clone()),
            SymbolRef::Env(n) => SymbolRefData::Env(n.clone()),
            SymbolRef::Component(n) => SymbolRefData::Component(n.clone()),
            SymbolRef::Livewire(n) => SymbolRefData::Livewire(n.clone()),
            SymbolRef::Middleware(n) => SymbolRefData::Middleware(n.clone()),
            SymbolRef::Binding(n) => SymbolRefData::Binding(n.clone()),
        }
    }
}

/// Decide what classified symbol (if any) the cursor sits on. The cursor must
/// be on a position the parser has tagged with one of the supported pattern
/// kinds — we never fall back to raw string-shape matching for variable names
/// or other untyped tokens (that requires real scope analysis; see Phase 3e/3f
/// of the implementation plan).
///
/// Directive patterns (`@include('users.profile')`) are mapped to their
/// underlying view reference where applicable, so the cursor in either the
/// directive name or its argument flows through the same view rename path.
pub fn classify_pattern_at_cursor(
    patterns: &ParsedPatternsData,
    line: u32,
    column: u32,
) -> Option<SymbolRef> {
    let pattern = patterns.find_at_position(line, column)?;
    match pattern {
        PatternAtPosition::View(v) => Some(SymbolRef::View(v.name.clone())),
        PatternAtPosition::Route(r) => Some(SymbolRef::Route(r.name.clone())),
        PatternAtPosition::ConfigRef(c) => Some(SymbolRef::Config(c.key.clone())),
        PatternAtPosition::Translation(t) => Some(SymbolRef::Translation(t.key.clone())),
        PatternAtPosition::EnvRef(e) => Some(SymbolRef::Env(e.name.clone())),
        PatternAtPosition::Component(c) => Some(SymbolRef::Component(c.name.clone())),
        PatternAtPosition::Livewire(l) => Some(SymbolRef::Livewire(l.name.clone())),
        PatternAtPosition::Middleware(m) => Some(SymbolRef::Middleware(m.name.clone())),
        PatternAtPosition::Binding(b) => Some(SymbolRef::Binding(b.name.clone())),
        PatternAtPosition::Directive(d) => {
            let args = d.arguments.as_deref()?;
            // `@livewire('counter')` directive form. Carried in the
            // directive bucket because the .scm captures it as a generic
            // directive node, not as a Livewire pattern. Classify it as
            // a Livewire symbol so rename / find-references / goto match
            // the `<livewire:...>` tag form.
            if d.name == "livewire" {
                let name = directive_first_string_arg(args)?;
                return Some(SymbolRef::Livewire(name));
            }
            // @include('users.profile') and friends carry the view name in the
            // arguments. The classifier surfaces it as a view symbol so the
            // user can find every other place that view is referenced.
            let view_name = directive_view_name(&d.name, args)?;
            Some(SymbolRef::View(view_name))
        }
        // Patterns that don't yet participate in cross-file references.
        // `MemberAccess` is captured (M2) but not yet resolved/classified —
        // it gets armed for find-references in M4.
        PatternAtPosition::Asset(_)
        | PatternAtPosition::Url(_)
        | PatternAtPosition::Action(_)
        | PatternAtPosition::Feature(_)
        | PatternAtPosition::MemberAccess(_) => None,
    }
}

/// Extract the view name carried by a Blade directive that takes a view
/// argument (`@include`, `@extends`, `@component`, `@each`, plus the
/// conditional variants). Returns `None` for directives that don't reference
/// views or whose argument couldn't be parsed as a single string literal.
fn directive_view_name(name: &str, args: &str) -> Option<String> {
    if !matches!(
        name,
        "include" | "extends" | "component" | "each" | "includeIf" | "includeWhen"
    ) {
        return None;
    }
    directive_first_string_arg(args)
}

/// Extract the first string argument from a directive's parenthesized
/// argument list. Handles `@livewire('counter')`, `@include('view')`, etc.
/// Returns `None` when the args can't be parsed as a single quoted
/// string at the head position.
fn directive_first_string_arg(args: &str) -> Option<String> {
    let trimmed = args.trim().trim_matches('(').trim_matches(')').trim();
    // First comma-separated argument; trim quotes.
    let first = trimmed.split(',').next()?.trim();
    let unquoted = first.trim_matches('\'').trim_matches('"');
    if unquoted.is_empty() {
        None
    } else {
        Some(unquoted.to_string())
    }
}

#[cfg(test)]
mod tests;
