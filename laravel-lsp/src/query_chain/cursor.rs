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
use super::methods::{
    is_from_opaque, is_from_replace, is_from_sub, is_subquery_join, is_table_join,
};
use std::sync::Arc;
use tower_lsp::lsp_types::Position;

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

/// Inverse of [`position_to_byte_offset`]: translate a byte offset inside
/// `content` into an LSP `Position` (0-based line + character). Characters are
/// counted as Unicode code points, matching `position_to_byte_offset` — so a
/// round-trip through both functions is stable for ASCII Laravel source. A
/// `byte_offset` past the end of `content` clamps to the final position; an
/// offset that lands mid-codepoint (only possible from a buggy caller) falls
/// back to column 0 on its line rather than panicking.
///
/// Used by the diagnostics path, which carries byte spans out of the chain
/// extractor and needs LSP `Range`s to attach squiggles.
pub fn byte_offset_to_position(content: &str, byte_offset: usize) -> Position {
    let clamped = byte_offset.min(content.len());
    let mut line: u32 = 0;
    let mut line_start = 0usize;
    for (idx, b) in content.bytes().enumerate() {
        if idx >= clamped {
            break;
        }
        if b == b'\n' {
            line += 1;
            line_start = idx + 1;
        }
    }
    let character = content
        .get(line_start..clamped)
        .map(|slice| slice.chars().count() as u32)
        .unwrap_or(0);
    Position { line, character }
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
    detect_in_chain(chains, chain, byte_offset)
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
    detect_in_chain(chains, chain, byte_offset).ok_or(ChainResolveFailure::InChain { chain })
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

/// Resolve the initial chain context from the receiver alone, before any links
/// are walked. Returns `(mode, effective_table, effective_model,
/// closure_relation_hop)`, or `None` when the receiver can't be resolved
/// synchronously (unknown receiver, or an instance var with no declared type
/// and no enclosing closure binding).
///
/// - DbTable carries the table directly; no model lookup needed.
/// - Eloquent static receivers (`User::where(...)`) carry the class FQCN; the
///   handler resolves it to a model/table async (file I/O). `effective_table`
///   stays `None` here.
/// - Eloquent instance receivers (`$user->newQuery()`) resolve via an
///   enclosing relation/same-model closure binding, or a declared `@var` /
///   typed-param type captured at extraction time.
fn initial_receiver_context(
    chains: &[Arc<BuilderChain>],
    chain: &BuilderChain,
) -> Option<(BuilderMode, Option<String>, Option<String>, Option<String>)> {
    let resolved = match &chain.receiver {
        ChainReceiver::DbTable { table, .. } => {
            (BuilderMode::BaseBuilder, Some(table.clone()), None, None)
        }
        ChainReceiver::Eloquent(EloquentReceiver::StaticModel(class)) => (
            BuilderMode::EloquentBuilder,
            None,
            Some(class.clone()),
            None,
        ),
        ChainReceiver::Eloquent(EloquentReceiver::InstanceVar { var, php_type }) => {
            // Phase 8: a `$q` receiver bound by an enclosing closure carrier
            // inherits the parent chain's model — RelationHop (`whereHas('rel',
            // closure)`) walks one relation hop; SameModel (`where(closure)`,
            // `when(...)`, …) inherits directly. Phase 9 fallback: the resolved
            // `php_type` from a typed param / `@var` docblock.
            match chain.closure_scope.as_ref() {
                Some(binding) if &binding.param_var == var => match &binding.kind {
                    ClosureScopeKind::RelationHop { relation_name } => {
                        let parent_model = parent_chain_eloquent_model(chains, chain)?;
                        (
                            BuilderMode::EloquentBuilder,
                            None,
                            Some(parent_model),
                            Some(relation_name.clone()),
                        )
                    }
                    ClosureScopeKind::SameModel => {
                        let parent_model = parent_chain_eloquent_model(chains, chain)?;
                        (BuilderMode::EloquentBuilder, None, Some(parent_model), None)
                    }
                    // `$join` is a base query builder rooted at the joined
                    // table. The table itself is supplied via the from_clause
                    // override ([`closure_join_from_override`]); here we only
                    // set the mode (and don't need the parent model, so a
                    // `DB::table()` parent works too).
                    ClosureScopeKind::JoinTable { .. } => {
                        (BuilderMode::BaseBuilder, None, None, None)
                    }
                },
                _ => match php_type {
                    Some(class) => (
                        BuilderMode::EloquentBuilder,
                        None,
                        Some(class.clone()),
                        None,
                    ),
                    None => return None,
                },
            }
        }
        ChainReceiver::Unknown => return None,
    };
    Some(resolved)
}

/// Resolve the chain context at a specific link index, applying the effects of
/// all links *before* `link_idx` to the running mode. Unlike
/// [`detect_chain_context_at`], this does NOT require the link to expose a
/// recognised string argument — it's for diagnostics on dynamic
/// `where{Column}` finders, whose column lives in the method name rather than
/// an argument. `expecting` is set to the target link's `ArgKind` (often
/// `None` for dynamic finders). Returns `None` if a prior link terminated the
/// chain or the receiver can't be resolved synchronously.
pub fn chain_context_for_link(
    chains: &[Arc<BuilderChain>],
    chain: &BuilderChain,
    link_idx: usize,
) -> Option<ChainContext> {
    let (mut mode, effective_table, effective_model, closure_relation_hop) =
        initial_receiver_context(chains, chain)?;
    for link in chain.links.iter().take(link_idx) {
        match link.effect {
            ChainEffect::FlipToBase => mode = BuilderMode::BaseBuilder,
            ChainEffect::FlipToCollection => {
                if mode == BuilderMode::EloquentBuilder {
                    mode = BuilderMode::EloquentCollection;
                }
            }
            ChainEffect::Terminate => return None,
            ChainEffect::None => {}
        }
    }
    let arg = chain.links.get(link_idx)?.arg;
    let (mut joined_tables, mut from_clause) = scan_accessible_tables(chain);
    let mut join_parent_model = None;
    if let Some(override_clause) = closure_join_from_override(chain) {
        let own_qualifier = match &override_clause {
            FromClause::Replace(t) => t.qualifier().to_string(),
            _ => String::new(),
        };
        from_clause = override_clause;
        let (parent_tables, parent_model) =
            join_closure_parent_tables(chains, chain, &own_qualifier);
        joined_tables.extend(parent_tables);
        join_parent_model = parent_model;
    }
    Some(ChainContext {
        mode,
        effective_table,
        effective_model,
        expecting: arg,
        dotted_prefix: None,
        closure_relation_hop,
        quote: '\'',
        joined_tables,
        from_clause,
        join_parent_model,
    })
}

/// The string literal the cursor sits in, plus the resolved [`ChainContext`].
/// Goto-definition needs the literal's value (which column/relation/table) and
/// its span — neither of which `ChainContext` carries — so the cursor resolver
/// surfaces them here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainTarget {
    pub ctx: ChainContext,
    /// The unquoted literal under the cursor (`"posts.author"`, `"email"`, …).
    pub value: String,
    /// Byte span of the string literal *including* its quotes.
    pub value_span: (usize, usize),
}

