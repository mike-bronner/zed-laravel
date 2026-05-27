use super::*;
use crate::parser::parse_php;

fn parse(src: &str) -> tree_sitter::Tree {
    let wrapped = format!("<?php\n{src}");
    parse_php(&wrapped).expect("parse")
}

fn extract(src: &str) -> Vec<BuilderChain> {
    let wrapped = format!("<?php\n{src}");
    let tree = parse(src);
    super::extract_chains(&tree, &wrapped)
}

fn method_names(chain: &BuilderChain) -> Vec<&str> {
    chain.links.iter().map(|l| l.method.as_str()).collect()
}

// ---- Eloquent static receivers ------------------------------------------

#[test]
fn extracts_static_query_chain() {
    let chains = extract("User::query()->where('email', $email)->with('posts');");
    assert_eq!(chains.len(), 1);
    let c = &chains[0];
    assert_eq!(
        c.receiver,
        ChainReceiver::Eloquent(EloquentReceiver::StaticModel("User".to_string()))
    );
    assert_eq!(method_names(c), vec!["query", "where", "with"]);
}

#[test]
fn extracts_idiomatic_where_form_without_query() {
    // The common form: User::where(...)->with(...).
    let chains = extract("User::where('email', $e)->with('posts')->get();");
    assert_eq!(chains.len(), 1);
    let c = &chains[0];
    assert_eq!(
        c.receiver,
        ChainReceiver::Eloquent(EloquentReceiver::StaticModel("User".to_string()))
    );
    assert_eq!(method_names(c), vec!["where", "with", "get"]);
}

#[test]
fn extracts_idiomatic_find_form() {
    let chains = extract("Post::find($id);");
    assert_eq!(chains.len(), 1);
    assert_eq!(
        chains[0].receiver,
        ChainReceiver::Eloquent(EloquentReceiver::StaticModel("Post".to_string()))
    );
}

#[test]
fn fqcn_static_call_keeps_basename() {
    // \App\Models\User::query() — receiver resolution later does the real
    // class lookup; here we just want the leading backslash stripped so the
    // class name matches resolver expectations.
    let chains = extract("\\App\\Models\\User::query()->where('a', 1);");
    assert_eq!(chains.len(), 1);
    match &chains[0].receiver {
        ChainReceiver::Eloquent(EloquentReceiver::StaticModel(name)) => {
            assert_eq!(name, "App\\Models\\User");
        }
        other => panic!("unexpected receiver: {other:?}"),
    }
}

// ---- Eloquent instance receivers ----------------------------------------

#[test]
fn extracts_instance_chain_with_unresolved_type() {
    let chains = extract("$user->newQuery()->where('email', $e);");
    assert_eq!(chains.len(), 1);
    let c = &chains[0];
    match &c.receiver {
        ChainReceiver::Eloquent(EloquentReceiver::InstanceVar { var, php_type }) => {
            assert_eq!(var, "user");
            // Phase 9 fills this in; Phase 2 reports it as unresolved.
            assert!(php_type.is_none());
        }
        other => panic!("unexpected receiver: {other:?}"),
    }
    assert_eq!(method_names(c), vec!["newQuery", "where"]);
}

// ---- DB::table receivers ------------------------------------------------

#[test]
fn extracts_db_table_chain() {
    let chains = extract("DB::table('users')->where('email', 'a@b.c');");
    assert_eq!(chains.len(), 1);
    let c = &chains[0];
    match &c.receiver {
        ChainReceiver::DbTable { table, .. } => assert_eq!(table, "users"),
        other => panic!("unexpected receiver: {other:?}"),
    }
    // The first link is still `table` — DB::table is a real call with a
    // string arg; the receiver just captures the table name out of it.
    assert_eq!(method_names(c), vec!["table", "where"]);
}

#[test]
fn extracts_db_table_with_double_quotes() {
    let chains = extract("DB::table(\"users\")->where('id', 1);");
    assert_eq!(chains.len(), 1);
    match &chains[0].receiver {
        ChainReceiver::DbTable { table, .. } => assert_eq!(table, "users"),
        other => panic!("unexpected receiver: {other:?}"),
    }
}

// ---- Link classification ------------------------------------------------

#[test]
fn link_arg_and_effect_populated() {
    let chains = extract("User::query()->where('email', $e)->get();");
    let c = &chains[0];
    let links = &c.links;
    assert_eq!(links[0].method, "query");
    assert_eq!(links[0].arg, ArgKind::None);
    assert_eq!(links[0].effect, ChainEffect::None);
    assert_eq!(links[1].method, "where");
    assert_eq!(links[1].arg, ArgKind::Column);
    assert_eq!(links[1].effect, ChainEffect::None);
    assert_eq!(links[2].method, "get");
    assert_eq!(links[2].arg, ArgKind::None);
    assert_eq!(links[2].effect, ChainEffect::FlipToCollection);
}

