//! Surface every PHP region embedded in a Blade file as `(content, row, col)`
//! triples, ready to be re-parsed with the PHP tree-sitter grammar.
//!
//! tree-sitter-php can't recover PHP expressions wrapped in Blade syntax
//! (HTML tags, `{{ }}`, `@if`, etc.). Run it directly on a Blade file and
//! the PHP captures come back empty — confirmed by a regression test in
//! `salsa_impl/tests.rs`. The fix is to extract each PHP region using the
//! Blade grammar (or, for `@php`, a regex) and feed each region through
//! the PHP parser individually, then offset positions back to Blade.
//!
//! Regions covered:
//!   - `{{ expression }}` — echo, via the Blade query `echo_php_content`
//!   - `{!! expression !!}` — raw echo, also captured by `echo_php_content`
//!   - `@php ... @endphp` — full PHP statements, captured here via regex
//!     since the Blade grammar's representation is uneven and regex gives
//!     us byte-accurate positions for the body.
//!
//! Each region's `(row, col)` is the start of the PHP content — the first
//! character AFTER the wrapper (`{{ `, `{!! `, `@php\n`). Callers prepend
//! a `<?php ` wrapper before parsing and use [`adjust_inner_position`] to
//! map snippet-local positions back to the Blade file.

use crate::parser::{language_blade, parse_blade};
use crate::queries::extract_all_blade_patterns;

/// Length of the `<?php ` wrapper callers prepend before parsing a region as
/// PHP. Snippet-local column 6 maps to Blade column 0 (on row 0).
pub const PHP_WRAPPER_PREFIX_LEN: u32 = 6;

/// One Blade-embedded PHP region. `content` is owned because regions
/// extracted via regex don't borrow from the source the same way the
/// tree-sitter ones do; uniform ownership simplifies the call site.
#[derive(Debug, Clone)]
pub struct BladePhpRegion {
    pub content: String,
    pub row: u32,
    pub column: u32,
    /// Byte offset of the **content** in the outer Blade file — the first
    /// byte of `content` lives at `source[byte_offset]`. Used by features
    /// (e.g. chain extraction) that need to translate snippet-local byte
    /// ranges back to outer-file coordinates: a token at snippet byte `N`
    /// (after the `<?php ` wrapper) lives at outer byte
    /// `byte_offset + (N - PHP_WRAPPER_PREFIX_LEN)`.
    pub byte_offset: usize,
}

/// Extract every PHP region from a Blade source. Returns regions in source
/// order (echoes interleaved with `@php` blocks as they appear). Failures
/// (parser errors, regex misses) return what was found, never `None`.
pub fn extract_php_regions(source: &str) -> Vec<BladePhpRegion> {
    let mut regions = Vec::new();

    // Echo content via tree-sitter Blade grammar. Captures both `{{ }}` and
    // `{!! !!}` because both produce `php_statement > php_only` nodes.
    if let Ok(tree) = parse_blade(source) {
        let lang = language_blade();
        if let Ok(blade_patterns) = extract_all_blade_patterns(&tree, source, &lang) {
            for echo in blade_patterns.echo_php {
                regions.push(BladePhpRegion {
                    content: echo.php_content.to_string(),
                    row: echo.row as u32,
                    column: echo.column as u32,
                    byte_offset: echo.byte_start,
                });
            }
        }
        // Bound-attribute (`:icon="$post->x"`) and directive-attribute
        // (`@class(['x' => $post->y])`) expressions — PHP that lives in HTML
        // attribute values, which the echo/`@php` passes don't reach. Reuses
        // the parse above.
        collect_attribute_php_regions(tree.root_node(), source, &mut regions);
    }

    // `@php ... @endphp` blocks. The Blade grammar's representation here is
    // uneven across tree-sitter-blade versions, so we lean on regex to get
    // byte-accurate body positions.
    regions.extend(extract_php_block_regions(source));

    // `@php(...)` inline form. Needs a balanced-paren walker because the
    // body may contain its own parens (`@php($x = route('home', ['a']))`).
    // Regex can't do this; the walker is small and self-contained.
    regions.extend(extract_php_inline_regions(source));

    // Dedupe by byte_offset: tree-sitter-blade's `_raw` rule (which fires
    // for `@php ... @endphp` blocks) also produces a `php_only` node, so
    // the tree-sitter pass above AND the regex pass below both capture the
    // same block region. Without dedup, downstream consumers re-process
    // the same content twice — historically harmless for routes/views but
    // problematic for chain extraction, where duplicate chains pollute the
    // position index. Keep the first occurrence (typically the tree-sitter
    // one, which has the same content but is captured earlier here).
    regions.sort_by_key(|r| (r.byte_offset, r.content.len()));
    regions.dedup_by(|a, b| a.byte_offset == b.byte_offset);

    // Stable sort by row/column for the public ordering contract.
    regions.sort_by_key(|r| (r.row, r.column));
    regions
}

