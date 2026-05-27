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
        ArgKind::Column | ArgKind::Relation | ArgKind::ClosureCarrier
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