#[test]
fn pluck_carries_column_arg_and_collection_effect() {
    let chains = extract("User::query()->pluck('email');");
    let c = &chains[0];
    let pluck = c.links.iter().find(|l| l.method == "pluck").unwrap();
    assert_eq!(pluck.arg, ArgKind::Column);
    assert_eq!(pluck.effect, ChainEffect::FlipToCollection);
}

#[test]
fn to_base_classified_as_mode_flip() {
    let chains = extract("User::where('a', 1)->toBase()->where('b', 2);");
    let c = &chains[0];
    let to_base = c.links.iter().find(|l| l.method == "toBase").unwrap();
    assert_eq!(to_base.effect, ChainEffect::FlipToBase);
}

// ---- String argument extraction ----------------------------------------

#[test]
fn string_arg_records_value_quote_and_span() {
    let chains = extract("User::where('email', $e);");
    let where_link = &chains[0].links[0];
    assert_eq!(where_link.args.len(), 2);
    match &where_link.args[0] {
        ChainArg::StringLit { value, quote, .. } => {
            assert_eq!(value, "email");
            assert_eq!(*quote, '\'');
        }
        other => panic!("expected StringLit, got {other:?}"),
    }
    // Second arg is a variable — Other.
    assert!(matches!(where_link.args[1], ChainArg::Other));
}

#[test]
fn double_quoted_string_arg() {
    let chains = extract("User::where(\"email\", 1);");
    let where_link = &chains[0].links[0];
    match &where_link.args[0] {
        ChainArg::StringLit { value, quote, .. } => {
            assert_eq!(value, "email");
            assert_eq!(*quote, '"');
        }
        other => panic!("expected StringLit, got {other:?}"),
    }
}

// ---- Closure arguments --------------------------------------------------

#[test]
fn closure_arg_records_parameter_name() {
    let chains =
        extract("User::whereHas('posts', function ($q) { $q->where('published', 1); })->get();");
    // Outer chain
    let c = chains
        .iter()
        .find(|c| c.links.iter().any(|l| l.method == "whereHas"))
        .expect("outer chain present");
    let where_has = c.links.iter().find(|l| l.method == "whereHas").unwrap();
    assert_eq!(where_has.arg, ArgKind::ClosureCarrier);
    assert_eq!(where_has.args.len(), 2);
    // First arg: 'posts'
    match &where_has.args[0] {
        ChainArg::StringLit { value, .. } => assert_eq!(value, "posts"),
        other => panic!("expected 'posts' string, got {other:?}"),
    }
    // Second arg: closure with $q
    match &where_has.args[1] {
        ChainArg::Closure { params, .. } => {
            assert_eq!(params.len(), 1);
            assert_eq!(params[0].name, "q");
            assert!(params[0].php_type.is_none());
        }
        other => panic!("expected Closure, got {other:?}"),
    }
}

#[test]
fn arrow_function_closure_arg() {
    let chains = extract("User::whereHas('posts', fn ($q) => $q->where('published', 1));");
    let outer = chains
        .iter()
        .find(|c| c.links.iter().any(|l| l.method == "whereHas"))
        .expect("outer chain present");
    let where_has = outer.links.iter().find(|l| l.method == "whereHas").unwrap();
    match &where_has.args[1] {
        ChainArg::Closure { params, .. } => {
            assert_eq!(params.len(), 1);
            assert_eq!(params[0].name, "q");
        }
        other => panic!("expected arrow Closure, got {other:?}"),
    }
}

// ---- Nested / sibling chains --------------------------------------------

#[test]
fn nested_chains_extract_independently() {
    // The outer chain rooted at User and the inner chain inside the closure
    // (rooted at $q) are BOTH chain roots — they're independent chains as
    // far as extraction is concerned. Closure-scope binding (Phase 8) is
    // what later links the inner chain's `$q` receiver back to `Post`.
    let chains = extract("User::whereHas('posts', function ($q) { $q->where('published', 1); });");
    // We expect at least 2 chains: outer User chain, inner $q chain.
    let outer = chains
        .iter()
        .find(|c| c.links.iter().any(|l| l.method == "whereHas"))
        .expect("outer User chain");
    assert_eq!(
        outer.receiver,
        ChainReceiver::Eloquent(EloquentReceiver::StaticModel("User".to_string()))
    );

    let inner = chains
        .iter()
        .find(|c| {
            matches!(
                &c.receiver,
                ChainReceiver::Eloquent(EloquentReceiver::InstanceVar { var, .. }) if var == "q"
            )
        })
        .expect("inner $q chain");
    assert_eq!(method_names(inner), vec!["where"]);
}

