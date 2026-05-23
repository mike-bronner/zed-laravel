//! Document symbol extraction for `textDocument/documentSymbol`.
//!
//! Produces a hierarchical symbol tree for Laravel-aware file types so editors
//! (Zed outline, Helix symbol picker, Neovim aerial, etc.) can show structural
//! navigation that's meaningful for Laravel projects.
//!
//! ## Supported file kinds
//!
//! | Kind         | Symbols                                                                |
//! |--------------|------------------------------------------------------------------------|
//! | `RouteFile`  | `Route::get/post/...` calls labelled `METHOD URI [n=…]`                |
//! | `Blade`      | `@section`, `@push`, `@yield`, `@stack`, `@component` with nesting     |
//! | `Php`        | Classes / interfaces / traits / enums + members + free functions       |
//! | `Other`      | Empty                                                                  |
//!
//! For `Php` files, the extractor further specialises: Livewire components
//! emit *public-only* properties and methods; Eloquent models emit *only*
//! relationship methods and `scope*` methods; everything else emits full
//! class structure (all visibility levels, all top-level structures, plus
//! free functions).
//!
//! Structural parsing of PHP class bodies goes through [`crate::php_outline`]
//! (tree-sitter); only routes and Blade still use regex. All positions are
//! 0-based to match the LSP and the rest of this codebase (see `CLAUDE.md`
//! § Position Indexing Convention).

use lazy_static::lazy_static;
use regex::Regex;
use std::path::Path;

use crate::php_outline::{
    extract_php_structure, PhpFileStructure, PhpFunctionInfo, PhpMethodInfo, PhpPropertyInfo,
    PhpStructure, PhpStructureKind, PhpVisibility,
};

/// A single entry in the document symbol tree. Plain data so it can cross the
/// Salsa async boundary and be cached cheaply.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SymbolEntry {
    /// Display name (e.g. "GET /users", "increment", "$count").
    pub name: String,
    /// Secondary detail shown after the name (e.g. "[name=users.index]",
    /// "HasMany").
    pub detail: Option<String>,
    pub kind: SymbolEntryKind,
    /// 0-based start line.
    pub start_line: u32,
    /// 0-based start column.
    pub start_column: u32,
    /// 0-based end line.
    pub end_line: u32,
    /// 0-based end column (exclusive — column one past the last character).
    pub end_column: u32,
    pub children: Vec<SymbolEntry>,
}

/// LSP-aligned symbol kinds we actually emit. Mapping to `tower_lsp::SymbolKind`
/// happens in `main.rs` where the LSP types are available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SymbolEntryKind {
    Class,
    Interface,
    Trait,
    Enum,
    Method,
    Property,
    Field,
    Function,
    Namespace,
    Variable,
}

/// Path-based file classification. PHP class files are all `Php` — the
/// Livewire/Model/Generic subclassification happens inside extraction so we
/// only parse PHP once per request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileKind {
    /// A file under `routes/` (web.php, api.php, etc.).
    RouteFile,
    /// A `*.blade.php` file.
    Blade,
    /// Any `.php` file outside `routes/` — Livewire / model / plain class /
    /// helpers file. Differentiation happens in `extract_symbols`.
    Php,
    /// Anything else — no Laravel-aware symbols to emit.
    Other,
}

/// Classify a file by path. Path is sufficient for the top-level dispatch —
/// PHP files are all routed to the `Php` extractor which then parses the
/// content once with tree-sitter to pick the right shape.
pub fn classify_file(path: &Path) -> FileKind {
    let path_str = path.to_string_lossy();

    if path_str.ends_with(".blade.php") {
        return FileKind::Blade;
    }
    if !path_str.ends_with(".php") {
        return FileKind::Other;
    }
    if path
        .components()
        .any(|c| c.as_os_str().eq_ignore_ascii_case("routes"))
    {
        return FileKind::RouteFile;
    }
    FileKind::Php
}

/// Dispatch to the right extractor based on file kind.
pub fn extract_symbols(content: &str, kind: FileKind) -> Vec<SymbolEntry> {
    match kind {
        FileKind::RouteFile => extract_route_symbols(content),
        FileKind::Blade => extract_blade_symbols(content),
        FileKind::Php => extract_php_symbols(content),
        FileKind::Other => Vec::new(),
    }
}

// ============================================================================
// Routes
// ============================================================================

