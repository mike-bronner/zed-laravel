//! Reference-count code lens targets (#59).
//!
//! Given an open PHP/Blade file, enumerate the *declaration* sites of symbols
//! whose references this server counts accurately — model magic members
//! (relationships, scopes, accessors, public properties) and Livewire/Volt
//! component members (`#[Computed]` methods + public properties). Each target
//! carries the [`SymbolRefData`] key the reverse index counts by; the LSP
//! `code_lens` handler turns these into lenses and `code_lens/resolve` fills in
//! the count.
//!
//! Deliberately scoped to accurately-counted symbols: plain method *calls* and
//! class references aren't indexed, so they get no lens (a generic PHP LSP
//! covers those). Laravel literal definitions (routes/views/…) are a follow-up.

use std::path::Path;

use tree_sitter::Node;

use crate::parser::parse_php;
use crate::salsa_impl::SymbolRefData;

/// One code-lens anchor: the symbol-name position (0-based) and the index key
/// its reference count is looked up under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeLensTarget {
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
    pub symbol: SymbolRefData,
}

const RELATIONSHIP_METHODS: &[&str] = &[
    "hasMany",
    "hasOne",
    "belongsTo",
    "belongsToMany",
    "morphTo",
    "morphMany",
    "morphOne",
    "morphToMany",
    "morphedByMany",
    "hasManyThrough",
    "hasOneThrough",
];

/// Build the code-lens targets for `path`/`source`.
pub fn code_lens_targets(path: &Path, source: &str) -> Vec<CodeLensTarget> {
    let is_blade = path.to_string_lossy().ends_with(".blade.php");

    // A component class lives either in this `.php` (inline anonymous class) or
    // in this `.blade.php`'s own Volt front-matter (SFC). An MFC `.blade.php`
    // template carries no class — its members are lensed on the sibling `.php`.
    if is_blade {
        if crate::livewire_resolver::source_contains_volt_signature(source) {
            if let Some((front, base_line)) = front_matter(source) {
                let key = format!("volt::{}", path.display());
                return component_targets(front, &key, base_line);
            }
        }
        return Vec::new();
    }

    if crate::php_class::detect_inline_livewire_class(source) {
        let key = format!("volt::{}", path.display());
        return component_targets(source, &key, 0);
    }

    model_targets(source)
}

/// The leading `<?php … ?>` front-matter and the 0-based line it starts on.
fn front_matter(source: &str) -> Option<(&str, u32)> {
    let start = source.find("<?php")?;
    let base_line = source[..start].matches('\n').count() as u32;
    let after = &source[start..];
    let block = match after.find("?>") {
        Some(end) => &after[..end + 2],
        None => after,
    };
    Some((block, base_line))
}

