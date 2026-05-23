//! Document symbol extraction for `textDocument/documentSymbol`.
//!
//! Produces a hierarchical symbol tree for Laravel-aware file types so editors
//! (Zed outline, Helix symbol picker, Neovim aerial, etc.) can show structural
//! navigation that's meaningful for Laravel projects.
//!
//! ## Supported file kinds
//!
//! | Kind                | Symbols                                                 |
//! |---------------------|---------------------------------------------------------|
//! | `RouteFile`         | `Route::get/post/...` calls labelled `METHOD URI [n=…]` |
//! | `Blade`             | `@section`, `@push`, `@yield`, `@stack`, `@component`   |
//! | `LivewireComponent` | Class + public properties + public methods              |
//! | `EloquentModel`     | Class + relationship methods + `scope*` methods         |
//!
//! Each extractor returns plain `SymbolEntry` values; the LSP handler in
//! `main.rs` converts these to `tower_lsp::lsp_types::DocumentSymbol` and the
//! Salsa actor in `salsa_impl.rs` memoizes them per file version.
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

/// LSP-aligned symbol kinds. Restricted to the variants this extension actually
/// emits — wider mapping happens in `main.rs` where `tower_lsp` types are
/// available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SymbolEntryKind {
    Class,
    Method,
    Property,
    Field,
    Function,
    Namespace,
    Variable,
}

/// File classification — drives which extractor runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileKind {
    /// A file under `routes/` (web.php, api.php, etc.).
    RouteFile,
    /// A `*.blade.php` file.
    Blade,
    /// A PHP class extending `Livewire\Component` (or aliased equivalent).
    LivewireComponent,
    /// A PHP class extending `Illuminate\Database\Eloquent\Model` (or aliased).
    EloquentModel,
    /// Anything else — no Laravel-aware symbols to emit.
    Other,
}

/// Classify a file by path + content. Path is checked first because it's
/// cheap; content is consulted only to disambiguate plain PHP files.
pub fn classify_file(path: &Path, content: &str) -> FileKind {
    let path_str = path.to_string_lossy();

    // Blade files — extension is authoritative.
    if path_str.ends_with(".blade.php") {
        return FileKind::Blade;
    }

    // Non-PHP files: nothing to do here.
    if !path_str.ends_with(".php") {
        return FileKind::Other;
    }

    // Files under any `routes/` directory are route files. Match the
    // `is_under_routes_dir` heuristic from route_discovery.rs.
    if path
        .components()
        .any(|c| c.as_os_str().eq_ignore_ascii_case("routes"))
    {
        return FileKind::RouteFile;
    }

    // PHP class files — disambiguate by what they extend.
    if content_extends_livewire_component(content) {
        return FileKind::LivewireComponent;
    }
    if content_extends_eloquent_model(content) {
        return FileKind::EloquentModel;
    }

    FileKind::Other
}