/// Collect PHP expressions embedded in HTML attribute values into `regions`:
///
/// - **Bound attributes** — `:icon="$post->is_published ? 'a' : 'b'"`. The
///   `attribute_value` text is a PHP expression. (`::escaped` is a literal —
///   skipped. `wire:`/`x-` are Livewire/Alpine property-path/JS strings, not
///   PHP expressions — skipped.)
/// - **Directive attributes** — `@class(['x' => $post->active])`,
///   `@checked($post->active)`. The `parameter` text is PHP.
///
/// The region position is the expression's own start, so the standard
/// `<?php `-wrap + [`adjust_inner_position`] maps captures back to the file.
fn collect_attribute_php_regions(
    root: tree_sitter::Node,
    source: &str,
    regions: &mut Vec<BladePhpRegion>,
) {
    let bytes = source.as_bytes();
    let mut push_region = |node: tree_sitter::Node| {
        if let Ok(content) = node.utf8_text(bytes) {
            if content.is_empty() {
                return;
            }
            let start = node.start_position();
            regions.push(BladePhpRegion {
                content: content.to_string(),
                row: start.row as u32,
                column: start.column as u32,
                byte_offset: node.start_byte(),
            });
        }
    };

    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "attribute" {
            // Gather the relevant child nodes in one pass (avoids nested
            // cursor borrows), then act.
            let mut name_node = None;
            let mut quoted = None;
            let mut param = None;
            let mut c = n.walk();
            for ch in n.children(&mut c) {
                match ch.kind() {
                    "attribute_name" => name_node = Some(ch),
                    "quoted_attribute_value" => quoted = Some(ch),
                    "parameter" => param = Some(ch),
                    _ => {}
                }
            }

            // Bound attribute (`:attr`, not literal `::attr`): the value is PHP.
            let bound = name_node
                .and_then(|nm| nm.utf8_text(bytes).ok())
                .map(|s| s.starts_with(':') && !s.starts_with("::"))
                .unwrap_or(false);
            if bound {
                if let Some(q) = quoted {
                    let mut vc = q.walk();
                    let val = q.children(&mut vc).find(|x| x.kind() == "attribute_value");
                    if let Some(v) = val {
                        push_region(v);
                    }
                }
            }

            // Directive attribute (`@class([...])`, `@checked($x)`): param is PHP.
            if let Some(p) = param {
                push_region(p);
            }
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
}

/// Map a snippet-local `(row, col)` produced by tree-sitter on a
/// `<?php `-wrapped Blade region back to its Blade-file position.
///
/// On snippet row 0, columns include the wrapper prefix and must be shifted
/// left. On subsequent rows the column is preserved as-is because the
/// wrapper only affects the first line. The row offset is the region's
/// base row plus the snippet-internal row.
pub fn adjust_inner_position(
    inner_row: u32,
    inner_col: u32,
    region_row: u32,
    region_col: u32,
) -> (u32, u32) {
    let line = region_row + inner_row;
    let column = if inner_row == 0 {
        region_col + inner_col.saturating_sub(PHP_WRAPPER_PREFIX_LEN)
    } else {
        inner_col
    };
    (line, column)
}

/// Find `@php ... @endphp` blocks and return them as PHP regions with
/// positions in the Blade source.
///
/// Inline `@php(expression)` form is intentionally skipped: the body can
/// contain its own parentheses (`@php($x = route('home'))`), and a regex
/// match would stop at the first inner `)`. A balanced-paren walker would
/// recover it but the inline form is rare in practice — block form is the
/// dominant pattern. Follow-up if needed.
fn extract_php_block_regions(source: &str) -> Vec<BladePhpRegion> {
    use lazy_static::lazy_static;
    use regex::Regex;

    lazy_static! {
        static ref BLOCK_RE: Regex = Regex::new(r"(?s)@php\b\s*(.*?)\s*@endphp").unwrap();
    }

    let mut regions = Vec::new();
    for cap in BLOCK_RE.captures_iter(source) {
        let body_match = match cap.get(1) {
            Some(m) => m,
            None => continue,
        };
        let (row, col) = byte_offset_to_row_col(source, body_match.start());
        regions.push(BladePhpRegion {
            content: body_match.as_str().to_string(),
            row,
            column: col,
            byte_offset: body_match.start(),
        });
    }
    regions
}

/// Find `@php(expression)` inline regions. Walks balanced parentheses so
/// inner parens (`@php($x = route('home', ['a']))`) are matched correctly.
/// String content is paren-skipped so embedded `(` / `)` inside `'...'` or
/// `"..."` don't throw off the counter. Escapes inside strings are honoured.
///
/// Returns the PHP body (the expression between `@php(` and the matching
/// `)`), along with its row/column/byte_offset in the Blade source.
fn extract_php_inline_regions(source: &str) -> Vec<BladePhpRegion> {
    let bytes = source.as_bytes();
    let needle = b"@php(";
    let mut regions = Vec::new();
    let mut search_from = 0usize;

    while let Some(rel) = find_subslice(bytes, needle, search_from) {
        let body_start = rel + needle.len();
        let Some(body_end) = match_balanced_paren_close(bytes, body_start) else {
            // Unmatched paren — skip this `@php(` and keep searching.
            search_from = body_start;
            continue;
        };
        // Skip if body is empty — nothing to parse.
        if body_end > body_start {
            let content = source[body_start..body_end].to_string();
            let (row, col) = byte_offset_to_row_col(source, body_start);
            regions.push(BladePhpRegion {
                content,
                row,
                column: col,
                byte_offset: body_start,
            });
        }
        // Continue scanning after the closing `)` so nested `@php(` calls
        // (rare but legal) are still found.
        search_from = body_end + 1;
    }

    regions
}

/// Find the first occurrence of `needle` in `haystack` starting at `from`.
/// Returns the byte offset of the first byte of the match.
fn find_subslice(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// Walk forward from `start` (the byte AFTER an opening `(`) looking for the
/// matching `)` at depth zero. Single/double quoted strings are treated as
/// opaque blocks — parens inside them don't affect depth, and backslash
/// escapes inside double-quoted strings advance past the next byte.
fn match_balanced_paren_close(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth: i32 = 1;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            b'\'' => {
                // Single-quoted string: only `\'` and `\\` are escapes in PHP.
                i += 1;
                while i < bytes.len() && bytes[i] != b'\'' {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        i += 2;
                        continue;
                    }
                    i += 1;
                }
            }
            b'"' => {
                // Double-quoted string: more escape sequences, but for paren
                // matching we only care about skipping the string contents.
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        i += 2;
                        continue;
                    }
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Convert a UTF-8 byte offset into a `(row, column)` tuple, both 0-based.
/// Used by the regex path that doesn't get positions from tree-sitter.
/// Column is the byte offset on the row — for ASCII this matches the
/// character column tree-sitter would report.
fn byte_offset_to_row_col(source: &str, byte_offset: usize) -> (u32, u32) {
    let mut row = 0u32;
    let mut last_newline = 0usize;
    for (i, ch) in source.char_indices() {
        if i >= byte_offset {
            break;
        }
        if ch == '\n' {
            row += 1;
            last_newline = i + 1;
        }
    }
    let col = (byte_offset.saturating_sub(last_newline)) as u32;
    (row, col)
}

#[cfg(test)]
mod tests;
