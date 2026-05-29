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
fn detects_empty_where_with_no_close_paren_end_to_end() {
    // The exact case Mike reported: user had `where('a'` (no close paren),
    // hit delete on `a`, ended up with `where(''` (cursor between the two
    // `'`, still no close paren). Without paren fixup in case-1a the chain
    // misparses; with it the empty string parses as a `where` arg and the
    // ctx resolves to Column completion.
    let raw = "<?php\nDB::table('transaction_type')->where(''";
    let cursor_byte = "<?php\nDB::table('transaction_type')->where('".len();
    let prep = fixup_for_completion(raw, cursor_byte).expect("auto-pair + paren fixup");
    let tree = crate::parser::parse_php(&prep.fixed_content).expect("parse");
    let chains: Vec<Arc<BuilderChain>> =
        crate::query_chain::extract_chains(&tree, &prep.fixed_content)
            .into_iter()
            .map(Arc::new)
            .collect();
    let ctx = detect_chain_context_at(&chains, cursor_byte)
        .expect("after fixup the chain should resolve to a Column completion context");
    assert_eq!(ctx.mode, BuilderMode::BaseBuilder);
    assert_eq!(ctx.expecting, ArgKind::Column);
    assert_eq!(ctx.effective_table.as_deref(), Some("transaction_type"));
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

// ---- detect_chain_context_at: Eloquent static receivers (Phase 4) -------

#[test]
fn eloquent_static_receiver_resolves_to_builder_mode_with_model_set() {
    // `User::where('em|')` — receiver is Eloquent::StaticModel. detect_in_chain
    // populates `effective_model` with the class name and leaves
    // `effective_table` as `None`; the handler is responsible for resolving
    // the model file → table name via async I/O.
    let ctx = detect("User::where('em|');")
        .expect("Eloquent static receivers should produce a ChainContext in Phase 4+");
    assert_eq!(ctx.mode, BuilderMode::EloquentBuilder);
    assert_eq!(ctx.expecting, ArgKind::Column);
    assert!(
        ctx.effective_table.is_none(),
        "table is resolved at the handler, not in detect_in_chain"
    );
    assert_eq!(
        ctx.effective_model.as_deref(),
        Some("User"),
        "the FQCN (or short name if no `use`) should be preserved for the handler"
    );
}

#[test]
fn eloquent_static_receiver_with_use_alias_carries_fqcn() {
    // `use App\Models\User;` in the file should make the receiver's class
    // name fully qualified, so the handler can resolve it directly without
    // re-running use-alias resolution.
    let src = "<?php\nuse App\\Models\\User;\nUser::where('em|');";
    let pos = src.find('|').expect("test fixture missing `|` marker");
    let cleaned = src.replacen('|', "", 1);
    let tree = crate::parser::parse_php(&cleaned).expect("parse");
    let chains: Vec<Arc<BuilderChain>> = crate::query_chain::extract_chains(&tree, &cleaned)
        .into_iter()
        .map(Arc::new)
        .collect();
    let ctx = detect_chain_context_at(&chains, pos).expect("ctx");
    assert_eq!(
        ctx.effective_model.as_deref(),
        Some("App\\Models\\User"),
        "use-alias should produce the FQCN, not the short name"
    );
}

#[test]
fn eloquent_static_with_first_arg_resolves_to_relation_completion() {
    // `User::with('po|')` — `with` is a RELATION_METHOD, so the cursor's
    // arg kind is Relation. Handler reads the model's relationships.
    let ctx = detect("User::with('po|');")
        .expect("Eloquent with() arg should produce a ChainContext expecting=Relation");
    assert_eq!(ctx.mode, BuilderMode::EloquentBuilder);
    assert_eq!(ctx.expecting, ArgKind::Relation);
    assert_eq!(ctx.effective_model.as_deref(), Some("User"));
}

#[test]
fn eloquent_static_where_has_first_arg_resolves_to_closure_carrier() {
    // `User::whereHas('po|', closure)` — `whereHas` is in CLOSURE_CARRIERS
    // which wins precedence over RELATION_METHODS in arg_kind(). For the
    // first string arg the meaning is identical (a relation name), so the
    // handler accepts both Relation and ClosureCarrier as relation-name
    // positions.
    let ctx = detect("User::whereHas('po|', function ($q) {});")
        .expect("whereHas() first arg should produce a ChainContext");
    assert_eq!(ctx.mode, BuilderMode::EloquentBuilder);
    assert_eq!(ctx.expecting, ArgKind::ClosureCarrier);
    assert_eq!(ctx.effective_model.as_deref(), Some("User"));
}

#[test]
fn eloquent_static_with_array_arg_resolves_to_relation_completion() {
    // Mike's reported case: `User::with([''])` with cursor between the
    // two `'`s inside the array. Without Phase 5.2 the array arg was
    // ChainArg::Other and string_arg_at found nothing; now we recurse
    // into Array elements and the StringLit inside is reachable.
    let ctx = detect("User::with(['|']);")
        .expect("cursor inside array-arg string should resolve to Relation");
    assert_eq!(ctx.mode, BuilderMode::EloquentBuilder);
    assert_eq!(ctx.expecting, ArgKind::Relation);
    assert_eq!(ctx.effective_model.as_deref(), Some("User"));
}

#[test]
fn eloquent_static_with_array_arg_multiple_strings() {
    // `User::with(['posts', 'co|mments'])` — cursor on the second string
    // in the array. Same resolution as the single-string case.
    let ctx = detect("User::with(['posts', 'co|mments']);").expect("ctx");
    assert_eq!(ctx.expecting, ArgKind::Relation);
}

#[test]
fn eloquent_static_select_array_arg_resolves_to_column() {
    // `User::select(['na|me', 'email'])` — column completion in array form.
    let ctx = detect("User::select(['na|me', 'email']);").expect("ctx");
    assert_eq!(ctx.expecting, ArgKind::Column);
}

#[test]
fn eloquent_static_load_first_arg_resolves_to_relation() {
    // `User::load(...)` doesn't actually exist as a static (load is on the
    // model instance / collection), but `loadCount` and similar are on
    // builder. Use `with` as the canonical static-position relation method
    // and `withCount` for the closure-carrier-on-Eloquent-builder shape.
    let ctx = detect("User::withCount('po|');").expect("ctx");
    assert_eq!(ctx.expecting, ArgKind::ClosureCarrier);
    assert_eq!(ctx.effective_model.as_deref(), Some("User"));
}

#[test]
fn eloquent_instance_receiver_still_returns_none_until_phase_9() {
    // `$user->newQuery()->where('|')` — InstanceVar receiver still needs
    // `@var` / typed-param scanning (Phase 9). For now, return None.
    let ctx = detect("$user->newQuery()->where('em|');");
    assert!(
        ctx.is_none(),
        "instance-var receivers still return None until Phase 9 lands var-type resolution"
    );
}

// ---- Closure-scope resolution (Phase 8) ---------------------------------

#[test]
fn where_has_closure_resolves_to_parent_model_and_hop() {
    // `User::whereHas('posts', fn ($q) => $q->where('publi|shed', 1))` —
    // the inner cursor should produce a ChainContext with effective_model
    // = "User" (the parent) and closure_relation_hop = Some("posts").
    // The handler resolves the hop async to flip effective_model to Post.
    let ctx = detect("User::whereHas('posts', fn ($q) => $q->where('publi|shed', 1));")
        .expect("inner cursor in whereHas closure should resolve");
    assert_eq!(ctx.mode, BuilderMode::EloquentBuilder);
    assert_eq!(ctx.expecting, ArgKind::Column);
    assert_eq!(ctx.effective_model.as_deref(), Some("User"));
    assert_eq!(ctx.closure_relation_hop.as_deref(), Some("posts"));
}

#[test]
fn with_keyed_array_closure_resolves_to_parent_model_and_hop() {
    // The shape Mike reported:
    // `OAuthClient::with(['tokens' => function ($q) { $q->where('expi|red', 1); }])`
    let src = "OAuthClient::with(['tokens' => function ($q) { $q->where('expi|red', 1); }]);";
    let ctx = detect(src).expect("inner cursor in with-keyed-array closure should resolve");
    assert_eq!(ctx.mode, BuilderMode::EloquentBuilder);
    assert_eq!(ctx.expecting, ArgKind::Column);
    assert_eq!(ctx.effective_model.as_deref(), Some("OAuthClient"));
    assert_eq!(ctx.closure_relation_hop.as_deref(), Some("tokens"));
}

#[test]
fn nested_where_closure_inherits_parent_model() {
    // `User::where(function ($q) { $q->where('em|', 1); })` — same-model
    // closure: $q is bound to a User builder. effective_model = "User",
    // closure_relation_hop = None (no hop, inherit directly).
    let ctx = detect("User::where(function ($q) { $q->where('em|', 1); });")
        .expect("inner cursor in same-model closure should resolve");
    assert_eq!(ctx.mode, BuilderMode::EloquentBuilder);
    assert_eq!(ctx.expecting, ArgKind::Column);
    assert_eq!(ctx.effective_model.as_deref(), Some("User"));
    assert!(
        ctx.closure_relation_hop.is_none(),
        "same-model bindings shouldn't set closure_relation_hop"
    );
}

#[test]
fn when_closure_inherits_parent_model() {
    let ctx = detect("User::when($cond, function ($q) { $q->where('na|me', 1); });").expect("ctx");
    assert_eq!(ctx.effective_model.as_deref(), Some("User"));
    assert!(ctx.closure_relation_hop.is_none());
}

#[test]
fn closure_scope_with_unrelated_var_returns_none() {
    // The closure receiver is `$other`, not the closure param `$q`. We
    // shouldn't bind — return None.
    let ctx =
        detect("User::whereHas('posts', function ($q) use ($other) { $other->where('a|', 1); });");
    assert!(
        ctx.is_none(),
        "unrelated-var receivers inside the closure shouldn't get the parent's model"
    );
}

// ---- Dotted relation paths (Phase 7) -------------------------------------

#[test]
fn dotted_relation_path_single_hop_sets_prefix() {
    // `User::with('posts.|')` — cursor right after the dot.
    // dotted_prefix = "posts" (the hop to walk before listing relations).
    let ctx = detect("User::with('posts.|');").expect("ctx");
    assert_eq!(ctx.expecting, ArgKind::Relation);
    assert_eq!(ctx.dotted_prefix.as_deref(), Some("posts"));
}

#[test]
fn dotted_relation_path_multi_hop_sets_full_prefix() {
    // `User::with('posts.author.|')` — two hops. The walker resolves
    // posts → Post, then author → Author. Items will be Author's
    // relations.
    let ctx = detect("User::with('posts.author.|');").expect("ctx");
    assert_eq!(ctx.dotted_prefix.as_deref(), Some("posts.author"));
}

#[test]
fn dotted_relation_path_cursor_mid_last_segment_still_uses_prior_dots() {
    // `User::with('posts.au|thor')` — cursor inside the last segment.
    // The prefix is everything before the last dot ("posts"); the
    // editor handles fuzzy-filtering by what the user typed of the
    // final segment.
    let ctx = detect("User::with('posts.au|thor');").expect("ctx");
    assert_eq!(ctx.dotted_prefix.as_deref(), Some("posts"));
}

#[test]
fn dotted_relation_path_no_dot_means_no_prefix() {
    // `User::with('posts|')` — no dot. We're listing User's relations
    // at the top level, no hops needed.
    let ctx = detect("User::with('posts|');").expect("ctx");
    assert!(ctx.dotted_prefix.is_none());
}

#[test]
fn dotted_path_only_applies_to_relation_args_not_columns() {
    // `User::where('a.b|', 1)` — column args don't have dotted-path
    // semantics. We should NOT set dotted_prefix here.
    let ctx = detect("User::where('a.b|', 1);").expect("ctx");
    assert_eq!(ctx.expecting, ArgKind::Column);
    assert!(
        ctx.dotted_prefix.is_none(),
        "dotted prefix should only fire for Relation/ClosureCarrier args"
    );
}

#[test]
fn dotted_path_in_where_has_closure_carrier_also_works() {
    // `User::whereHas('posts.author.|', closure)` — whereHas's first
    // arg is a relation name, supports dotted paths the same as with().
    let ctx = detect("User::whereHas('posts.author.|', function ($q) {});").expect("ctx");
    assert_eq!(ctx.expecting, ArgKind::ClosureCarrier);
    assert_eq!(ctx.dotted_prefix.as_deref(), Some("posts.author"));
}

// ---- Mode flips: toBase / Collection terminators (Phase 6) -------------

#[test]
fn to_base_flips_mode_to_base_builder() {
    // `User::where(...)->toBase()->where('|')` — after toBase, the chain
    // is operating on a Query Builder. Mode should be BaseBuilder; the
    // model is still known (handler resolves the table from it).
    let ctx = detect("User::where('a', 1)->toBase()->where('em|', 2);")
        .expect("cursor after ->toBase() should still resolve");
    assert_eq!(ctx.mode, BuilderMode::BaseBuilder);
    assert_eq!(ctx.expecting, ArgKind::Column);
    assert_eq!(
        ctx.effective_model.as_deref(),
        Some("User"),
        "model is preserved across toBase() — handler resolves table from it"
    );
}

#[test]
fn to_base_then_with_returns_none_no_relations_on_base() {
    // `User::where(...)->toBase()->with('|')` — Query Builder has no
    // relation methods, so `with` would fail at runtime. The walker
    // detects expecting=Relation in BaseBuilder mode and the handler
    // returns empty.
    let ctx = detect("User::where('a', 1)->toBase()->with('po|');").expect("ctx");
    assert_eq!(ctx.mode, BuilderMode::BaseBuilder);
    assert_eq!(ctx.expecting, ArgKind::Relation);
    // (The HANDLER returns empty for this combo; cursor just reports
    // the mode + expecting accurately.)
}

#[test]
fn get_terminator_flips_to_collection_mode() {
    // `User::where(...)->get()->where('|')` — get() is a CollectionTerminator,
    // so the chain is now operating on a Collection. Same model.
    let ctx = detect("User::where('a', 1)->get()->where('em|', 2);").expect("ctx");
    assert_eq!(ctx.mode, BuilderMode::EloquentCollection);
    assert_eq!(ctx.effective_model.as_deref(), Some("User"));
}

#[test]
fn all_static_starter_flips_to_collection_mode() {
    // `User::all()->where('|')` — `all` is a Collection terminator at
    // the static-call entry point. Mode should be EloquentCollection
    // (not EloquentBuilder) by the time the cursor's link runs.
    let ctx = detect("User::all()->where('na|me', 'jim');").expect("ctx");
    assert_eq!(ctx.mode, BuilderMode::EloquentCollection);
    assert_eq!(ctx.effective_model.as_deref(), Some("User"));
}

#[test]
fn collection_load_resolves_to_relation() {
    // Collection's `load('|')` takes relation names, same as Builder's
    // `with('|')`. expecting should be Relation, mode EloquentCollection.
    let ctx = detect("User::all()->load('po|sts');").expect("ctx");
    assert_eq!(ctx.mode, BuilderMode::EloquentCollection);
    assert_eq!(ctx.expecting, ArgKind::Relation);
}

#[test]
fn chain_terminator_count_kills_further_completion() {
    // `count` is a ChainTerminator — it returns an int, so any chained
    // method that follows can't be a builder operation. detect_in_chain
    // returns None for cursors past a terminator.
    let ctx = detect("User::where('a', 1)->count()->where('em|', 2);");
    assert!(
        ctx.is_none(),
        "chain terminator should prevent further completion; got {ctx:?}"
    );
}

#[test]
fn chain_terminator_first_kills_further_completion() {
    // `first` returns a Model|null, not a builder. Subsequent chained
    // calls aren't builder ops either; completion stops.
    let ctx = detect("User::where('a', 1)->first()->where('em|', 2);");
    assert!(ctx.is_none(), "first() terminates the chain");
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
    let wrapped =
        "<?php\nDB::table('users')->where('exists', function ($q) { $q->where('a', 1); });";
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

// ---- byte_offset_to_position ----------------------------------------------

#[test]
fn byte_offset_to_position_first_line() {
    let content = "abc\ndef";
    assert_eq!(
        byte_offset_to_position(content, 0),
        tower_lsp::lsp_types::Position {
            line: 0,
            character: 0
        }
    );
    assert_eq!(
        byte_offset_to_position(content, 2),
        tower_lsp::lsp_types::Position {
            line: 0,
            character: 2
        }
    );
}

#[test]
fn byte_offset_to_position_after_newline() {
    let content = "abc\ndef";
    // Byte 4 is 'd' on line 1, column 0.
    assert_eq!(
        byte_offset_to_position(content, 4),
        tower_lsp::lsp_types::Position {
            line: 1,
            character: 0
        }
    );
    // Byte 6 is 'f' → line 1, column 2.
    assert_eq!(
        byte_offset_to_position(content, 6),
        tower_lsp::lsp_types::Position {
            line: 1,
            character: 2
        }
    );
}

#[test]
fn byte_offset_to_position_clamps_past_end() {
    let content = "ab\ncd";
    assert_eq!(
        byte_offset_to_position(content, 999),
        tower_lsp::lsp_types::Position {
            line: 1,
            character: 2
        }
    );
}

#[test]
fn position_byte_round_trip_is_stable() {
    let content = "<?php\nUser::where('email');\n$x = 1;\n";
    for byte in [0usize, 6, 12, 20, 27] {
        let pos = byte_offset_to_position(content, byte);
        let back =
            position_to_byte_offset(content, pos.line, pos.character).expect("round-trip offset");
        assert_eq!(back, byte, "round-trip failed for byte {byte}");
    }
}