/// Dispatch to the right extractor based on file kind.
pub fn extract_symbols(content: &str, kind: FileKind) -> Vec<SymbolEntry> {
    match kind {
        FileKind::RouteFile => extract_route_symbols(content),
        FileKind::Blade => extract_blade_symbols(content),
        FileKind::LivewireComponent => extract_livewire_symbols(content),
        FileKind::EloquentModel => extract_model_symbols(content),
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
// Livewire components
// ============================================================================

/// Extract the component class + its public properties and methods.
fn extract_livewire_symbols(content: &str) -> Vec<SymbolEntry> {
    let Some(class) = extract_class_outline(content) else {
        return Vec::new();
    };
    vec![class]
}

// ============================================================================
// Eloquent models
// ============================================================================

/// Extract the model class + relationship methods + `scope*` methods. Other
/// public methods are intentionally omitted — they're rarely what someone
/// jumps to in a model.
fn extract_model_symbols(content: &str) -> Vec<SymbolEntry> {
    let Some(mut class) = extract_class_outline(content) else {
        return Vec::new();
    };

    // Filter the class's methods to relationships + scopes; keep everything
    // else off the outline to avoid noise.
    class.children.retain(|child| match child.kind {
        SymbolEntryKind::Method => {
            let is_scope = child.name.starts_with("scope")
                && child
                    .name
                    .chars()
                    .nth(5)
                    .is_some_and(|c| c.is_ascii_uppercase());
            let is_relationship = child
                .detail
                .as_deref()
                .is_some_and(is_relationship_return_type);
            is_scope || is_relationship
        }
        _ => false,
    });

    vec![class]
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

// ============================================================================
// Shared PHP class outline parser (used by Livewire + Model extractors)
// ============================================================================

/// Parse a PHP class declaration and emit a `Class` symbol with public
/// properties (`Property`) and public methods (`Method`) as children. Method
/// `detail` carries the declared return type, when present, so the model
/// filter can recognise relationships.
fn extract_class_outline(content: &str) -> Option<SymbolEntry> {
    lazy_static! {
        static ref CLASS_RE: Regex =
            Regex::new(r#"\bclass\s+([A-Za-z_][A-Za-z0-9_]*)\b(?:\s+extends\s+([A-Za-z_\\][A-Za-z0-9_\\]*))?"#)
                .unwrap();
        // public [readonly] [type] $name [= ...];
        static ref PROPERTY_RE: Regex = Regex::new(
            r#"(?m)^\s*public\s+(?:readonly\s+)?(?:(\??[\w\\]+)\s+)?\$([A-Za-z_][A-Za-z0-9_]*)"#,
        )
        .unwrap();
        // public function name(...) [: Return]
        static ref METHOD_RE: Regex = Regex::new(
            r#"(?m)^\s*public\s+(?:static\s+)?function\s+([A-Za-z_][A-Za-z0-9_]*)\s*\([^)]*\)\s*(?::\s*(\??[\w\\|]+))?"#,
        )
        .unwrap();
    }

    let class_cap = CLASS_RE.captures(content)?;
    let class_match = class_cap.get(0).expect("regex match always has group 0");
    let class_name = class_cap.get(1)?.as_str().to_string();
    let extends = class_cap.get(2).map(|m| simple_class_name(m.as_str()));

    let (start_line, start_column) = byte_to_line_col(content, class_match.start());

    let mut class = SymbolEntry {
        name: class_name,
        detail: extends.map(|e| format!("extends {e}")),
        kind: SymbolEntryKind::Class,
        start_line,
        start_column,
        end_line: start_line,
        end_column: start_column,
        children: Vec::new(),
    };

    for cap in PROPERTY_RE.captures_iter(content) {
        let m = cap.get(0).expect("regex match always has group 0");
        let type_hint = cap.get(1).map(|t| t.as_str().to_string());
        let name = cap.get(2)?.as_str().to_string();
        let (line, column) = byte_to_line_col(content, m.start());
        let (end_line, end_column) = byte_to_line_col(content, m.end());

        class.children.push(SymbolEntry {
            name: format!("${name}"),
            detail: type_hint,
            kind: SymbolEntryKind::Property,
            start_line: line,
            start_column: column,
            end_line,
            end_column,
            children: Vec::new(),
        });
    }

    for cap in METHOD_RE.captures_iter(content) {
        let m = cap.get(0).expect("regex match always has group 0");
        let name = cap.get(1)?.as_str().to_string();
        let return_type = cap.get(2).map(|t| simple_class_name(t.as_str()));
        let (line, column) = byte_to_line_col(content, m.start());
        let (end_line, end_column) = byte_to_line_col(content, m.end());

        class.children.push(SymbolEntry {
            name,
            detail: return_type,
            kind: SymbolEntryKind::Method,
            start_line: line,
            start_column: column,
            end_line,
            end_column,
            children: Vec::new(),
        });
    }

    // Sort children by position so editors render them in source order.
    class
        .children
        .sort_by_key(|c| (c.start_line, c.start_column));

    // Approximate the class's end position with the last child's end (or the
    // class declaration if the body has no children).
    if let Some(last) = class.children.last() {
        class.end_line = last.end_line;
        class.end_column = last.end_column;
    }

    Some(class)
}

fn simple_class_name(fqn: &str) -> String {
    fqn.rsplit('\\').next().unwrap_or(fqn).to_string()
}

fn content_extends_livewire_component(content: &str) -> bool {
    // Match `extends Component`, `extends \Livewire\Component`, or
    // `extends Livewire\Component`. False positives on plain PHP classes
    // that happen to extend something else named "Component" are tolerable —
    // the model extractor only fires when this returns false anyway.
    lazy_static! {
        static ref RE: Regex = Regex::new(r#"extends\s+(?:\\?Livewire\\)?Component\b"#,).unwrap();
    }
    RE.is_match(content)
}

fn content_extends_eloquent_model(content: &str) -> bool {
    // Match `extends Model`, `extends \Illuminate\Database\Eloquent\Model`,
    // or `extends Authenticatable` (the User model's typical base).
    lazy_static! {
        static ref RE: Regex = Regex::new(
            r#"extends\s+(?:\\?Illuminate\\Database\\Eloquent\\)?(?:Model|Authenticatable|Pivot|MorphPivot)\b"#,
        )
        .unwrap();
    }
    RE.is_match(content)
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
