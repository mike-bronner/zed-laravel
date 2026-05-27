use super::*;
use crate::parser::parse_php;
use crate::query_chain::extract_chains;

/// Parse a snippet, extract chains, and wrap them in Arc to match the shape
/// the caller hands in at runtime (chains live behind Arc inside
/// `ParsedPatternsData`).
fn chains_of(src: &str) -> (String, Vec<Arc<BuilderChain>>) {
    let wrapped = format!("<?php\n{src}");
    let tree = parse_php(&wrapped).expect("parse");
    let chains = extract_chains(&tree, &wrapped)
        .into_iter()
        .map(Arc::new)
        .collect();
    (wrapped, chains)
}

/// Find the byte offset of the single `|` marker inside the wrapped source.
fn cursor_at(src: &str) -> (String, usize) {
    let wrapped = format!("<?php\n{src}");
    let pos = wrapped.find('|').expect("test fixture missing `|` marker");
    // Remove the `|` so the parsed source matches what the user actually has.
    let cleaned = wrapped.replacen('|', "", 1);
    (cleaned, pos)
}

fn detect(src_with_cursor: &str) -> Option<ChainContext> {
    let (wrapped, byte_offset) = cursor_at(src_with_cursor);
    let tree = parse_php(&wrapped).expect("parse");
    let chains: Vec<Arc<BuilderChain>> = extract_chains(&tree, &wrapped)
        .into_iter()
        .map(Arc::new)
        .collect();
    detect_chain_context_at(&chains, byte_offset)
}

// ---- fixup_for_completion ---------------------------------------------

#[test]
fn fixup_returns_none_when_nothing_to_fix() {
    let src = "$x = 1;";
    assert!(fixup_for_completion(src, src.len()).is_none());
}

#[test]
fn fixup_unterminated_quote_injects_close() {
    // No `)` either, so the fixup injects both quote and paren to keep
    // tree-sitter from extending the call expression downstream.
    let src = "DB::table('";
    let prep = fixup_for_completion(src, src.len()).expect("unbalanced single quote");
    assert_eq!(prep.fixed_content, "DB::table('')");
    // Source already had an open quote, so insertion stays bare.
    assert_eq!(prep.quote_for_insertion, None);
}

#[test]
fn fixup_unterminated_double_quote_injects_close() {
    let src = "DB::table(\"";
    let prep = fixup_for_completion(src, src.len()).expect("unbalanced double quote");
    assert_eq!(prep.fixed_content, "DB::table(\"\")");
    assert_eq!(prep.quote_for_insertion, None);
}

#[test]
fn fixup_auto_paired_close_returns_unchanged_no_quoting() {
    // Editor auto-paired: source is `DB::table('')` and cursor is between
    // the two `'`. State scan sees one `'` open at cursor, but the next char
    // IS `'` (auto-paired close), so don't double-inject.
    let src = "DB::table('')";
    let cursor_between_quotes = "DB::table('".len(); // byte right after the first `'`
    let prep = fixup_for_completion(src, cursor_between_quotes)
        .expect("should still return Some for the auto-pair case (just no fixup)");
    assert_eq!(prep.fixed_content, src, "must NOT inject another quote");
    assert_eq!(prep.quote_for_insertion, None);
}

#[test]
fn fixup_auto_paired_quotes_but_unclosed_paren_injects_close_paren() {
    // The real-world Sail-typing case: editor auto-paired the quotes
    // (`DB::table('')`) but the `)` was never typed. Without injecting `)`,
    // tree-sitter recovers by extending the call expression downstream and
    // the `''` arg ends up classified as ChainArg::Other — completion
    // silently misses. We need to leave the cursor's open quote alone but
    // patch the missing paren so the call expression terminates at the
    // close quote.
    let src = "DB::table(''\n        \\Log::info('keep going');\n";
    let cursor_between_quotes = "DB::table('".len();
    let prep =
        fixup_for_completion(src, cursor_between_quotes).expect("auto-pair safety + paren fixup");
    // Insertion stays bare — the source already has the open quote and the
    // editor auto-paired the close.
    assert_eq!(prep.quote_for_insertion, None);
    // The `)` must land right after the auto-paired close quote, so the
    // call expression is `DB::table('')` and the rest of the file parses
    // independently.
    assert!(
        prep.fixed_content.starts_with("DB::table('')"),
        "got: {:?}",
        prep.fixed_content
    );
}

