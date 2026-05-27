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
fn fixup_returns_none_when_no_open_quote() {
    let src = "$x = 1;";
    assert!(fixup_for_completion(src, src.len()).is_none());
}

#[test]
fn fixup_balanced_quotes_returns_none() {
    let src = "DB::table('users')->where('";
    // First `'`/`'` balanced (`'users'`), then a second `'` opened — that's
    // unbalanced. But fixup_for_completion sees one open, no close, so it
    // SHOULD return Some. Pin that.
    let fixed = fixup_for_completion(src, src.len()).expect("unbalanced");
    assert_eq!(fixed, "DB::table('users')->where(''");
}

#[test]
fn fixup_injects_single_quote_for_unterminated_db_table() {
    let src = "DB::table('";
    let fixed = fixup_for_completion(src, src.len()).expect("unbalanced single quote");
    assert_eq!(fixed, "DB::table(''");
}

#[test]
fn fixup_injects_double_quote_for_unterminated_double_string() {
    let src = "DB::table(\"";
    let fixed = fixup_for_completion(src, src.len()).expect("unbalanced double quote");
    assert_eq!(fixed, "DB::table(\"\"");
}

#[test]
fn fixup_respects_escapes() {
    // `\'` inside a `'`-string doesn't toggle the state.
    let src = "DB::table('it\\'s ";
    let fixed = fixup_for_completion(src, src.len()).expect("still open after \\'");
    assert_eq!(fixed, "DB::table('it\\'s '");
}

#[test]
fn fixup_stops_at_line_boundary() {
    // Quote opened on a previous line doesn't bleed into the current line.
    // (We deliberately ignore those — multi-line strings in chain args are
    // rare, and treating them per-line is safer than chasing arbitrary
    // history.)
    let src = "'unterminated\nDB::table(";
    // Cursor at end of `DB::table(` — current line has no opened quote,
    // even though the previous line did.
    let fixed = fixup_for_completion(src, src.len());
    assert!(
        fixed.is_none(),
        "previous-line quote shouldn't trigger fixup, got: {:?}",
        fixed
    );
}

#[test]
fn fixup_handles_inner_double_inside_single() {
    // `'has "double" inside'` — inner `"` doesn't toggle when we're inside `'`.
    let src = "echo 'a \"b\" c";
    let fixed = fixup_for_completion(src, src.len()).expect("single still open");
    assert_eq!(fixed, "echo 'a \"b\" c'");
}

#[test]
fn fixup_handles_inner_single_inside_double() {
    let src = "echo \"a 'b' c";
    let fixed = fixup_for_completion(src, src.len()).expect("double still open");
    assert_eq!(fixed, "echo \"a 'b' c\"");
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