/// Extract `Route::verb('/uri', ...)` definitions and their `->name(...)`
/// suffixes (when present). Each becomes a top-level symbol; route groups
/// are flattened — we don't currently track `Route::prefix(...)->group(...)`
/// nesting in the outline.
fn extract_route_symbols(content: &str) -> Vec<SymbolEntry> {
    lazy_static! {
        // Match `Route::verb('uri'` or `Route::verb("uri"`. The verb capture
        // covers the standard HTTP routing helpers we surface in the outline.
        static ref ROUTE_RE: Regex = Regex::new(
            r#"Route::(get|post|put|patch|delete|options|any|match|view|redirect|fallback)\s*\(\s*(?:\[[^\]]*\]\s*,\s*)?['"]([^'"]*)['"]"#,
        )
        .unwrap();
        // Match `->name('foo')` or `->name("foo")` following the route. Captures
        // only the name; the caller searches for this within the route's tail
        // up to the next semicolon.
        static ref NAME_RE: Regex = Regex::new(r#"->name\s*\(\s*['"]([^'"]+)['"]"#).unwrap();
    }

    let mut symbols = Vec::new();

    for cap in ROUTE_RE.captures_iter(content) {
        let full_match = cap.get(0).expect("regex match always has group 0");
        let verb = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let uri = cap.get(2).map(|m| m.as_str()).unwrap_or("");

        // Look ahead for `->name(...)` up to the next semicolon — that
        // captures the typical fluent-chain shape without consuming the next
        // route call.
        let tail_start = full_match.end();
        let tail_end = content[tail_start..]
            .find(';')
            .map(|i| tail_start + i)
            .unwrap_or(content.len());
        let tail = &content[tail_start..tail_end];

        let name = NAME_RE
            .captures(tail)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string());

        let (start_line, start_column) = byte_to_line_col(content, full_match.start());
        let (end_line, end_column) = byte_to_line_col(content, tail_end);

        let detail = name.as_ref().map(|n| format!("[name={n}]"));
        let label = format!("{} {}", verb.to_uppercase(), uri);

        symbols.push(SymbolEntry {
            name: label,
            detail,
            kind: SymbolEntryKind::Function,
            start_line,
            start_column,
            end_line,
            end_column,
            children: Vec::new(),
        });
    }

    symbols
}

// ============================================================================
// Blade
// ============================================================================

/// Extract structural Blade directives (`@section`, `@push`, `@stack`,
/// `@yield`, `@component`) with parent/child nesting derived from the
/// open/close pairs.
fn extract_blade_symbols(content: &str) -> Vec<SymbolEntry> {
    lazy_static! {
        // Match any of the structural directives we surface, optionally with
        // an argument list. The directive name is captured for dispatch.
        static ref DIRECTIVE_RE: Regex = Regex::new(
            r#"@(section|endsection|push|endpush|prepend|endprepend|stack|yield|component|endcomponent|extends)\b(?:\s*\(\s*['"]([^'"]*)['"]\s*(?:,[^)]*)?\))?"#,
        )
        .unwrap();
    }

    let mut roots: Vec<SymbolEntry> = Vec::new();
    let mut stack: Vec<BladeOpenBlock> = Vec::new();

    for cap in DIRECTIVE_RE.captures_iter(content) {
        let full = cap.get(0).expect("regex match always has group 0");
        let directive = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let arg = cap.get(2).map(|m| m.as_str().to_string());

        let (start_line, start_column) = byte_to_line_col(content, full.start());
        let (end_line, end_column) = byte_to_line_col(content, full.end());

        match directive {
            // Closing directives pop the matching open block. If the stack
            // head doesn't match (unbalanced source), we still pop to avoid
            // runaway nesting — outlines tolerate inexact end positions better
            // than missing blocks.
            "endsection" => {
                close_blade_block(&mut stack, &mut roots, "section", end_line, end_column)
            }
            "endpush" => close_blade_block(&mut stack, &mut roots, "push", end_line, end_column),
            "endprepend" => {
                close_blade_block(&mut stack, &mut roots, "prepend", end_line, end_column)
            }
            "endcomponent" => {
                close_blade_block(&mut stack, &mut roots, "component", end_line, end_column)
            }

            // Opening directives push a new block onto the stack.
            "section" | "push" | "prepend" | "component" => {
                let name = arg.unwrap_or_else(|| format!("@{directive}"));
                stack.push(BladeOpenBlock {
                    directive: directive.to_string(),
                    symbol: SymbolEntry {
                        name,
                        detail: Some(format!("@{directive}")),
                        kind: SymbolEntryKind::Namespace,
                        start_line,
                        start_column,
                        end_line,
                        end_column,
                        children: Vec::new(),
                    },
                });
            }

            // Self-contained directives — emit as leaves of the current parent
            // (or root) without entering the stack.
            "stack" | "yield" | "extends" => {
                let name = arg.unwrap_or_else(|| format!("@{directive}"));
                push_blade_entry(
                    &mut stack,
                    &mut roots,
                    SymbolEntry {
                        name,
                        detail: Some(format!("@{directive}")),
                        kind: SymbolEntryKind::Field,
                        start_line,
                        start_column,
                        end_line,
                        end_column,
                        children: Vec::new(),
                    },
                );
            }
            _ => {}
        }
    }

    // Flush any unclosed open blocks — emit at whatever depth they ended up.
    while let Some(open) = stack.pop() {
        push_blade_entry(&mut stack, &mut roots, open.symbol);
    }

    roots
}