#[test]
fn fixup_unterminated_quote_also_injects_close_paren_when_unbalanced() {
    // The delete-from-end case: user backspaced to `DB::table('trua` and
    // kept typing. No close quote, no `)`. Case-1b previously only
    // injected the `'`, leaving the `(` dangling and tree-sitter free to
    // extend the call expression downstream. The fixup must now also
    // inject `)` after the close quote when the line's parens are
    // unbalanced.
    let src = "DB::table('trua\n        return 0;\n";
    let cursor = "DB::table('trua".len();
    let prep =
        fixup_for_completion(src, cursor).expect("unterminated quote + unbalanced paren fixup");
    // Both `'` and `)` should land — `DB::table('trua')` — so the call
    // terminates on this line.
    assert!(
        prep.fixed_content.starts_with("DB::table('trua')"),
        "expected close quote + close paren injected; got: {:?}",
        prep.fixed_content
    );
    // Source has the open quote; insertion goes in bare.
    assert_eq!(prep.quote_for_insertion, None);
}

#[test]
fn fixup_unterminated_quote_only_when_parens_already_balanced() {
    // If the call's parens are already balanced (e.g. the `)` is on the
    // same line later because the editor auto-paired it), don't
    // double-inject `)`.
    let src = "DB::table('trua)";
    let cursor = "DB::table('trua".len();
    let prep = fixup_for_completion(src, cursor).expect("unterminated quote only");
    // Just `'` — no extra `)` since the line already has a closing paren.
    assert_eq!(prep.fixed_content, "DB::table('trua')");
    assert_eq!(prep.quote_for_insertion, None);
}

#[test]
fn fixup_auto_paired_quotes_with_balanced_parens_stays_unchanged() {
    // Same shape as the bug-fix case above, but the `)` IS already present
    // (full auto-pair, or user typed `)` after seeing autocomplete). Don't
    // inject another `)`.
    let src = "DB::table('')";
    let cursor_between_quotes = "DB::table('".len();
    let prep = fixup_for_completion(src, cursor_between_quotes).expect("balanced");
    assert_eq!(prep.fixed_content, src);
    assert_eq!(prep.quote_for_insertion, None);
}

#[test]
fn fixup_after_open_paren_injects_empty_string_and_close_paren() {
    // User just typed `(`. No quotes yet. Inject `'')` so the call parses
    // with an empty-string first arg AND a closing paren.
    let src = "DB::table(";
    let prep = fixup_for_completion(src, src.len()).expect("after-paren case");
    assert_eq!(prep.fixed_content, "DB::table('')");
    // Source has no quotes, so items must wrap with `'`.
    assert_eq!(prep.quote_for_insertion, Some('\''));
}

#[test]
fn fixup_after_open_paren_with_existing_close_paren_injects_just_quotes() {
    // `DB::table(|)` — the `)` is already there (editor auto-pair on `(`).
    // Only inject `''`, not `'')`.
    let src = "DB::table()";
    let cursor_between_parens = "DB::table(".len();
    let prep = fixup_for_completion(src, cursor_between_parens).expect("after-paren case");
    assert_eq!(prep.fixed_content, "DB::table('')");
    assert_eq!(prep.quote_for_insertion, Some('\''));
}

#[test]
fn fixup_after_open_paren_with_whitespace_still_fires() {
    // `DB::table(   |` — whitespace between `(` and cursor still counts.
    let src = "DB::table(   ";
    let prep = fixup_for_completion(src, src.len()).expect("after-paren w/ whitespace");
    assert_eq!(prep.fixed_content, "DB::table(   '')");
    assert_eq!(prep.quote_for_insertion, Some('\''));
}

#[test]
fn fixup_respects_escapes() {
    // `\'` inside a `'`-string doesn't toggle the state. Line still has an
    // unbalanced `(`, so the fixup injects both `'` and `)`.
    let src = "DB::table('it\\'s ";
    let prep = fixup_for_completion(src, src.len()).expect("still open after \\'");
    assert_eq!(prep.fixed_content, "DB::table('it\\'s ')");
    assert_eq!(prep.quote_for_insertion, None);
}

#[test]
fn fixup_stops_at_line_boundary() {
    // Quote opened on a previous line doesn't bleed into the current line.
    let src = "'unterminated\nDB::table(";
    // Current line is `DB::table(` — no open quote on this line; after-paren
    // case applies.
    let prep = fixup_for_completion(src, src.len()).expect("after-paren on new line");
    assert!(prep.fixed_content.ends_with("DB::table('')"));
    assert_eq!(prep.quote_for_insertion, Some('\''));
}

#[test]
fn fixup_handles_inner_double_inside_single() {
    let src = "echo 'a \"b\" c";
    let prep = fixup_for_completion(src, src.len()).expect("single still open");
    assert_eq!(prep.fixed_content, "echo 'a \"b\" c'");
    assert_eq!(prep.quote_for_insertion, None);
}

#[test]
fn fixup_handles_inner_single_inside_double() {
    let src = "echo \"a 'b' c";
    let prep = fixup_for_completion(src, src.len()).expect("double still open");
    assert_eq!(prep.fixed_content, "echo \"a 'b' c\"");
    assert_eq!(prep.quote_for_insertion, None);
}

