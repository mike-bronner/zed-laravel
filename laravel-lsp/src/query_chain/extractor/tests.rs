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