/// One frame on the Blade open-block stack — the directive that opened it
/// plus the in-progress symbol that will receive children until `@end*` closes it.
struct BladeOpenBlock {
    directive: String,
    symbol: SymbolEntry,
}

fn close_blade_block(
    stack: &mut Vec<BladeOpenBlock>,
    roots: &mut Vec<SymbolEntry>,
    expected: &str,
    end_line: u32,
    end_column: u32,
) {
    while let Some(top) = stack.pop() {
        let matched = top.directive == expected;
        let mut symbol = top.symbol;
        if matched {
            symbol.end_line = end_line;
            symbol.end_column = end_column;
        }
        push_blade_entry(stack, roots, symbol);
        if matched {
            return;
        }
    }
}

fn push_blade_entry(
    stack: &mut [BladeOpenBlock],
    roots: &mut Vec<SymbolEntry>,
    entry: SymbolEntry,
) {
    if let Some(parent) = stack.last_mut() {
        parent.symbol.children.push(entry);
    } else {
        roots.push(entry);
    }
}

// ============================================================================
// PHP (Livewire / Eloquent / Generic) — single parse, content-based dispatch
// ============================================================================

/// Parse the PHP file once via tree-sitter, then dispatch by what the first
/// class extends. Plain PHP files (no class, or class that doesn't extend a
/// Laravel base) get a full structural outline.
fn extract_php_symbols(content: &str) -> Vec<SymbolEntry> {
    let structure = extract_php_structure(content);

    let first_class_extends = structure
        .structures
        .iter()
        .find(|s| s.kind == PhpStructureKind::Class)
        .and_then(|c| c.extends.as_deref());

    match first_class_extends {
        Some(extends) if is_livewire_component(extends) => livewire_symbols(&structure),
        Some(extends) if is_eloquent_model(extends) => model_symbols(&structure),
        _ => generic_php_symbols(&structure),
    }
}

/// `Component` (bare), `Livewire\Component`, or any class whose extends target
/// resolves to `Component` after namespace-simplification. Conservative — a
/// non-Livewire `Component` base would also match, but the generic path also
/// produces sensible output, so the cost of a false positive is low.
fn is_livewire_component(extends: &str) -> bool {
    extends == "Component"
}

fn is_eloquent_model(extends: &str) -> bool {
    matches!(
        extends,
        "Model" | "Authenticatable" | "Pivot" | "MorphPivot"
    )
}

/// Livewire components: emit the class with *public* properties and methods
/// only. Private/protected members are implementation details, not surface area.
fn livewire_symbols(structure: &PhpFileStructure) -> Vec<SymbolEntry> {
    let Some(class) = structure
        .structures
        .iter()
        .find(|s| s.kind == PhpStructureKind::Class)
    else {
        return Vec::new();
    };

    let mut entry = structure_to_symbol(class);

    let mut children: Vec<SymbolEntry> = class
        .properties
        .iter()
        .filter(|p| p.visibility == PhpVisibility::Public)
        .map(property_to_symbol)
        .chain(
            class
                .methods
                .iter()
                .filter(|m| m.visibility == PhpVisibility::Public)
                .map(method_to_symbol),
        )
        .collect();
    children.sort_by_key(|c| (c.start_line, c.start_column));
    extend_class_end_to_last_child(&mut entry, &children);
    entry.children = children;

    vec![entry]
}

/// Eloquent models: emit the class with *only* relationship methods (return
/// type matches a known Eloquent relation) and query scopes (`scope*`). Other
/// methods are noise on the outline of a model.
fn model_symbols(structure: &PhpFileStructure) -> Vec<SymbolEntry> {
    let Some(class) = structure
        .structures
        .iter()
        .find(|s| s.kind == PhpStructureKind::Class)
    else {
        return Vec::new();
    };

    let mut entry = structure_to_symbol(class);

    let mut children: Vec<SymbolEntry> = class
        .methods
        .iter()
        .filter(|m| is_relationship(m) || is_scope(m))
        .map(method_to_symbol)
        .collect();
    children.sort_by_key(|c| (c.start_line, c.start_column));
    extend_class_end_to_last_child(&mut entry, &children);
    entry.children = children;

    vec![entry]
}

fn is_relationship(m: &PhpMethodInfo) -> bool {
    m.return_type
        .as_deref()
        .is_some_and(is_relationship_return_type)
}

