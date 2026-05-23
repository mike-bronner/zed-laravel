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
//! | `Php`        | Empty — see note below                                                 |
//! | `Other`      | Empty                                                                  |
//!
//! ## Why `Php` returns no symbols
//!
//! Most Zed users with Laravel projects have the `php` extension installed,
//! which registers Intelephense / Phpactor / PhpTools as language servers
//! for the `PHP` language. Those LSPs return their own `textDocument/document
//! Symbol` responses, and Zed *merges* responses from all LSPs serving a
//! file — which means anything we emit for `.php` files appears twice in
//! the outline panel: once with our rich labels (`public function show(int
//! $id): View`) and once with the PHP LSP's bare labels (`public function
//! show` with locals/catches as children).
//!
//! We can't deduplicate from our side (Zed handles the merge), and our
//! rich-labelled PHP outline doesn't add Laravel-specific information that
//! Intelephense doesn't already cover at the class-member level. So we
//! cede PHP class bodies to the dedicated PHP LSP and keep our own
//! documentSymbol contribution focused on Laravel-specific shapes the PHP
//! LSP doesn't understand: route declarations and Blade templates.
//!
//! All positions are 0-based to match the LSP and the rest of this codebase
//! (see `CLAUDE.md` § Position Indexing Convention).

use lazy_static::lazy_static;
use regex::Regex;
use std::path::Path;

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

/// Path-based file classification. Plain PHP files (`Php`) are classified
/// so that the dispatch knows to skip them — see the module-level docs on
/// why we don't emit document symbols for plain PHP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileKind {
    /// A file under `routes/` (web.php, api.php, etc.).
    RouteFile,
    /// A `*.blade.php` file.
    Blade,
    /// Any `.php` file outside `routes/` — Livewire component, model,
    /// controller, helpers file, etc. Dispatched to an empty extractor;
    /// see the module-level docs for why.
    Php,
    /// Anything else — no Laravel-aware symbols to emit.
    Other,
}

/// Classify a file by path. Path is sufficient for the top-level dispatch —
/// we don't peek into PHP file contents because the `Php` extractor returns
/// empty regardless.
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

/// Dispatch to the right extractor based on file kind. `Php` returns
/// empty — see module-level docs.
pub fn extract_symbols(content: &str, kind: FileKind) -> Vec<SymbolEntry> {
    match kind {
        FileKind::RouteFile => extract_route_symbols(content),
        FileKind::Blade => extract_blade_symbols(content),
        FileKind::Php | FileKind::Other => Vec::new(),
    }
}

// ============================================================================
// Routes
// ============================================================================

/// Walk the route file's AST via [`crate::route_outline`] and convert the
/// resulting `RouteOutline` tree into `SymbolEntry` form. Group containers
/// become hierarchical `Namespace`-kind symbols with their child routes
/// nested inside; leaf routes become `Function`-kind symbols with label
/// `METHOD URI` and a `[name=…]` detail when named.
fn extract_route_symbols(content: &str) -> Vec<SymbolEntry> {
    let outline = crate::route_outline::extract_route_outline(content);
    outline.iter().map(route_outline_to_symbol).collect()
}

fn route_outline_to_symbol(route: &crate::route_outline::RouteOutline) -> SymbolEntry {
    if route.is_group {
        // Group container labelled `group [prefix=/x, name=y.]` — the
        // literal `group` is the primary type identifier; the bracket
        // suffix lists whichever modifiers the group applies. Detail is
        // not set because Zed's outline panel doesn't render it
        // (zed-industries/zed#49095) — we keep everything in `name` for
        // consistent rendering across editors. Upstream coloring tracked
        // at zed#57576.
        //
        // `kind: Function` (not `Namespace`) and `range` taken from the
        // closure expression (not the wider chain) together prevent Zed
        // from overlaying tree-sitter `Closure` children inside our group.
        SymbolEntry {
            name: group_label(&route.uri, route.name.as_deref()),
            detail: None,
            kind: SymbolEntryKind::Function,
            start_line: route.start_line,
            start_column: route.start_column,
            end_line: route.end_line,
            end_column: route.end_column,
            children: route.children.iter().map(route_outline_to_symbol).collect(),
        }
    } else {
        // Leaf route: `METHOD URI` with the route name appended in
        // brackets when present.
        SymbolEntry {
            name: route_label(&route.method, &route.uri, route.name.as_deref()),
            detail: None,
            kind: SymbolEntryKind::Function,
            start_line: route.start_line,
            start_column: route.start_column,
            end_line: route.end_line,
            end_column: route.end_column,
            children: Vec::new(),
        }
    }
}

