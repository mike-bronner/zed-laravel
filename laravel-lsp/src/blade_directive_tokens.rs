//! Blade directive semantic-token extraction for LSP highlighting.
//!
//! Zed's tree-sitter Blade grammar highlights a fixed set of directives plus
//! generic *paired* (`@foo … @endfoo`) forms, but it cannot colour custom
//! *inline* directives an app registers via `Blade::directive()`. This module
//! produces LSP semantic tokens (all `FUNCTION`-typed) for the directives in a
//! buffer so that, with `"semantic_tokens": "combined"`, those custom inline
//! directives highlight like first-class ones.
//!
//! Two guards keep it precise — the highlighter is only as good as the names it
//! trusts and the regions it ignores:
//!
//! 1. **Known-set filter** — a `@word` is tokenised only if its name (without
//!    the leading `@`, compared case-insensitively) is in the caller-supplied
//!    set. That set is the standard directives ∪ the names scanned from real
//!    `Blade::directive()` registrations, so non-directive `@`-text is rejected:
//!    `@param` in a PHPDoc block, `@media` in inline CSS, or the `@example` in
//!    an email address.
//! 2. **Comment exclusion** — matches inside Blade `{{-- … --}}` or HTML
//!    `<!-- … -->` comments are skipped, so a commented-out directive stays
//!    dark instead of lighting up.
//!
//! Positions are 0-based throughout (LSP convention). Columns are byte offsets
//! from the line start, matching the rest of the LSP's token handling — Blade
//! directives are ASCII, so this stays correct for the tokens we emit.

use std::collections::HashSet;

use lazy_static::lazy_static;
use tower_lsp::lsp_types::SemanticToken;
use regex::Regex;

lazy_static! {
    /// A `@`-prefixed directive candidate: `@` followed by a PHP-identifier-shaped
    /// name. Widened from `@[a-zA-Z]+` to include digits and underscores (e.g.
    /// `@feature2`, `@my_directive`) so custom names match; the known-set filter
    /// stops the wider pattern from over-matching.
    static ref DIRECTIVE_RE: Regex = Regex::new(r"@[a-zA-Z_][a-zA-Z0-9_]*").unwrap();

    /// Blade (`{{-- --}}`) and HTML (`<!-- -->`) comment spans. Non-greedy so
    /// neighbouring comments don't merge into one; `(?s)` lets a comment span
    /// multiple lines.
    static ref COMMENT_RE: Regex = Regex::new(r"(?s)\{\{--.*?--\}\}|<!--.*?-->").unwrap();
}

/// Byte ranges (`start..end`) of Blade and HTML comments in `content`.
pub fn blade_comment_spans(content: &str) -> Vec<(usize, usize)> {
    COMMENT_RE
        .find_iter(content)
        .map(|m| (m.start(), m.end()))
        .collect()
}

/// Whether byte offset `pos` falls inside any of the given comment spans.
fn in_comment(pos: usize, spans: &[(usize, usize)]) -> bool {
    spans.iter().any(|&(start, end)| pos >= start && pos < end)
}

/// Find the directives to highlight as 0-based `(line, start_column, length)`
/// triples. A `@word` is included only when it is not inside a comment and its
/// name is present in `known` (which must already be lowercased).
pub fn directive_token_positions(content: &str, known: &HashSet<String>) -> Vec<(u32, u32, u32)> {
    let comment_spans = blade_comment_spans(content);

    // Byte offset of each line start, for mapping a match offset to line/column.
    let mut line_starts: Vec<usize> = vec![0];
    for (i, &b) in content.as_bytes().iter().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }

    let mut positions = Vec::new();

    for mat in DIRECTIVE_RE.find_iter(content) {
        let start_byte = mat.start();

        // Skip directives sitting inside a Blade/HTML comment.
        if in_comment(start_byte, &comment_spans) {
            continue;
        }

        // Keep only names we recognise (standard or registered custom). The
        // match is ASCII, so slicing past the leading '@' is byte-safe.
        let name = &content[start_byte + 1..mat.end()];
        if !known.contains(&name.to_lowercase()) {
            continue;
        }

        let line = line_starts
            .iter()
            .position(|&start| start > start_byte)
            .map(|i| i - 1)
            .unwrap_or(line_starts.len() - 1) as u32;
        let col = (start_byte - line_starts[line as usize]) as u32;
        let length = mat.len() as u32;

        positions.push((line, col, length));
    }

    positions
}

/// Build delta-encoded LSP semantic tokens (all `FUNCTION`-typed — index 0 in
/// the server's legend) for the Blade directives in `content` that are present
/// in `known`. See the module docs for the filtering rules.
pub fn extract_blade_directive_tokens(
    content: &str,
    known: &HashSet<String>,
) -> Vec<SemanticToken> {
    let positions = directive_token_positions(content, known);

    let mut tokens = Vec::with_capacity(positions.len());
    let mut prev_line: u32 = 0;
    let mut prev_col: u32 = 0;

    for (line, col, length) in positions {
        let delta_line = line - prev_line;
        let delta_start = if delta_line == 0 {
            col - prev_col
        } else {
            col // absolute column on a new line
        };

        tokens.push(SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type: 0, // FUNCTION (index 0 in the server legend)
            token_modifiers_bitset: 0,
        });

        prev_line = line;
        prev_col = col;
    }

    tokens
}

#[cfg(test)]
mod tests;
