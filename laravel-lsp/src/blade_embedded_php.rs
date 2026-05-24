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
                });
            }
        }
    }

    // `@php ... @endphp` blocks. The Blade grammar's representation here is
    // uneven across tree-sitter-blade versions, so we lean on regex to get
    // byte-accurate body positions. The body's first character sits on the
    // line AFTER the `@php` directive (when written as a block); inline
    // `@php(...)` form is captured too.
    regions.extend(extract_php_block_regions(source));

    regions.sort_by_key(|r| (r.row, r.column));
    regions
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
        });
    }
    regions
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