fn detect_in_chain(
    chains: &[Arc<BuilderChain>],
    chain: &BuilderChain,
    byte_offset: usize,
) -> Option<ChainContext> {
    detect_target_in_chain(chains, chain, byte_offset).map(|t| t.ctx)
}

fn detect_target_in_chain(
    chains: &[Arc<BuilderChain>],
    chain: &BuilderChain,
    byte_offset: usize,
) -> Option<ChainTarget> {
    let (mut mode, effective_table, effective_model, closure_relation_hop) =
        initial_receiver_context(chains, chain)?;

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
    let (quote, string_value, value_span) = string_arg_at(cursor_link, byte_offset)?;

    // Position-aware classification: most methods carry one ArgKind for the
    // whole link, but join/`from` methods name a TABLE in their first string
    // arg (`crossJoin('|')`, `from('|')`) — so we offer table completion there,
    // just like `DB::table('|')`. Inside a join closure, `$join->on(...)` takes
    // columns (`in_join_clause`).
    let expecting = expecting_at(cursor_link, byte_offset, is_join_clause_chain(chain));

    if !matches!(
        expecting,
        ArgKind::Column | ArgKind::Relation | ArgKind::ClosureCarrier | ArgKind::Table
    ) {
        return None;
    }

    // Dotted prefixes — everything before the LAST dot of the literal.
    // - Relation / ClosureCarrier (`with('posts.author.|')`): the relation
    //   chain to walk ("posts" → Post, then "author" on Post → Author).
    // - Column (`where('orders.|')`): the table qualifier (alias or table
    //   name) the column belongs to, so completion narrows to that one
    //   table. Joins make qualified columns meaningful (`orders.status`);
    //   without joins it's still harmless (the qualifier just matches the
    //   root table or nothing).
    // Table args stay single-segment.
    let dotted_prefix = if matches!(
        expecting,
        ArgKind::Relation | ArgKind::ClosureCarrier | ArgKind::Column
    ) {
        string_value
            .rfind('.')
            .map(|idx| string_value[..idx].to_string())
    } else {
        None
    };

    let (mut joined_tables, mut from_clause) = scan_accessible_tables(chain);
    let mut join_parent_model = None;
    if let Some(override_clause) = closure_join_from_override(chain) {
        let own_qualifier = match &override_clause {
            FromClause::Replace(t) => t.qualifier().to_string(),
            _ => String::new(),
        };
        from_clause = override_clause;
        // Inside a join closure, the parent query's tables are also
        // referenceable (both sides of the ON clause).
        let (parent_tables, parent_model) =
            join_closure_parent_tables(chains, chain, &own_qualifier);
        joined_tables.extend(parent_tables);
        join_parent_model = parent_model;
    }

    Some(ChainTarget {
        ctx: ChainContext {
            mode,
            effective_table,
            effective_model,
            expecting,
            dotted_prefix,
            closure_relation_hop,
            quote,
            joined_tables,
            from_clause,
            join_parent_model,
        },
        value: string_value,
        value_span,
    })
}