#[test]
fn fixup_offset_beyond_eof_returns_none() {
    let src = "abc";
    assert!(fixup_for_completion(src, 100).is_none());
}

// ---- position_to_byte_offset ------------------------------------------

#[test]
fn position_first_line_first_char() {
    let content = "abc\ndef";
    assert_eq!(position_to_byte_offset(content, 0, 0), Some(0));
}

#[test]
fn position_first_line_mid() {
    let content = "abc\ndef";
    assert_eq!(position_to_byte_offset(content, 0, 2), Some(2));
}

#[test]
fn position_second_line() {
    let content = "abc\ndef";
    assert_eq!(position_to_byte_offset(content, 1, 0), Some(4));
    assert_eq!(position_to_byte_offset(content, 1, 2), Some(6));
}

#[test]
fn position_beyond_line_end_clamps() {
    // Asking for character 10 on a 3-char line should clamp to end of line.
    let content = "abc\ndef";
    assert_eq!(position_to_byte_offset(content, 0, 10), Some(3));
}

#[test]
fn position_beyond_eof_returns_none() {
    let content = "abc";
    assert!(position_to_byte_offset(content, 5, 0).is_none());
}

// ---- detect_chain_context_at: DB::table ----------------------------------

#[test]
fn detects_db_table_inside_where_string() {
    let ctx = detect("DB::table('users')->where('email|');")
        .expect("cursor inside where() string arg should produce a ChainContext");
    assert_eq!(ctx.mode, BuilderMode::BaseBuilder);
    assert_eq!(ctx.effective_table.as_deref(), Some("users"));
    assert!(ctx.effective_model.is_none());
    assert_eq!(ctx.expecting, ArgKind::Column);
    assert_eq!(ctx.quote, '\'');
}

#[test]
fn detects_db_table_inside_empty_where_string() {
    // The "backspace to empty" case Mike hit. Source is well-formed
    // (`where('')`), cursor is between the two `'`. Should still resolve
    // to a Column completion context so the handler returns all columns.
    let ctx = detect("DB::table('users')->where('|');")
        .expect("empty where() arg should still produce a ChainContext");
    assert_eq!(ctx.mode, BuilderMode::BaseBuilder);
    assert_eq!(ctx.effective_table.as_deref(), Some("users"));
    assert_eq!(ctx.expecting, ArgKind::Column);
}

#[test]
fn detects_db_table_with_double_quoted_arg() {
    let ctx = detect("DB::table('users')->where(\"em|\");").expect("ctx");
    assert_eq!(ctx.quote, '"');
}

#[test]
fn db_table_with_chained_where_still_base_mode() {
    // Cursor on the second where() — preceding where() doesn't flip the mode.
    let ctx = detect("DB::table('users')->where('a', 1)->where('b|');").expect("ctx");
    assert_eq!(ctx.mode, BuilderMode::BaseBuilder);
    assert_eq!(ctx.effective_table.as_deref(), Some("users"));
}

#[test]
fn db_table_inside_table_arg_offers_table_completion() {
    // Cursor on the `'users|'` arg of DB::table() — the `table` link's first
    // string arg gets ArgKind::Table so the completion handler returns the
    // list of database tables.
    let ctx = detect("DB::table('users|')->where('a', 1);")
        .expect("cursor inside DB::table() string arg should produce a ChainContext");
    assert_eq!(ctx.mode, BuilderMode::BaseBuilder);
    assert_eq!(ctx.expecting, ArgKind::Table);
    assert_eq!(ctx.quote, '\'');
}

#[test]
fn lowercase_db_facade_resolves_too() {
    // PHP class names are case-insensitive, so `db::table('|')` works just
    // like `DB::table('|')`. The receiver is DbTable, the link is Table.
    let ctx = detect("db::table('|');").expect("db (lowercase) should resolve to DB facade");
    assert_eq!(ctx.expecting, ArgKind::Table);
    match ctx.effective_table.as_deref() {
        Some("") => {} // empty table name — the user just typed `'`
        other => panic!("unexpected table: {:?}", other),
    }
}

#[test]
fn db_table_cursor_outside_string_returns_none() {
    // Cursor on the method name `wher|e` — not inside a string arg.
    let ctx = detect("DB::table('users')->wher|e('a', 1);");
    assert!(ctx.is_none());
}

