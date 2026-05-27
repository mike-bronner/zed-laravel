//! Cursor → [`ChainContext`] resolution.
//!
//! Given the extracted chains for a file and a byte position (typically
//! translated from an LSP `Position`), find the chain that contains the
//! cursor, walk preceding links to determine the effective `BuilderMode` and
//! table, and report what kind of completion to offer at the cursor's link.
//!
//! Phase 3 supports DB-table receivers fully and reports `None` for Eloquent
//! receivers — those need model lookup which is async I/O and lands in
//! later phases.

use super::chain::*;
use std::sync::Arc;

/// At completion time the user is mid-typing and the file often has an
/// unterminated string at the cursor (e.g., `DB::table('|`). Tree-sitter
/// recovers by extending the `string` node up to the next quote anywhere in
/// the file — which is usually thousands of bytes away and produces nonsense
/// spans. Chain extraction sees this and ends up with `ChainArg::Other`
/// instead of a real `StringLit`, so cursor detection fails.
///
/// This helper scans the line up to the cursor, tracks PHP single/double
/// quote balance (with `\` escapes), and returns a content string with a
/// matching closing quote injected at `byte_offset` if a quote is open.
/// Returns `None` if no fixup is needed (the cursor isn't inside an
/// unterminated string).
///
/// We deliberately don't model heredocs/nowdocs — those don't appear in
/// query chain arguments in any real code. We also stop the scan at the
/// previous newline, so quotes that opened on prior lines don't confuse us
/// (multi-line strings inside chain args are vanishingly rare).
pub fn fixup_for_completion(content: &str, byte_offset: usize) -> Option<String> {
    if byte_offset > content.len() {
        return None;
    }
    let line_start = content[..byte_offset]
        .rfind('\n')
        .map(|n| n + 1)
        .unwrap_or(0);
    let line_to_cursor = &content[line_start..byte_offset];

    // Tiny state machine: `None` = not in a string, `Some(q)` = inside a
    // string opened with quote `q`. Escapes consume the next character so
    // `\'` and `\"` inside the matching quote style don't toggle the state.
    let mut state: Option<char> = None;
    let mut iter = line_to_cursor.chars();
    while let Some(c) = iter.next() {
        match (state, c) {
            (None, '\'') => state = Some('\''),
            (None, '"') => state = Some('"'),
            (Some(_), '\\') => {
                // Skip the next char (escaped).
                iter.next();
            }
            (Some(open), close) if open == close => state = None,
            _ => {}
        }
    }

    let unclosed = state?;
    let mut result = String::with_capacity(content.len() + 1);
    result.push_str(&content[..byte_offset]);
    result.push(unclosed);
    result.push_str(&content[byte_offset..]);
    Some(result)
}

/// Translate an LSP `Position` (0-based line + character) into a byte offset
/// inside `content`. Characters are treated as Unicode code points, not
/// UTF-16 code units — this matches every existing position handler in the
/// LSP and works correctly for ASCII Laravel source (which is ~all of it).
/// If `line` is beyond the file or `character` is beyond the line length,
/// the offset clamps to the end of that line.
pub fn position_to_byte_offset(content: &str, line: u32, character: u32) -> Option<usize> {
    // Build a small index of line-start byte offsets up to and including the
    // requested line. Linear scan — the file is already in memory, this is
    // dominated by the byte iteration which is L1-cache-fast.
    let mut line_start = 0usize;
    let mut current_line: u32 = 0;
    for (idx, b) in content.bytes().enumerate() {
        if current_line == line {
            break;
        }
        if b == b'\n' {
            current_line += 1;
            line_start = idx + 1;
        }
    }
    if current_line < line {
        return None;
    }

    let line_end = content[line_start..]
        .find('\n')
        .map(|n| line_start + n)
        .unwrap_or(content.len());

    let line_slice = &content[line_start..line_end];
    let byte_in_line: usize = line_slice
        .chars()
        .take(character as usize)
        .map(char::len_utf8)
        .sum();
    Some(line_start + byte_in_line)
}

/// Resolve a cursor inside a parsed file to a `ChainContext`. Returns `None`
/// when no chain contains the cursor, when the chain's receiver can't be
/// resolved without async I/O (Eloquent — handled by later phases), or when
/// the cursor isn't inside an argument we know how to complete.
pub fn detect_chain_context_at(
    chains: &[Arc<BuilderChain>],
    byte_offset: usize,
) -> Option<ChainContext> {
    let chain = find_chain_containing(chains, byte_offset)?;
    detect_in_chain(chain, byte_offset)
}