/// Resolve a cursor to the chain literal under it plus its [`ChainContext`].
/// Like [`detect_chain_context_at`], but also returns the literal's value and
/// span — used by goto-definition to route the literal to its source.
pub fn detect_chain_target_at(
    chains: &[Arc<BuilderChain>],
    byte_offset: usize,
) -> Option<ChainTarget> {
    let chain = find_chain_containing(chains, byte_offset)?;
    detect_target_in_chain(chains, chain, byte_offset)
}

/// Find the smallest chain in `chains` whose span *strictly* contains
/// `child`'s span — the innermost enclosing chain (the one whose method
/// invoked the closure `child`'s receiver is bound by).
fn parent_chain<'a>(
    chains: &'a [Arc<BuilderChain>],
    child: &BuilderChain,
) -> Option<&'a BuilderChain> {
    let (cs, ce) = child.span_byte_range;
    let mut best: Option<&BuilderChain> = None;
    for chain in chains {
        let (ps, pe) = chain.span_byte_range;
        // Strictly contains (not equal — that'd be the child itself).
        if ps <= cs && pe >= ce && (ps, pe) != (cs, ce) {
            let new_size = pe - ps;
            let pick = match best {
                None => true,
                Some(b) => new_size < (b.span_byte_range.1 - b.span_byte_range.0),
            };
            if pick {
                best = Some(chain.as_ref());
            }
        }
    }
    best
}

/// Phase 8 helper: the Eloquent model class of the innermost chain enclosing
/// `child`. Used when a child chain's receiver is `$q` bound by an enclosing
/// `whereHas` / `with` closure — the parent's model is the starting point for
/// the relation hop the handler will perform.
fn parent_chain_eloquent_model(
    chains: &[Arc<BuilderChain>],
    child: &BuilderChain,
) -> Option<String> {
    match &parent_chain(chains, child)?.receiver {
        ChainReceiver::Eloquent(EloquentReceiver::StaticModel(class)) => Some(class.clone()),
        // Future: also support `(new self)` receivers etc. — already
        // produce StaticModel via Phase 5.1.
        _ => None,
    }
}

/// The tables the *parent* query contributes inside a join closure (issue
/// #24), so `$join->on('orders.id', '=', 'users.|')` completes the `users`
/// side. Returns `(sync_tables, parent_model)`:
///
/// - `sync_tables` — the parent's root when it's a `DB::table()` / `from()`
///   table, plus the parent's other joins. These resolve without I/O.
/// - `parent_model` — set when the parent is rooted at an Eloquent model whose
///   table needs async resolution; consumers fold it in (see
///   `ChainContext::join_parent_model`).
///
/// The join's *own* table (`own_qualifier`) is excluded — it's already the
/// inner chain's root.
fn join_closure_parent_tables(
    chains: &[Arc<BuilderChain>],
    chain: &BuilderChain,
    own_qualifier: &str,
) -> (Vec<AccessibleTable>, Option<String>) {
    let Some(parent) = parent_chain(chains, chain) else {
        return (Vec::new(), None);
    };
    let (parent_joins, parent_from) = scan_accessible_tables(parent);

    let mut tables = Vec::new();
    let mut parent_model = None;
    match &parent_from {
        FromClause::Replace(table) => tables.push(table.clone()),
        FromClause::Opaque => {}
        FromClause::Inherit => match &parent.receiver {
            ChainReceiver::DbTable { table, .. } => {
                tables.push(AccessibleTable::bare(table.clone()));
            }
            ChainReceiver::Eloquent(EloquentReceiver::StaticModel(class)) => {
                parent_model = Some(class.clone());
            }
            ChainReceiver::Eloquent(EloquentReceiver::InstanceVar {
                php_type: Some(class),
                ..
            }) => {
                parent_model = Some(class.clone());
            }
            _ => {}
        },
    }
    tables.extend(parent_joins);
    // Drop the join's own table — it's the inner root, not a sibling.
    tables.retain(|t| t.qualifier() != own_qualifier);
    (tables, parent_model)
}