/// Compose a group label. Examples:
///   `group`
///   `group [prefix=/api]`
///   `group [name=api.]`
///   `group [prefix=/api, name=api.]`
fn group_label(uri: &str, name: Option<&str>) -> String {
    let mut parts = Vec::with_capacity(2);
    if !uri.is_empty() {
        parts.push(format!("prefix={uri}"));
    }
    if let Some(n) = name {
        parts.push(format!("name={n}"));
    }
    if parts.is_empty() {
        "group".to_string()
    } else {
        format!("group [{}]", parts.join(", "))
    }
}

/// Compose a leaf-route label. Examples:
///   `GET /users`
///   `GET /users [name=users.index]`
fn route_label(method: &str, uri: &str, name: Option<&str>) -> String {
    match name {
        Some(n) => format!("{method} {uri} [name={n}]"),
        None => format!("{method} {uri}"),
    }
}

// ============================================================================
// Blade
// ============================================================================

/// Extract noteworthy structural elements from a Blade file: layout
/// directives (`@extends`, `@section`, `@yield`, `@push`, `@stack`),
/// declarative directives (`@props`, `@slot`, `@include*`), and the modern
/// HTML-tag-based component usage (`<x-…>`, `<livewire:…>`, `<flux:…>`,
/// `<x-slot:…>`).
///
/// Container directives that have `@end*` partners (`@section`, `@push`,
/// `@prepend`, `@component`) form parent/child nesting via an open-block
/// stack. Everything else — `@yield`, `@stack`, `@extends`, `@include`,
/// `@props`, `@slot`, HTML component tags — emits as leaves of whatever
/// container they live inside.
fn extract_blade_symbols(content: &str) -> Vec<SymbolEntry> {
    lazy_static! {
        // Directives we surface — both block-style (with @end partners) and
        // self-contained. Capture 1 = directive name. The argument list is
        // parsed separately by `parse_directive_args` because real Blade
        // code has nested parens (`@includeWhen($user->method(), 'partial')`)
        // and array literals (`@props(['title', 'count' => 0])`) which a
        // pure regex can't balance.
        static ref DIRECTIVE_RE: Regex = Regex::new(
            r#"@(section|endsection|push|endpush|prepend|endprepend|stack|yield|component|endcomponent|extends|include|includeIf|includeWhen|includeUnless|includeFirst|slot|endslot|props)\b"#,
        )
        .unwrap();
        // Opening / self-closing component tags. Capture 1 = full tag-name
        // (e.g. `x-button`, `livewire:counter`, `flux:icon`). Capture 2 =
        // any trailing `/` that makes the tag self-closing (`<x-icon />`).
        // The Rust regex crate doesn't support look-ahead, so `<x-slot:…>`
        // and `<x-slot name="…">` are filtered out in the loop below
        // (handled by `SLOT_TAG_RE` separately so we can render them as
        // slot declarations rather than generic component usage).
        static ref COMPONENT_OPEN_TAG_RE: Regex = Regex::new(
            r#"<((?:x-[a-z][a-z0-9._:-]*)|(?:livewire:[a-z][a-z0-9._-]*)|(?:flux:[a-z][a-z0-9._-]*))\b[^>]*?(/?)>"#,
        )
        .unwrap();
        // Closing component tags — `</x-name>`, `</livewire:name>`,
        // `</flux:name>`. Capture 1 = the tag name (must match the opening
        // tag's name for the pair to nest).
        static ref COMPONENT_CLOSE_TAG_RE: Regex = Regex::new(
            r#"</((?:x-[a-z][a-z0-9._:-]*)|(?:livewire:[a-z][a-z0-9._-]*)|(?:flux:[a-z][a-z0-9._-]*))>"#,
        )
        .unwrap();
        // Slot declarations — `<x-slot:name>` (Laravel 9+) and the legacy
        // `<x-slot name="value">` form. Capture 1 = the slot name (modern
        // syntax); capture 2 = the slot name (legacy syntax).
        static ref SLOT_TAG_RE: Regex = Regex::new(
            r#"<x-slot(?::([a-zA-Z_][a-zA-Z0-9_-]*)|\s+name=['"]([^'"]+)['"])"#,
        )
        .unwrap();
    }

    // Collect every interesting match across all regexes, keyed by source
    // byte offset. We process them in source order so the nesting stack
    // assigns children to the right parent.
    enum BladeMatch {
        Directive {
            directive: String,
            arg: Option<String>,
            start: usize,
            end: usize,
        },
        /// `<x-button>` (opens a block that nests until `</x-button>`).
        ComponentOpen {
            tag: String,
            start: usize,
            end: usize,
        },
        /// `</x-button>` (closes the matching `ComponentOpen`).
        ComponentClose {
            tag: String,
            start: usize,
            end: usize,
        },
        /// `<x-icon />` (self-closing, emitted as a leaf without nesting).
        ComponentSelfClose {
            tag: String,
            start: usize,
            end: usize,
        },
        /// `<x-slot:name>` or `<x-slot name="…">` — emitted as a leaf for
        /// now (slot pairing across the modern/legacy closing forms isn't
        /// worth the complexity yet).
        SlotTag {
            name: String,
            start: usize,
            end: usize,
        },
    }

    fn match_start(m: &BladeMatch) -> usize {
        match m {
            BladeMatch::Directive { start, .. }
            | BladeMatch::ComponentOpen { start, .. }
            | BladeMatch::ComponentClose { start, .. }
            | BladeMatch::ComponentSelfClose { start, .. }
            | BladeMatch::SlotTag { start, .. } => *start,
        }
    }

    let mut matches: Vec<BladeMatch> = Vec::new();

    for cap in DIRECTIVE_RE.captures_iter(content) {
        let full = cap.get(0).expect("regex match always has group 0");
        let directive = cap
            .get(1)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        let (arg, end_pos) = parse_directive_args(content, full.end());
        matches.push(BladeMatch::Directive {
            directive,
            arg,
            start: full.start(),
            end: end_pos,
        });
    }
    for cap in COMPONENT_OPEN_TAG_RE.captures_iter(content) {
        let full = cap.get(0).expect("regex match always has group 0");
        let tag = cap
            .get(1)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        // Skip slot tags — emitted as `SlotTag` below.
        if tag == "x-slot" || tag.starts_with("x-slot:") {
            continue;
        }
        let self_closing = cap.get(2).map(|m| m.as_str() == "/").unwrap_or(false);
        let entry = if self_closing {
            BladeMatch::ComponentSelfClose {
                tag,
                start: full.start(),
                end: full.end(),
            }
        } else {
            BladeMatch::ComponentOpen {
                tag,
                start: full.start(),
                end: full.end(),
            }
        };
        matches.push(entry);
    }
    for cap in COMPONENT_CLOSE_TAG_RE.captures_iter(content) {
        let full = cap.get(0).expect("regex match always has group 0");
        let tag = cap
            .get(1)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        if tag == "x-slot" || tag.starts_with("x-slot:") {
            continue;
        }
        matches.push(BladeMatch::ComponentClose {
            tag,
            start: full.start(),
            end: full.end(),
        });
    }
    for cap in SLOT_TAG_RE.captures_iter(content) {
        let full = cap.get(0).expect("regex match always has group 0");
        let name = cap
            .get(1)
            .or_else(|| cap.get(2))
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        matches.push(BladeMatch::SlotTag {
            name,
            start: full.start(),
            end: full.end(),
        });
    }

    matches.sort_by_key(match_start);

    let mut roots: Vec<SymbolEntry> = Vec::new();
    let mut stack: Vec<BladeOpenBlock> = Vec::new();

    for m in matches {
        match m {
            BladeMatch::Directive {
                directive,
                arg,
                start,
                end,
            } => {
                let (start_line, start_column) = byte_to_line_col(content, start);
                let (end_line, end_column) = byte_to_line_col(content, end);

                match directive.as_str() {
                    // Closing directives pop the matching open block. If the
                    // stack head doesn't match (unbalanced source), we still
                    // pop to avoid runaway nesting.
                    "endsection" => {
                        close_blade_block(&mut stack, &mut roots, "section", end_line, end_column)
                    }
                    "endpush" => {
                        close_blade_block(&mut stack, &mut roots, "push", end_line, end_column)
                    }
                    "endprepend" => {
                        close_blade_block(&mut stack, &mut roots, "prepend", end_line, end_column)
                    }
                    "endcomponent" => {
                        close_blade_block(&mut stack, &mut roots, "component", end_line, end_column)
                    }
                    "endslot" => {
                        close_blade_block(&mut stack, &mut roots, "slot", end_line, end_column)
                    }

                    // Opening directives push a new block onto the stack.
                    "section" | "push" | "prepend" | "component" | "slot" => {
                        let label = blade_directive_label(&directive, arg.as_deref());
                        stack.push(BladeOpenBlock {
                            directive: directive.clone(),
                            symbol: SymbolEntry {
                                name: label,
                                detail: None,
                                kind: SymbolEntryKind::Namespace,
                                start_line,
                                start_column,
                                end_line,
                                end_column,
                                children: Vec::new(),
                            },
                        });
                    }

                    // Self-contained directives.
                    "stack" | "yield" | "extends" | "props" | "include" | "includeIf"
                    | "includeWhen" | "includeUnless" | "includeFirst" => {
                        let label = blade_directive_label(&directive, arg.as_deref());
                        push_blade_entry(
                            &mut stack,
                            &mut roots,
                            SymbolEntry {
                                name: label,
                                detail: None,
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
            BladeMatch::ComponentOpen { tag, start, end } => {
                // Push as a container; children will be added until the
                // matching `</tag>` arrives (or we flush on EOF).
                let (start_line, start_column) = byte_to_line_col(content, start);
                let (end_line, end_column) = byte_to_line_col(content, end);
                stack.push(BladeOpenBlock {
                    directive: format!("tag:{tag}"),
                    symbol: SymbolEntry {
                        name: format!("<{tag}>"),
                        detail: None,
                        kind: SymbolEntryKind::Namespace,
                        start_line,
                        start_column,
                        end_line,
                        end_column,
                        children: Vec::new(),
                    },
                });
            }
            BladeMatch::ComponentClose { tag, start: _, end } => {
                let (end_line, end_column) = byte_to_line_col(content, end);
                close_blade_block(
                    &mut stack,
                    &mut roots,
                    &format!("tag:{tag}"),
                    end_line,
                    end_column,
                );
            }
            BladeMatch::ComponentSelfClose { tag, start, end } => {
                let (start_line, start_column) = byte_to_line_col(content, start);
                let (end_line, end_column) = byte_to_line_col(content, end);
                push_blade_entry(
                    &mut stack,
                    &mut roots,
                    SymbolEntry {
                        name: format!("<{tag} />"),
                        detail: None,
                        kind: SymbolEntryKind::Field,
                        start_line,
                        start_column,
                        end_line,
                        end_column,
                        children: Vec::new(),
                    },
                );
            }
            BladeMatch::SlotTag { name, start, end } => {
                let (start_line, start_column) = byte_to_line_col(content, start);
                let (end_line, end_column) = byte_to_line_col(content, end);
                push_blade_entry(
                    &mut stack,
                    &mut roots,
                    SymbolEntry {
                        name: format!("<x-slot:{name}>"),
                        detail: None,
                        kind: SymbolEntryKind::Field,
                        start_line,
                        start_column,
                        end_line,
                        end_column,
                        children: Vec::new(),
                    },
                );
            }
        }
    }

    // Flush any unclosed open blocks — emit at whatever depth they ended up.
    while let Some(open) = stack.pop() {
        push_blade_entry(&mut stack, &mut roots, open.symbol);
    }

    roots
}

/// Compose a Blade directive label. The directive is always present (used
/// to identify the kind of entry); the argument is appended when supplied.
/// Examples:
///   `@extends layouts.app`
///   `@section content`
///   `@include partials.header`
///   `@props title`      ← first array key when @props(['title', ...])
fn blade_directive_label(directive: &str, arg: Option<&str>) -> String {
    match arg {
        Some(a) => format!("@{directive} {a}"),
        None => format!("@{directive}"),
    }
}

/// Scan a Blade directive's `(...)` argument list with brace-aware balanced
/// paren tracking. Returns `(first_string, end_of_directive)`:
///   - `first_string` is the first quoted string literal anywhere in the
///     args (`'title'` from `@props(['title', ...])`, or
///     `'partial'` from `@includeWhen($x->y(), 'partial')`).
///   - `end_of_directive` is the byte offset just past the matching close
///     paren, or `after_directive` if there are no parens. Used as the
///     symbol's end position.
///
/// Handles nested parens (so `$user->method()` inside args doesn't truncate
/// the search early) and quoted-string escape sequences.
fn parse_directive_args(content: &str, after_directive: usize) -> (Option<String>, usize) {
    let bytes = content.as_bytes();
    let mut i = after_directive;

    // Skip whitespace between the directive and its opening `(`. Stop at
    // newlines — a directive without args on the same line has none.
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'(' {
        return (None, after_directive);
    }
    i += 1; // step past the `(`

    let mut depth: u32 = 0;
    let mut first_string: Option<String> = None;
    let mut in_string = false;
    let mut string_quote = b' ';
    let mut string_start = 0usize;

    while i < bytes.len() {
        let c = bytes[i];

        if in_string {
            if c == b'\\' && i + 1 < bytes.len() {
                // Skip the escaped char. We're only looking for the string
                // closing quote; precise escape semantics don't matter here.
                i += 2;
                continue;
            }
            if c == string_quote {
                if first_string.is_none() {
                    if let Ok(s) = std::str::from_utf8(&bytes[string_start..i]) {
                        first_string = Some(s.to_string());
                    }
                }
                in_string = false;
            }
        } else {
            match c {
                b'(' => depth += 1,
                b')' => {
                    if depth == 0 {
                        return (first_string, i + 1);
                    }
                    depth -= 1;
                }
                b'\'' | b'"' => {
                    in_string = true;
                    string_quote = c;
                    string_start = i + 1;
                }
                _ => {}
            }
        }
        i += 1;
    }

    // Unclosed args — return whatever we found, ending at EOF.
    (first_string, bytes.len())
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