#[test]
fn multiple_top_level_chains_in_one_file() {
    let chains = extract(
        r#"
        User::where('a', 1)->get();
        Post::find(3);
        DB::table('events')->count();
        "#,
    );
    // 3 distinct chains, each with its own receiver.
    assert_eq!(chains.len(), 3);
    let receivers: Vec<&ChainReceiver> = chains.iter().map(|c| &c.receiver).collect();
    assert!(receivers.iter().any(|r| matches!(
        r,
        ChainReceiver::Eloquent(EloquentReceiver::StaticModel(s)) if s == "User"
    )));
    assert!(receivers.iter().any(|r| matches!(
        r,
        ChainReceiver::Eloquent(EloquentReceiver::StaticModel(s)) if s == "Post"
    )));
    assert!(receivers.iter().any(|r| matches!(
        r,
        ChainReceiver::DbTable { table, .. } if table == "events"
    )));
}

// ---- Negative cases -----------------------------------------------------

#[test]
fn non_call_code_produces_no_chains() {
    let chains = extract("$a = 1; $b = $a + 2; echo $b;");
    assert!(chains.is_empty());
}

// ---- shift_chain_byte_ranges -------------------------------------------

#[test]
fn shift_chain_byte_ranges_shifts_every_span() {
    // Build a chain manually with snippet-local byte ranges, shift it,
    // verify every span moved correctly. Receiver, links, args, and
    // closure body all participate.
    let mut chain = BuilderChain {
        receiver: ChainReceiver::DbTable {
            table: "users".to_string(),
            name_byte_range: (10, 17),
        },
        span_byte_range: (6, 50),
        links: vec![ChainLink {
            method: "where".to_string(),
            arg: ArgKind::Column,
            effect: ChainEffect::None,
            span_byte_range: (30, 45),
            args: vec![
                ChainArg::StringLit {
                    value: "email".to_string(),
                    quote: '\'',
                    span_byte_range: (37, 43),
                },
                ChainArg::Closure {
                    params: vec![ClosureParam {
                        name: "q".to_string(),
                        php_type: None,
                    }],
                    body_byte_range: (44, 48),
                },
            ],
        }],
    };
    // Region starts at outer byte 100. Wrapper prefix is 6 bytes (<?php ).
    // So snippet byte 6 (start of content) maps to outer byte 100.
    // Every span shifts by 100 - 6 = 94.
    super::shift_chain_byte_ranges(&mut chain, 100, 6);

    assert_eq!(chain.span_byte_range, (100, 144));
    match &chain.receiver {
        ChainReceiver::DbTable {
            name_byte_range, ..
        } => assert_eq!(*name_byte_range, (104, 111)),
        _ => panic!("receiver type changed"),
    }
    let link = &chain.links[0];
    assert_eq!(link.span_byte_range, (124, 139));
    match &link.args[0] {
        ChainArg::StringLit {
            span_byte_range, ..
        } => assert_eq!(*span_byte_range, (131, 137)),
        _ => panic!("arg type changed"),
    }
    match &link.args[1] {
        ChainArg::Closure {
            body_byte_range, ..
        } => assert_eq!(*body_byte_range, (138, 142)),
        _ => panic!("arg type changed"),
    }
}

// ---- Blade-embedded chain extraction (Phase 3.5) ----------------------

/// Replicate the in-LSP flow for a Blade source: extract regions, parse each
/// as wrapped PHP, extract chains, shift ranges back to outer coordinates.
/// Returns chains in outer-file byte-range space.
fn extract_blade_chains(source: &str) -> Vec<BuilderChain> {
    use crate::blade_embedded_php::{extract_php_regions, PHP_WRAPPER_PREFIX_LEN};
    use crate::parser::parse_php;
    let mut chains = Vec::new();
    for region in extract_php_regions(source) {
        let wrapped = format!("<?php {}", region.content);
        let Ok(tree) = parse_php(&wrapped) else {
            continue;
        };
        for mut chain in super::extract_chains(&tree, &wrapped) {
            super::shift_chain_byte_ranges(
                &mut chain,
                region.byte_offset,
                PHP_WRAPPER_PREFIX_LEN as usize,
            );
            chains.push(chain);
        }
    }
    chains
}