#[test]
fn db_table_with_open_quote_and_partial_text_no_close_paren_resolves() {
    // The delete-from-end case end-to-end: source line is `DB::table('trua`
    // with cursor at the end. No close quote, no `)`. After fixup, the
    // chain should resolve to a Table completion with the partial text
    // `trua` as the effective_table prefix.
    let raw = "<?php\nDB::table('trua\n        return 0;\n";
    let cursor_byte = "<?php\nDB::table('trua".len();
    let prep = fixup_for_completion(raw, cursor_byte).expect("quote + paren fixup");
    let tree = crate::parser::parse_php(&prep.fixed_content).expect("parse");
    let chains: Vec<Arc<BuilderChain>> =
        crate::query_chain::extract_chains(&tree, &prep.fixed_content)
            .into_iter()
            .map(Arc::new)
            .collect();
    let ctx = detect_chain_context_at(&chains, cursor_byte)
        .expect("after fixup the chain should resolve to a Table completion context");
    assert_eq!(ctx.mode, BuilderMode::BaseBuilder);
    assert_eq!(ctx.expecting, ArgKind::Table);
    assert_eq!(ctx.effective_table.as_deref(), Some("trua"));
}

#[test]
fn db_table_with_auto_paired_quotes_no_close_paren_resolves_to_table_completion() {
    // The Sail-typing case end-to-end: user typed `DB::table('` and the
    // editor auto-paired the `'`, leaving source as `DB::table('')` with no
    // `)` yet. Without the paren fixup tree-sitter recovers by extending
    // the call expression, and detect_chain_context_at returns None — the
    // bug we're fixing. After fixup, we should land on BaseBuilder /
    // expecting=Table with an empty effective_table (user hasn't typed
    // anything yet — show all tables).
    let raw = "<?php\nDB::table('')";
    let cursor_byte = "<?php\nDB::table('".len();
    let prep = fixup_for_completion(raw, cursor_byte).expect("auto-pair + paren fixup");
    let tree = crate::parser::parse_php(&prep.fixed_content).expect("parse");
    let chains: Vec<Arc<BuilderChain>> =
        crate::query_chain::extract_chains(&tree, &prep.fixed_content)
            .into_iter()
            .map(Arc::new)
            .collect();
    let ctx = detect_chain_context_at(&chains, cursor_byte)
        .expect("after fixup injects `)`, the cursor should resolve to a Table completion context");
    assert_eq!(ctx.mode, BuilderMode::BaseBuilder);
    assert_eq!(ctx.expecting, ArgKind::Table);
    assert_eq!(ctx.effective_table.as_deref(), Some(""));
}

// ---- detect_chain_context_at: Eloquent (Phase 4+ — current expectation) -

#[test]
fn eloquent_receiver_returns_none_for_now() {
    // Eloquent model resolution is async — Phase 3's sync detector returns
    // None for Eloquent chains. Phase 4 will hook in the async resolver.
    let ctx = detect("User::where('em|');");
    assert!(
        ctx.is_none(),
        "Phase 3 expects Eloquent chains to return None"
    );
}

// ---- Chain terminators end completion -----------------------------------

#[test]
fn terminator_before_cursor_returns_none() {
    // count() is a ChainTerminator — anything after it isn't a builder
    // operation. We've decided not to complete past a terminator.
    let ctx = detect("DB::table('users')->count()->where('a|');");
    assert!(ctx.is_none());
}

// ---- Nested chains ------------------------------------------------------

#[test]
fn nested_chain_picks_inner_chain() {
    // Inside the closure, $q is its own chain. The cursor sits inside the
    // inner $q->where(). The receiver is InstanceVar (Eloquent), so Phase 3
    // returns None — but `find_chain_containing` should pick the *inner*
    // chain, not the outer one. We confirm that indirectly: with Eloquent
    // chains returning None, this returns None. (In Phase 9 with var-type
    // resolution this becomes a real completion.)
    let ctx = detect("DB::table('users')->where('exists', function ($q) { $q->where('a|'); });");
    // The outer chain is DB::table (Base) — but the cursor falls within the
    // inner chain ($q->where), so we pick that one and return None.
    // If find_chain_containing picked the outer chain instead, we'd see
    // BaseBuilder + 'users' here.
    assert!(
        ctx.is_none(),
        "expected None (inner $q chain wins); got {:?}",
        ctx
    );
}

// ---- find_chain_containing direct test ---------------------------------

#[test]
fn find_chain_containing_picks_innermost() {
    let (_src, chains) =
        chains_of("DB::table('users')->where('exists', function ($q) { $q->where('a', 1); });");
    // Find offset of the inner `$q->where('a', 1)` — pick a byte inside
    // the inner where's args.
    let wrapped = format!(
        "<?php\nDB::table('users')->where('exists', function ($q) {{ $q->where('a', 1); }});"
    );
    // Look for the second occurrence of 'a' inside the chains
    let inner_offset = wrapped.rfind("'a'").unwrap() + 1; // mid 'a'
    let chain = find_chain_containing(&chains, inner_offset).expect("chain at inner offset");
    match &chain.receiver {
        ChainReceiver::Eloquent(EloquentReceiver::InstanceVar { var, .. }) => {
            assert_eq!(var, "q", "innermost chain should be $q's");
        }
        other => panic!("expected $q's chain, got {other:?}"),
    }
}