/// Diagnostic variant of [`detect_chain_context_at`] that also returns a
/// reason string describing why resolution failed. Used by the LSP handler
/// to emit specific INFO logs (chain not found vs. cursor outside a string
/// arg vs. unsupported receiver). Same logic, just plumbed-through reasons.
pub fn detect_chain_context_at_diagnostic<'a>(
    chains: &'a [Arc<BuilderChain>],
    byte_offset: usize,
) -> Result<ChainContext, ChainResolveFailure<'a>> {
    let chain = find_chain_containing(chains, byte_offset)
        .ok_or(ChainResolveFailure::NoChainAtCursor { chains })?;
    detect_in_chain(chain, byte_offset).ok_or(ChainResolveFailure::InChain { chain })
}

/// Why `detect_chain_context_at_diagnostic` returned no context.
#[derive(Debug)]
pub enum ChainResolveFailure<'a> {
    /// No chain in the file's chain list has a span containing the cursor.
    /// The full chain list is returned so the caller can log spans for
    /// comparison against the cursor byte.
    NoChainAtCursor { chains: &'a [Arc<BuilderChain>] },
    /// A chain contains the cursor but the cursor isn't in a completable
    /// position within it (e.g., between method tokens, or the receiver is
    /// an Eloquent model we can't resolve synchronously yet).
    InChain { chain: &'a BuilderChain },
}

fn find_chain_containing(
    chains: &[Arc<BuilderChain>],
    byte_offset: usize,
) -> Option<&BuilderChain> {
    // Pick the innermost chain — for nested chains (e.g., `$q->where('|')`
    // inside a `whereHas` closure) we want the inner chain. Linear scan is
    // fine; chains-per-file is typically <100.
    let mut best: Option<&BuilderChain> = None;
    for chain in chains {
        let (start, end) = chain.span_byte_range;
        if byte_offset >= start && byte_offset <= end {
            let chain_ref: &BuilderChain = chain.as_ref();
            // Prefer the shorter (more deeply nested) chain.
            match best {
                None => best = Some(chain_ref),
                Some(prev) if (end - start) < (prev.span_byte_range.1 - prev.span_byte_range.0) => {
                    best = Some(chain_ref);
                }
                _ => {}
            }
        }
    }
    best
}

fn detect_in_chain(chain: &BuilderChain, byte_offset: usize) -> Option<ChainContext> {
    // Initial mode + table + model from the receiver. Phase 3 only handles
    // DbTable; Eloquent returns None until model lookup lands.
    let (mut mode, effective_table, effective_model) = match &chain.receiver {
        ChainReceiver::DbTable { table, .. } => {
            (BuilderMode::BaseBuilder, Some(table.clone()), None)
        }
        ChainReceiver::Eloquent(_) => return None,
        ChainReceiver::Unknown => return None,
    };

    // Walk links in source order. For each link, decide: (a) does the cursor
    // sit inside this link? (b) if not, apply this link's effect and move on.
    let mut cursor_link_idx: Option<usize> = None;
    for (idx, link) in chain.links.iter().enumerate() {
        let (start, end) = link.span_byte_range;
        if byte_offset >= start && byte_offset <= end {
            cursor_link_idx = Some(idx);
            break;
        }
        // The cursor is past this link — apply its effect to the running mode.
        match link.effect {
            ChainEffect::FlipToBase => mode = BuilderMode::BaseBuilder,
            ChainEffect::FlipToCollection => {
                // Only Eloquent transitions to Collection; Base stays Base
                // (a base query builder doesn't have a collection variant
                // worth distinguishing here).
                if mode == BuilderMode::EloquentBuilder {
                    mode = BuilderMode::EloquentCollection;
                }
            }
            ChainEffect::Terminate => return None,
            ChainEffect::None => {}
        }
    }

    let cursor_link = &chain.links[cursor_link_idx?];

    // The cursor must be inside a string-literal arg of the cursor link.
    // `pluck` is `ArgKind::Column` even though it terminates the chain —
    // we still complete inside its first arg.
    let (quote, _dotted_prefix) = string_arg_at(cursor_link, byte_offset)?;

    if !matches!(
        cursor_link.arg,
        ArgKind::Column | ArgKind::Relation | ArgKind::ClosureCarrier | ArgKind::Table
    ) {
        return None;
    }

    Some(ChainContext {
        mode,
        effective_table,
        effective_model,
        expecting: cursor_link.arg,
        // Dotted-relation prefix splitting lives in Phase 7. Phase 3 doesn't
        // need it (DB::table chains don't have relation methods), and Phase 4
        // (Eloquent columns) is single-segment.
        dotted_prefix: None,
        quote,
    })
}

/// Find the string-literal arg of `link` that contains the cursor, returning
/// its quote character. Returns `None` if no string arg covers the cursor.
fn string_arg_at(link: &ChainLink, byte_offset: usize) -> Option<(char, ())> {
    for arg in &link.args {
        if let ChainArg::StringLit {
            quote,
            span_byte_range,
            ..
        } = arg
        {
            let (start, end) = *span_byte_range;
            if byte_offset >= start && byte_offset <= end {
                return Some((*quote, ()));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests;