#[test]
fn blade_echo_chain_lands_at_outer_byte_offset() {
    // Source layout (byte positions in comments):
    //   0:  <div>{{ DB::table('users')->where('email', 1) }}</div>
    //              ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
    //              starts at byte 8 (after `<div>{{ `)
    let source = "<div>{{ DB::table('users')->where('email', 1) }}</div>";
    let chains = extract_blade_chains(source);
    assert_eq!(chains.len(), 1);
    let c = &chains[0];
    // Receiver is DbTable. The chain span should fall inside the source —
    // specifically, the chain starts at the `DB` token (byte 8) and ends
    // somewhere before the closing `)`.
    assert!(c.span_byte_range.0 >= 8, "{:?}", c.span_byte_range);
    assert!(
        c.span_byte_range.1 <= source.len(),
        "{:?}",
        c.span_byte_range
    );
    // The byte slice for the chain span should look like a chain.
    let slice = &source[c.span_byte_range.0..c.span_byte_range.1];
    assert!(slice.starts_with("DB::table"), "slice = {:?}", slice);
    assert!(slice.contains("where"), "slice = {:?}", slice);
    // The 'email' string-arg byte range should also be on the outer file.
    let where_link = c.links.iter().find(|l| l.method == "where").unwrap();
    if let ChainArg::StringLit {
        span_byte_range, ..
    } = &where_link.args[0]
    {
        let arg_slice = &source[span_byte_range.0..span_byte_range.1];
        assert_eq!(arg_slice, "'email'", "arg slice mismatch");
    } else {
        panic!("expected StringLit for first where arg");
    }
}

#[test]
fn blade_raw_echo_chain() {
    let source = "<p>{!! User::where('a', 1)->pluck('email') !!}</p>";
    let chains = extract_blade_chains(source);
    assert!(
        chains.iter().any(|c| matches!(
            &c.receiver,
            ChainReceiver::Eloquent(EloquentReceiver::StaticModel(s)) if s == "User"
        )),
        "User chain missing from {:?}",
        chains.iter().map(|c| &c.receiver).collect::<Vec<_>>()
    );
}

#[test]
fn blade_php_block_chain() {
    let source = r#"<div>
@php
    $users = DB::table('users')->where('active', 1)->get();
@endphp
</div>"#;
    let chains = extract_blade_chains(source);
    assert_eq!(chains.len(), 1);
    let c = &chains[0];
    match &c.receiver {
        ChainReceiver::DbTable { table, .. } => assert_eq!(table, "users"),
        other => panic!("expected DbTable, got {other:?}"),
    }
    let slice = &source[c.span_byte_range.0..c.span_byte_range.1];
    assert!(
        slice.starts_with("DB::table"),
        "outer-byte slice didn't land on chain start: {:?}",
        slice
    );
}

#[test]
fn blade_php_inline_short_form_chain() {
    let source = "<x-card>@php($users = DB::table('users')->where('id', 1)->get())</x-card>";
    let chains = extract_blade_chains(source);
    assert_eq!(chains.len(), 1);
    let c = &chains[0];
    match &c.receiver {
        ChainReceiver::DbTable { table, .. } => assert_eq!(table, "users"),
        other => panic!("expected DbTable, got {other:?}"),
    }
    let slice = &source[c.span_byte_range.0..c.span_byte_range.1];
    assert!(slice.starts_with("DB::table"), "slice = {:?}", slice);
}

#[test]
fn blade_native_php_tag_chain() {
    let source = "<div><?php $u = User::where('id', 1)->get(); ?></div>";
    let chains = extract_blade_chains(source);
    let user_chain = chains
        .iter()
        .find(|c| {
            matches!(
                &c.receiver,
                ChainReceiver::Eloquent(EloquentReceiver::StaticModel(s)) if s == "User"
            )
        })
        .unwrap_or_else(|| {
            panic!(
                "User chain missing from native <?php ?> region; got receivers {:?}",
                chains.iter().map(|c| &c.receiver).collect::<Vec<_>>()
            )
        });
    let slice = &source[user_chain.span_byte_range.0..user_chain.span_byte_range.1];
    assert!(slice.starts_with("User::where"), "slice = {:?}", slice);
}

#[test]
fn blade_multiline_chain_in_php_block() {
    // Multi-line chain inside @php ... @endphp — byte ranges must still land
    // correctly even though the chain spans multiple lines.
    let source = r#"@php
$rows = DB::table('users')
    ->where('a', 1)
    ->where('b', 2)
    ->get();
@endphp"#;
    let chains = extract_blade_chains(source);
    assert_eq!(chains.len(), 1);
    let c = &chains[0];
    // Span should cover the whole chain.
    let slice = &source[c.span_byte_range.0..c.span_byte_range.1];
    assert!(slice.starts_with("DB::table"), "slice start = {:?}", slice);
    assert!(slice.contains("'b'"), "slice = {:?}", slice);
}

#[test]
fn unknown_receiver_classified_as_unknown() {
    // `($qb)` — parenthesised expression as receiver. We don't try to
    // resolve these in Phase 2; the chain just gets ChainReceiver::Unknown.
    let chains = extract("($qb)->where('a', 1);");
    // The extractor still produces a chain (the chain root exists), but the
    // receiver is Unknown.
    assert_eq!(chains.len(), 1);
    assert_eq!(chains[0].receiver, ChainReceiver::Unknown);
}
