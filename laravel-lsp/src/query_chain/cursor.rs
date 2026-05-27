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

/// Result of preparing source content for chain-completion at the cursor.
/// Carries the (possibly fixed) content to parse PLUS metadata the items
/// builder needs to format `insert_text` correctly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionPrep {
    /// Content to feed to tree-sitter for chain extraction. May be the
    /// original `content` unchanged, or modified to balance an unterminated
    /// string or to seed an empty-string arg right after an open paren.
    pub fixed_content: String,
    /// If `Some(q)`, the user is in a position where the source doesn't
    /// yet have a quote pair around the string arg (typically immediately
    /// after `(` was typed). Completion items should wrap their value with
    /// `q` so the inserted text is a valid string literal.
    ///
    /// If `None`, the source already contains the quotes (the cursor is
    /// inside an existing string, fixed-up or not), and items should be
    /// inserted bare without extra quotes.
    pub quote_for_insertion: Option<char>,
}

impl CompletionPrep {
    fn unchanged(content: &str) -> Self {
        Self {
            fixed_content: content.to_string(),
            quote_for_insertion: None,
        }
    }
}

/// Prepare the source for chain-completion at the cursor. Handles three
/// cases the user might be in:
///
/// 1. **Inside an unterminated string** (e.g. `DB::table('|`). Inject the
///    matching close quote so tree-sitter doesn't extend the `string` node
///    thousands of bytes downstream. Insertions stay bare.
///
/// 2. **Inside an already-closed empty/partial string** (e.g. the
///    auto-paired `DB::table('|')`). No fixup — the source parses cleanly
///    and the auto-paired close is right there. Insertions stay bare.
///
/// 3. **Right after `(` with no string yet** (e.g. `DB::table(|`). Inject
///    `''` (or `''` plus `)` if no close paren exists) so the call expression
///    parses with an empty-string first arg. Insertions wrap with `'` because
///    the source doesn't have quotes around the would-be arg.
///
/// Anything else: content unchanged, no quote wrapping.
///
/// We deliberately don't model heredocs/nowdocs — those don't appear in
/// query chain arguments in any real code.
pub fn fixup_for_completion(content: &str, byte_offset: usize) -> Option<CompletionPrep> {
    if byte_offset > content.len() {
        return None;
    }
    let line_start = content[..byte_offset]
        .rfind('\n')
        .map(|n| n + 1)
        .unwrap_or(0);
    let line_to_cursor = &content[line_start..byte_offset];

    // ---- Case 1/2: open quote at cursor? -----------------------------------
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

    if let Some(unclosed) = state {
        // Auto-pair safety: if the matching close quote is already at the
        // cursor (the editor inserted it), don't inject another — that
        // produces `'''` and breaks the parse worse than the original.
        if content[byte_offset..].starts_with(unclosed) {
            // BUT: the editor only auto-pairs the quote, not the surrounding
            // `)`. If the user typed `DB::table('` and the editor inserted
            // the second `'`, the source is `DB::table('')` with no closing
            // paren. Tree-sitter recovers by extending the call expression
            // downstream, which causes the empty-string arg to fall out of
            // the argument list entirely (classified as `Other` instead of
            // `StringLit`). Inject `)` right after the close quote so the
            // call expression terminates cleanly.
            let line_end = content[line_start..]
                .find('\n')
                .map(|n| line_start + n)
                .unwrap_or(content.len());
            let full_line = &content[line_start..line_end];
            if line_paren_balance(full_line) > 0 {
                // Insertion point: one byte past the auto-paired close quote.
                let insert_at = byte_offset + unclosed.len_utf8();
                let mut fixed = String::with_capacity(content.len() + 1);
                fixed.push_str(&content[..insert_at]);
                fixed.push(')');
                fixed.push_str(&content[insert_at..]);
                return Some(CompletionPrep {
                    fixed_content: fixed,
                    // Source already has both quotes — completion items go
                    // in bare.
                    quote_for_insertion: None,
                });
            }
            return Some(CompletionPrep::unchanged(content));
        }

        // The matching close quote isn't at the cursor — inject it. While
        // we're at it, also check whether the cursor's line has unbalanced
        // parens (e.g. user deleted back to `DB::table('trua` — no close
        // quote, no `)`). If so, inject `)` after the close quote so the
        // call expression terminates cleanly. Without the paren patch
        // tree-sitter recovers by extending the call across the next
        // statement, and the string arg falls out of the argument list.
        let line_end = content[line_start..]
            .find('\n')
            .map(|n| line_start + n)
            .unwrap_or(content.len());
        let full_line = &content[line_start..line_end];
        let needs_close_paren = line_paren_balance(full_line) > 0;

        let mut fixed =
            String::with_capacity(content.len() + if needs_close_paren { 2 } else { 1 });
        fixed.push_str(&content[..byte_offset]);
        fixed.push(unclosed);
        if needs_close_paren {
            fixed.push(')');
        }
        fixed.push_str(&content[byte_offset..]);
        return Some(CompletionPrep {
            fixed_content: fixed,
            quote_for_insertion: None, // source already has the open quote
        });
    }

    // ---- Case 3: cursor immediately after `(`? -----------------------------
    // Scan back through any whitespace; if the previous non-whitespace char
    // is `(`, we're in "user just opened the paren" territory. Inject `''`
    // (and `)` if the paren is unclosed) so the parser sees a well-formed
    // call expression with an empty-string first arg. Items will wrap with
    // `'` since the source has no quotes around the arg.
    let trimmed_before: &str = line_to_cursor.trim_end_matches([' ', '\t', '\r']);
    if trimmed_before.ends_with('(') {
        let after_cursor = content[byte_offset..].trim_start_matches([' ', '\t']);
        let inject: &str = if after_cursor.starts_with(')') {
            "''"
        } else {
            "'')"
        };
        let mut fixed = String::with_capacity(content.len() + inject.len());
        fixed.push_str(&content[..byte_offset]);
        fixed.push_str(inject);
        fixed.push_str(&content[byte_offset..]);
        return Some(CompletionPrep {
            fixed_content: fixed,
            quote_for_insertion: Some('\''),
        });
    }

    None
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

/// Count literal `(` minus `)` on a single line. Does NOT try to skip
/// string contents — when the cursor is inside an unclosed string, a
/// string-aware counter treats the rest of the line as "in string" and
/// ignores any `)` there, leading us to inject an unwanted second close
/// paren. The literal count is dumber but more robust: if the line has
/// any `)` at all, even one nominally inside a string, we won't
/// double-inject. The edge case where someone writes `'foo)'` with an
/// unmatched outer paren is rare; worst case we leave the source as-is
/// (degraded behavior, not wrong behavior).
fn line_paren_balance(line: &str) -> i32 {
    let mut balance: i32 = 0;
    for c in line.chars() {
        match c {
            '(' => balance += 1,
            ')' => balance -= 1,
            _ => {}
        }
    }
    balance
}

#[cfg(test)]
mod tests;