/// Scan ALL links in the chain and collect the tables made accessible by
/// `join()`-family calls, plus any `from*()` root override.
///
/// Visibility is **global**: Laravel compiles the entire chain into one SQL
/// query, so a join appearing anywhere in the chain makes its table
/// referenceable from every column position — we don't gate on whether the
/// join textually precedes the cursor. (Mode flips like `toBase()` stay
/// positional; that's a separate concern handled by the link walk.)
///
/// The last `from*()` wins, mirroring Laravel's runtime where a later
/// `from()` overrides an earlier FROM clause.
fn scan_accessible_tables(chain: &BuilderChain) -> (Vec<AccessibleTable>, FromClause) {
    let mut joined = Vec::new();
    let mut from_clause = FromClause::Inherit;
    for link in &chain.links {
        let method = link.method.as_str();
        if is_table_join(method) {
            if let Some(table_ref) = first_string_arg_value(link) {
                joined.push(parse_table_ref(&table_ref));
            }
        } else if is_from_replace(method) {
            if let Some(table_ref) = first_string_arg_value(link) {
                from_clause = FromClause::Replace(parse_table_ref(&table_ref));
            }
        } else if is_from_opaque(method) {
            from_clause = FromClause::Opaque;
        } else if is_from_sub(method) {
            // `fromSub($query, 'alias')` — virtual root from the subquery's
            // SELECT list. Unknown columns (no usable SELECT) → opaque root.
            from_clause = match (first_string_arg_value(link), &link.subquery_columns) {
                (Some(alias), Some(cols)) if !cols.is_empty() => {
                    FromClause::Replace(AccessibleTable::virtual_table(alias, cols.clone()))
                }
                _ => FromClause::Opaque,
            };
        } else if is_subquery_join(method) {
            // `joinSub($query, 'alias', …)` — virtual joined table. Skip when
            // the subquery's columns are unknown (offer nothing for it).
            if let (Some(alias), Some(cols)) =
                (first_string_arg_value(link), &link.subquery_columns)
            {
                if !cols.is_empty() {
                    joined.push(AccessibleTable::virtual_table(alias, cols.clone()));
                }
            }
        }
    }
    (joined, from_clause)
}

/// If this chain's receiver `$var` is bound by an enclosing join closure
/// (`join('orders', fn ($var) => $var->where(…))`), the `$var` `JoinClause`
/// builder is rooted at the joined table — model it as a `from(orders)`
/// override so column completion/narrowing inside the closure resolves
/// against that table (alias included). Returns `None` for any other chain.
fn closure_join_from_override(chain: &BuilderChain) -> Option<FromClause> {
    let var = match &chain.receiver {
        ChainReceiver::Eloquent(EloquentReceiver::InstanceVar { var, .. }) => var,
        _ => return None,
    };
    let binding = chain.closure_scope.as_ref()?;
    if &binding.param_var != var {
        return None;
    }
    match &binding.kind {
        ClosureScopeKind::JoinTable { table_ref } => {
            Some(FromClause::Replace(parse_table_ref(table_ref)))
        }
        _ => None,
    }
}

/// The value of a link's first `StringLit` argument, if any. Used to read the
/// table name out of a `join('orders', …)` / `from('admins')` call. Closures
/// and other arg shapes are skipped — `join('orders', fn ($j) => …)` still
/// returns `"orders"` because the closure is a later arg.
fn first_string_arg_value(link: &ChainLink) -> Option<String> {
    link.args.iter().find_map(|arg| match arg {
        ChainArg::StringLit { value, .. } => Some(value.clone()),
        _ => None,
    })
}