/// Component members read via `$this->` (and bare `$prop`): `#[Computed]`
/// methods + public properties. Action methods (called via `wire:click`, not a
/// member access) are skipped — we don't index those, so a lens would wrongly
/// read "0 references".
fn component_targets(source: &str, key: &str, base_line: u32) -> Vec<CodeLensTarget> {
    let Ok(tree) = parse_php(source) else {
        return Vec::new();
    };
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "method_declaration" if has_attribute(n, bytes, "Computed") => {
                if let Some((name, line, col, end)) = name_position(n, bytes) {
                    out.push(target(key, &name, line + base_line, col, end));
                }
            }
            "property_declaration" if is_public(n, bytes) => {
                for (name, line, col, end) in property_names(n, bytes) {
                    out.push(target(key, &name, line + base_line, col, end));
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

/// Model magic members: relationships, scopes, and public properties (column /
/// attribute reads). Keyed by the file's class FQCN + the *usage* name.
fn model_targets(source: &str) -> Vec<CodeLensTarget> {
    let Ok(tree) = parse_php(source) else {
        return Vec::new();
    };
    let bytes = source.as_bytes();
    let Some(fqcn) = first_class_fqcn(tree.root_node(), bytes) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "method_declaration" => {
                if let Some((name, line, col, end)) = name_position(n, bytes) {
                    if let Some(usage) = scope_usage_name(&name) {
                        out.push(target(&fqcn, &usage, line, col, end));
                    } else if let Some(usage) = accessor_usage_name(n, &name, bytes) {
                        out.push(target(&fqcn, &usage, line, col, end));
                    } else if method_is_relationship(n, bytes) {
                        out.push(target(&fqcn, &name, line, col, end));
                    }
                }
            }
            "property_declaration" if is_public(n, bytes) => {
                for (name, line, col, end) in property_names(n, bytes) {
                    out.push(target(&fqcn, &name, line, col, end));
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

fn target(fqcn: &str, member: &str, line: u32, column: u32, end_column: u32) -> CodeLensTarget {
    CodeLensTarget {
        line,
        column,
        end_column,
        symbol: SymbolRefData::MagicMember {
            fqcn: fqcn.to_string(),
            member: member.to_string(),
        },
    }
}

/// `scopeActive` → `Some("active")`; not a scope → `None`.
fn scope_usage_name(method: &str) -> Option<String> {
    let rest = method.strip_prefix("scope")?;
    let mut chars = rest.chars();
    let first = chars.next()?;
    if !first.is_ascii_uppercase() {
        return None;
    }
    Some(format!("{}{}", first.to_ascii_lowercase(), chars.as_str()))
}

/// The attribute name an accessor method exposes, matching the reverse index's
/// keying (`getFullNameAttribute` / `fullName(): Attribute` → `full_name`), or
/// `None` if the method isn't an accessor. Mirrors `chain::compute_accessors`.
fn accessor_usage_name(method: Node, name: &str, bytes: &[u8]) -> Option<String> {
    use crate::laravel_introspector::model_metadata::pascal_to_snake;
    // Old-style: `get{Middle}Attribute`.
    if let Some(middle) = name
        .strip_prefix("get")
        .and_then(|s| s.strip_suffix("Attribute"))
    {
        if !middle.is_empty() {
            return Some(pascal_to_snake(middle));
        }
    }
    // New-style: any method returning `Attribute` (possibly namespaced/nullable).
    let returns_attribute = method
        .child_by_field_name("return_type")
        .and_then(|rt| rt.utf8_text(bytes).ok())
        .map(|t| {
            t.trim()
                .trim_start_matches('?')
                .rsplit('\\')
                .next()
                .unwrap_or("")
                .trim()
                == "Attribute"
        })
        .unwrap_or(false);
    returns_attribute.then(|| pascal_to_snake(name))
}

/// True if a method body calls an Eloquent relationship factory
/// (`$this->hasMany(...)`, `belongsTo(...)`, …).
fn method_is_relationship(method: Node, bytes: &[u8]) -> bool {
    let text = method.utf8_text(bytes).unwrap_or("");
    RELATIONSHIP_METHODS
        .iter()
        .any(|m| text.contains(&format!("->{m}(")))
}

/// The `name` node's text + 0-based (row, start col, end col).
fn name_position(node: Node, bytes: &[u8]) -> Option<(String, u32, u32, u32)> {
    let name = node.child_by_field_name("name")?;
    let text = name.utf8_text(bytes).ok()?.to_string();
    let start = name.start_position();
    let end = name.end_position();
    Some((
        text,
        start.row as u32,
        start.column as u32,
        end.column as u32,
    ))
}

/// Each `property_element`'s name (stripped of `$`) + 0-based position.
fn property_names(node: Node, bytes: &[u8]) -> Vec<(String, u32, u32, u32)> {
    let mut out = Vec::new();
    let mut c = node.walk();
    for ch in node.children(&mut c) {
        if ch.kind() != "property_element" {
            continue;
        }
        if let Some(nm) = ch.child_by_field_name("name") {
            if let Ok(raw) = nm.utf8_text(bytes) {
                let start = nm.start_position();
                let end = nm.end_position();
                // Skip the leading `$` so the lens anchors on the name.
                let dollar = if raw.starts_with('$') { 1 } else { 0 };
                out.push((
                    raw.trim_start_matches('$').to_string(),
                    start.row as u32,
                    start.column as u32 + dollar,
                    end.column as u32,
                ));
            }
        }
    }
    out
}

fn is_public(node: Node, bytes: &[u8]) -> bool {
    let mut c = node.walk();
    // A property with no visibility modifier defaults to public in modern PHP
    // only via promotion; an explicit non-public modifier excludes it.
    let mut saw_modifier = false;
    let mut public = false;
    for ch in node.children(&mut c) {
        if ch.kind() == "visibility_modifier" {
            saw_modifier = true;
            if ch.utf8_text(bytes).map(|t| t == "public").unwrap_or(false) {
                public = true;
            }
        }
    }
    public || !saw_modifier
}

fn has_attribute(node: Node, bytes: &[u8], name: &str) -> bool {
    let mut c = node.walk();
    let mut found = false;
    for ch in node.children(&mut c) {
        if ch.kind() == "attribute_list"
            && ch
                .utf8_text(bytes)
                .map(|t| t.contains(name))
                .unwrap_or(false)
        {
            found = true;
        }
    }
    found
}

/// The FQCN of the first named class in the tree (`namespace` + class name).
fn first_class_fqcn(root: Node, bytes: &[u8]) -> Option<String> {
    let mut namespace: Option<String> = None;
    let mut class_name: Option<String> = None;
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "namespace_definition" => {
                if let Some(nm) = n.child_by_field_name("name") {
                    namespace = nm.utf8_text(bytes).ok().map(str::to_string);
                }
            }
            "class_declaration" if class_name.is_none() => {
                if let Some(nm) = n.child_by_field_name("name") {
                    class_name = nm.utf8_text(bytes).ok().map(str::to_string);
                }
            }
            _ => {}
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    let class = class_name?;
    Some(match namespace {
        Some(ns) => format!("{ns}\\{class}"),
        None => class,
    })
}

#[cfg(test)]
mod tests;