fn is_scope(m: &PhpMethodInfo) -> bool {
    m.name.starts_with("scope")
        && m.name
            .chars()
            .nth(5)
            .is_some_and(|c| c.is_ascii_uppercase())
}

fn is_relationship_return_type(detail: &str) -> bool {
    const RELATIONSHIPS: &[&str] = &[
        "HasMany",
        "HasOne",
        "BelongsTo",
        "BelongsToMany",
        "HasManyThrough",
        "HasOneThrough",
        "MorphMany",
        "MorphOne",
        "MorphTo",
        "MorphToMany",
        "MorphedByMany",
    ];
    RELATIONSHIPS.iter().any(|r| detail.contains(r))
}

/// Plain PHP files (controllers, jobs, helpers, etc.) — emit every top-level
/// class/interface/trait/enum with *all* properties and methods regardless of
/// visibility, plus any free functions. This is the strict-upgrade path when
/// a Zed user opts into LSP outlines: parity with tree-sitter outline.scm
/// plus our Laravel-aware sugar.
fn generic_php_symbols(structure: &PhpFileStructure) -> Vec<SymbolEntry> {
    let mut symbols = Vec::new();

    for s in &structure.structures {
        let mut entry = structure_to_symbol(s);

        let mut children: Vec<SymbolEntry> = s
            .properties
            .iter()
            .map(property_to_symbol)
            .chain(s.methods.iter().map(method_to_symbol))
            .collect();
        children.sort_by_key(|c| (c.start_line, c.start_column));
        extend_class_end_to_last_child(&mut entry, &children);
        entry.children = children;

        symbols.push(entry);
    }

    for f in &structure.functions {
        symbols.push(function_to_symbol(f));
    }

    symbols
}

fn structure_to_symbol(s: &PhpStructure) -> SymbolEntry {
    let kind = match s.kind {
        PhpStructureKind::Class => SymbolEntryKind::Class,
        PhpStructureKind::Interface => SymbolEntryKind::Interface,
        PhpStructureKind::Trait => SymbolEntryKind::Trait,
        PhpStructureKind::Enum => SymbolEntryKind::Enum,
    };
    let detail = s.extends.as_ref().map(|e| format!("extends {e}"));
    SymbolEntry {
        name: s.name.clone(),
        detail,
        kind,
        start_line: s.start_line,
        start_column: s.start_column,
        end_line: s.end_line,
        end_column: s.end_column,
        children: Vec::new(),
    }
}

fn method_to_symbol(m: &PhpMethodInfo) -> SymbolEntry {
    SymbolEntry {
        name: m.name.clone(),
        detail: m.return_type.clone(),
        kind: SymbolEntryKind::Method,
        start_line: m.start_line,
        start_column: m.start_column,
        end_line: m.end_line,
        end_column: m.end_column,
        children: Vec::new(),
    }
}

fn property_to_symbol(p: &PhpPropertyInfo) -> SymbolEntry {
    SymbolEntry {
        name: format!("${}", p.name),
        detail: p.property_type.clone(),
        kind: SymbolEntryKind::Property,
        start_line: p.start_line,
        start_column: p.start_column,
        end_line: p.end_line,
        end_column: p.end_column,
        children: Vec::new(),
    }
}

fn function_to_symbol(f: &PhpFunctionInfo) -> SymbolEntry {
    SymbolEntry {
        name: f.name.clone(),
        detail: f.return_type.clone(),
        kind: SymbolEntryKind::Function,
        start_line: f.start_line,
        start_column: f.start_column,
        end_line: f.end_line,
        end_column: f.end_column,
        children: Vec::new(),
    }
}

/// Tighten the class's `end_*` to the last child's end. tree-sitter already
/// gives us the true class end, but for *filtered* outlines (Livewire/Model)
/// shrinking to the visible children gives editors a tighter selection range.
fn extend_class_end_to_last_child(entry: &mut SymbolEntry, children: &[SymbolEntry]) {
    if let Some(last) = children.last() {
        entry.end_line = last.end_line;
        entry.end_column = last.end_column;
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Convert a byte offset into 0-based (line, column). Mirrors the helper in
/// `route_discovery::byte_to_line_col` — kept local so this module has no
/// dependency on internals of another extractor.
fn byte_to_line_col(content: &str, byte_offset: usize) -> (u32, u32) {
    let bytes = content.as_bytes();
    let mut line = 0u32;
    let mut last_newline: i64 = -1;
    for (idx, b) in bytes.iter().enumerate().take(byte_offset) {
        if *b == b'\n' {
            line += 1;
            last_newline = idx as i64;
        }
    }
    let column = (byte_offset as i64 - last_newline - 1) as u32;
    (line, column)
}

#[cfg(test)]
mod tests;