/// What the cursor's link expects at this byte position. Most links carry a
/// single `ArgKind` for the whole call, but join/`from` methods are
/// position-sensitive:
///
/// - **Join methods** (`join('orders', 'orders.user_id', '=', 'users.id')`):
///   the FIRST string arg names a table → [`ArgKind::Table`] (table-name
///   completion, like `DB::table('|')`). Every later arg is part of the ON
///   condition, which references the accessible tables → [`ArgKind::Column`]
///   (`join('orders', 'orders.|')` offers `orders` columns). The operator /
///   value slots over-offer harmlessly, exactly as `where('x', '=', '|')`
///   already does — the editor filters them out.
/// - **`from('admins')`**: the first arg names the new root table →
///   [`ArgKind::Table`]; any later arg (e.g. a connection name) →
///   [`ArgKind::None`].
/// - **`on`/`orOn` on a JoinClause** (`$join->on('orders.id', '=', 'users.id')`
///   inside a join closure, `in_join_clause = true`): the ON-condition args
///   are columns → [`ArgKind::Column`]. This is disambiguated by receiver: a
///   *member* call on the query builder takes columns, whereas the *static*
///   `Model::on('mysql')` connection-setter (`in_join_clause = false`) is left
///   as [`ArgKind::None`] — not a column.
///
/// Non-join/from links fall through to the link's own `arg` classification.
fn expecting_at(link: &ChainLink, byte_offset: usize, in_join_clause: bool) -> ArgKind {
    if is_table_join(&link.method) {
        return if cursor_in_first_string_arg(link, byte_offset) {
            ArgKind::Table
        } else {
            ArgKind::Column
        };
    }
    if is_from_replace(&link.method) {
        return if cursor_in_first_string_arg(link, byte_offset) {
            ArgKind::Table
        } else {
            ArgKind::None
        };
    }
    if in_join_clause && matches!(link.method.as_str(), "on" | "orOn") {
        return ArgKind::Column;
    }
    link.arg
}

/// Whether this chain is the body of a join closure — its receiver `$join` is
/// bound by an enclosing `join('orders', fn ($join) => …)`. Inside it, the
/// builder is a `JoinClause`, so `on`/`orOn` take columns (see
/// [`expecting_at`]).
fn is_join_clause_chain(chain: &BuilderChain) -> bool {
    matches!(
        chain.closure_scope.as_ref().map(|b| &b.kind),
        Some(ClosureScopeKind::JoinTable { .. })
    )
}

/// Whether `byte_offset` falls inside the link's FIRST string-literal arg —
/// the table-name slot of a join/`from` call. A cursor in a later string arg
/// (the join condition) returns `false`.
fn cursor_in_first_string_arg(link: &ChainLink, byte_offset: usize) -> bool {
    for arg in &link.args {
        if let ChainArg::StringLit {
            span_byte_range, ..
        } = arg
        {
            let (start, end) = *span_byte_range;
            return byte_offset >= start && byte_offset <= end;
        }
    }
    false
}

/// Find the string-literal arg of `link` that contains the cursor, returning
/// its quote character, value, and byte span (incl. quotes). Walks both
/// top-level `StringLit` args and string literals nested inside an `Array` arg,
/// so `with(['posts'|, 'comments'])` resolves the same as `with('posts'|)`.
/// Returns `None` if no string arg covers the cursor.
fn string_arg_at(link: &ChainLink, byte_offset: usize) -> Option<(char, String, (usize, usize))> {
    for arg in &link.args {
        if let Some(found) = string_arg_in(arg, byte_offset) {
            return Some(found);
        }
    }
    None
}

/// Check whether a single arg (or any of its nested string elements, for
/// `Array` args) contains the cursor and is a string literal. Returns the
/// quote character, literal value, and byte span on hit. Pulled out as a
/// separate helper so `Array` can recurse into its elements without
/// duplicating the span check.
fn string_arg_in(arg: &ChainArg, byte_offset: usize) -> Option<(char, String, (usize, usize))> {
    match arg {
        ChainArg::StringLit {
            quote,
            value,
            span_byte_range,
        } => {
            let (start, end) = *span_byte_range;
            if byte_offset >= start && byte_offset <= end {
                Some((*quote, value.clone(), *span_byte_range))
            } else {
                None
            }
        }
        ChainArg::Array { elements, .. } => {
            for elem in elements {
                if let Some(found) = string_arg_in(elem, byte_offset) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
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
