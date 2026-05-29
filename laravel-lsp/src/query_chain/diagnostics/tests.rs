//! Tests for query-chain diagnostics.
//!
//! Split in two: pure-helper unit tests (Levenshtein, identifier guard,
//! suggestion picking, arg selection) that need no I/O, and fixture-backed
//! end-to-end tests that parse real PHP, seed a schema, and assert which
//! literals get flagged — with heavy emphasis on the false-positive guards.

use super::{
    best_suggestion, chain_diagnostics, common_prefix_len, dynamic_where_finder, first_string_arg,
    is_simple_identifier, levenshtein, split_dynamic_segments,
};
use crate::database::{DatabaseSchema, DatabaseSchemaProvider};
use crate::parser::parse_php;
use crate::query_chain::{extract_chains, ArgKind, ChainArg, ChainEffect, ChainLink};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tempfile::TempDir;
use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString};

// ---- levenshtein ----------------------------------------------------------

#[test]
fn levenshtein_identical_is_zero() {
    assert_eq!(levenshtein("email", "email"), 0);
}

#[test]
fn levenshtein_canonical_typos() {
    // The issue's worked examples.
    assert_eq!(levenshtein("emial", "email"), 2); // transposition = 2 substitutions
    assert_eq!(levenshtein("postss", "posts"), 1); // one deletion
    assert_eq!(levenshtein("user", "users"), 1); // one insertion
    assert_eq!(levenshtein("authorr", "author"), 1);
}

#[test]
fn levenshtein_empty_operands() {
    assert_eq!(levenshtein("", "abc"), 3);
    assert_eq!(levenshtein("abc", ""), 3);
    assert_eq!(levenshtein("", ""), 0);
}

// ---- is_simple_identifier -------------------------------------------------

#[test]
fn simple_identifier_accepts_bare_names() {
    assert!(is_simple_identifier("email"));
    assert!(is_simple_identifier("created_at"));
    assert!(is_simple_identifier("_private"));
    assert!(is_simple_identifier("col9"));
}

#[test]
fn simple_identifier_rejects_qualified_and_expressions() {
    assert!(!is_simple_identifier("users.id")); // qualified column
    assert!(!is_simple_identifier("count(*)")); // aggregate expression
    assert!(!is_simple_identifier("name as n")); // alias (space)
    assert!(!is_simple_identifier("*")); // wildcard
    assert!(!is_simple_identifier("")); // empty
    assert!(!is_simple_identifier("9col")); // leading digit
}

// ---- best_suggestion ------------------------------------------------------

#[test]
fn suggestion_picks_closest_within_edit_distance_two() {
    let candidates = vec!["email".to_string(), "name".to_string(), "id".to_string()];
    assert_eq!(
        best_suggestion("emial", &candidates),
        Some("email".to_string())
    );
}

#[test]
fn suggestion_none_when_nothing_close() {
    let candidates = vec!["email".to_string(), "name".to_string()];
    assert_eq!(best_suggestion("xyzzyx", &candidates), None);
}

#[test]
fn suggestion_falls_back_to_prefix_match() {
    // "descr" is edit-distance 6 from "description" but shares a long prefix.
    let candidates = vec!["description".to_string()];
    assert_eq!(
        best_suggestion("descr", &candidates),
        Some("description".to_string())
    );
}

#[test]
fn suggestion_is_case_insensitive() {
    let candidates = vec!["Email".to_string()];
    assert_eq!(
        best_suggestion("emial", &candidates),
        Some("Email".to_string())
    );
}

// ---- common_prefix_len ----------------------------------------------------

#[test]
fn common_prefix_len_counts_shared_run() {
    assert_eq!(common_prefix_len("status", "stat"), 4);
    assert_eq!(common_prefix_len("abc", "xyz"), 0);
    assert_eq!(common_prefix_len("same", "same"), 4);
}

// ---- first_string_arg -----------------------------------------------------

fn lit(value: &str, start: usize, end: usize) -> ChainArg {
    ChainArg::StringLit {
        value: value.to_string(),
        quote: '\'',
        span_byte_range: (start, end),
    }
}

fn link(method: &str, arg: ArgKind, args: Vec<ChainArg>) -> ChainLink {
    ChainLink {
        method: method.to_string(),
        arg,
        effect: ChainEffect::None,
        span_byte_range: (0, 0),
        args,
    }
}

#[test]
fn first_string_arg_returns_leading_literal_only() {
    // `where('email', '=', 'active')` — only the column position is returned.
    let l = link(
        "where",
        ArgKind::Column,
        vec![
            lit("email", 10, 17),
            lit("=", 19, 22),
            lit("active", 24, 32),
        ],
    );
    let got = first_string_arg(&l).expect("a leading string arg");
    assert_eq!(got.0, "email");
    assert_eq!(got.1, (10, 17));
}

#[test]
fn first_string_arg_skips_array_and_nonstring_args() {
    // First arg is an array → no top-level string literal → None.
    let array_arg = ChainArg::Array {
        elements: vec![lit("posts", 1, 8)],
        span_byte_range: (0, 10),
    };
    let l = link("with", ArgKind::Relation, vec![array_arg]);
    assert!(first_string_arg(&l).is_none());

    // No args at all → None.
    let empty = link("get", ArgKind::None, vec![]);
    assert!(first_string_arg(&empty).is_none());
}

// ---- end-to-end fixtures --------------------------------------------------

/// Parse PHP and extract its query chains.
fn chains_of(source: &str) -> Vec<Arc<crate::query_chain::BuilderChain>> {
    let tree = parse_php(source).expect("parse php");
    extract_chains(&tree, source)
        .into_iter()
        .map(Arc::new)
        .collect()
}

/// Tempdir with one or more model files under `app/Models/`. Returns the
/// tempdir (hold it to keep the dir alive) and the project root.
fn project_with_models(models: &[(&str, &str)]) -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    let models_dir = dir.path().join("app").join("Models");
    std::fs::create_dir_all(&models_dir).expect("create models dir");
    for (name, body) in models {
        std::fs::write(models_dir.join(format!("{name}.php")), body).expect("write model");
    }
    let root = dir.path().to_path_buf();
    (dir, root)
}

/// Seed a `DatabaseSchemaProvider` directly from fixtures — no live DB.
async fn provider_with(
    root: PathBuf,
    tables: &[(&str, &[(&str, &str)])],
) -> DatabaseSchemaProvider {
    let mut columns = HashMap::new();
    let mut columns_with_types = HashMap::new();
    let mut table_names = Vec::new();
    for (table, cols) in tables {
        table_names.push(table.to_string());
        columns.insert(
            table.to_string(),
            cols.iter().map(|(n, _)| n.to_string()).collect(),
        );
        columns_with_types.insert(
            table.to_string(),
            cols.iter()
                .map(|(n, t)| (n.to_string(), t.to_string()))
                .collect(),
        );
    }
    let schema = DatabaseSchema {
        tables: table_names,
        columns,
        columns_with_types,
        cached_at: Instant::now(),
    };
    let provider = DatabaseSchemaProvider::new(root);
    provider.set_test_schema(schema).await;
    provider
}

const USER_MODEL: &str = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public function posts() { return $this->hasMany(Post::class); }
    public function comments() { return $this->hasMany(Comment::class); }
    public function scopeWhereActive($query) { return $query->where('status', 'active'); }
}
"#;

fn code_of(diag: &tower_lsp::lsp_types::Diagnostic) -> &str {
    match &diag.code {
        Some(NumberOrString::String(s)) => s.as_str(),
        _ => "",
    }
}

#[tokio::test]
async fn flags_unknown_column_with_suggestion() {
    let (_dir, root) = project_with_models(&[("User", USER_MODEL)]);
    let db = provider_with(
        root.clone(),
        &[("users", &[("id", "int"), ("email", "string")])],
    )
    .await;
    let source = "<?php\nuse App\\Models\\User;\nUser::where('emial', 1)->get();\n";
    let chains = chains_of(source);

    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;

    assert_eq!(diags.len(), 1, "exactly one unknown-column diagnostic");
    assert_eq!(code_of(&diags[0]), super::CODE_UNKNOWN_COLUMN);
    assert!(diags[0].message.contains("emial"));
    assert!(diags[0].message.contains("table \"users\""));
    assert!(
        diags[0].message.contains("Did you mean \"email\"?"),
        "got: {}",
        diags[0].message
    );
    assert_eq!(diags[0].severity, Some(DiagnosticSeverity::WARNING));
}

#[tokio::test]
async fn valid_column_produces_no_diagnostic() {
    let (_dir, root) = project_with_models(&[("User", USER_MODEL)]);
    let db = provider_with(
        root.clone(),
        &[("users", &[("id", "int"), ("email", "string")])],
    )
    .await;
    let source = "<?php\nuse App\\Models\\User;\nUser::where('email', 1)->get();\n";
    let chains = chains_of(source);

    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert!(
        diags.is_empty(),
        "valid column should not be flagged: {diags:?}"
    );
}

#[tokio::test]
async fn operator_and_value_args_are_not_flagged() {
    // `where('email', '=', 'emial')` — only the column arg is validated. The
    // operator `=` and the VALUE `emial` must never be treated as columns.
    let (_dir, root) = project_with_models(&[("User", USER_MODEL)]);
    let db = provider_with(
        root.clone(),
        &[("users", &[("id", "int"), ("email", "string")])],
    )
    .await;
    let source = "<?php\nuse App\\Models\\User;\nUser::where('email', '=', 'emial')->get();\n";
    let chains = chains_of(source);

    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert!(
        diags.is_empty(),
        "operator/value must not be flagged: {diags:?}"
    );
}

#[tokio::test]
async fn qualified_column_is_skipped() {
    // `where('users.id', 1)` is valid (qualified column for joins) — the
    // identifier guard skips it rather than flagging "users.id" as missing.
    let (_dir, root) = project_with_models(&[("User", USER_MODEL)]);
    let db = provider_with(root.clone(), &[("users", &[("id", "int")])]).await;
    let source = "<?php\nuse App\\Models\\User;\nUser::where('users.id', 1)->get();\n";
    let chains = chains_of(source);

    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert!(
        diags.is_empty(),
        "qualified column must be skipped: {diags:?}"
    );
}

#[tokio::test]
async fn select_flags_bare_typo() {
    // A bare typo in `select` IS diagnosed (AC names `select`). Alias/qualified
    // forms are handled separately by the identifier guard (below).
    let (_dir, root) = project_with_models(&[("User", USER_MODEL)]);
    let db = provider_with(
        root.clone(),
        &[("users", &[("id", "int"), ("email", "string")])],
    )
    .await;
    let source = "<?php\nuse App\\Models\\User;\nUser::select('emial')->get();\n";
    let chains = chains_of(source);

    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert_eq!(diags.len(), 1, "bare select typo should flag: {diags:?}");
    assert!(diags[0].message.contains("emial"));
}

#[tokio::test]
async fn select_alias_and_qualified_forms_are_skipped() {
    // `'votes as score'` (alias) and `'users.id'` (qualified) are not simple
    // identifiers → skipped, so un-denying `select` doesn't add false positives.
    let (_dir, root) = project_with_models(&[("User", USER_MODEL)]);
    let db = provider_with(root.clone(), &[("users", &[("id", "int")])]).await;
    let alias = "<?php\nuse App\\Models\\User;\nUser::select('votes as score')->get();\n";
    let qualified = "<?php\nuse App\\Models\\User;\nUser::select('users.id')->get();\n";
    for source in [alias, qualified] {
        let chains = chains_of(source);
        let diags =
            chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
        assert!(
            diags.is_empty(),
            "alias/qualified select must not flag: {source:?} → {diags:?}"
        );
    }
}

#[tokio::test]
async fn having_is_not_diagnosed() {
    // `having` filters on aggregate aliases — a bare identifier there is often
    // not a real column, so it stays deny-listed.
    let (_dir, root) = project_with_models(&[("User", USER_MODEL)]);
    let db = provider_with(root.clone(), &[("users", &[("id", "int")])]).await;
    let source = "<?php\nuse App\\Models\\User;\nUser::having('total', '>', 5)->get();\n";
    let chains = chains_of(source);

    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert!(diags.is_empty(), "having must not be diagnosed: {diags:?}");
}

#[tokio::test]
async fn no_diagnostic_when_schema_not_loaded() {
    // Provider has no `users` table → legal set is empty → stay quiet, even
    // though `emial` is "wrong". False positives are worse than nothing.
    let (_dir, root) = project_with_models(&[("User", USER_MODEL)]);
    let db = provider_with(root.clone(), &[]).await;
    let source = "<?php\nuse App\\Models\\User;\nUser::where('emial', 1)->get();\n";
    let chains = chains_of(source);

    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert!(diags.is_empty(), "cold schema must not flag: {diags:?}");
}

#[tokio::test]
async fn flags_unknown_relation() {
    let (_dir, root) = project_with_models(&[("User", USER_MODEL)]);
    let db = provider_with(root.clone(), &[("users", &[("id", "int")])]).await;
    let source = "<?php\nuse App\\Models\\User;\nUser::with('postss')->get();\n";
    let chains = chains_of(source);

    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert_eq!(diags.len(), 1, "one unknown-relation diagnostic: {diags:?}");
    assert_eq!(code_of(&diags[0]), super::CODE_UNKNOWN_RELATION);
    assert!(diags[0].message.contains("postss"));
    assert!(diags[0].message.contains("App\\Models\\User"));
    assert!(diags[0].message.contains("Did you mean \"posts\"?"));
}

#[tokio::test]
async fn valid_relation_produces_no_diagnostic() {
    let (_dir, root) = project_with_models(&[("User", USER_MODEL)]);
    let db = provider_with(root.clone(), &[("users", &[("id", "int")])]).await;
    let source = "<?php\nuse App\\Models\\User;\nUser::with('posts')->get();\n";
    let chains = chains_of(source);

    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert!(
        diags.is_empty(),
        "valid relation must not be flagged: {diags:?}"
    );
}

#[tokio::test]
async fn flags_unknown_table_in_db_table() {
    let (_dir, root) = project_with_models(&[("User", USER_MODEL)]);
    let db = provider_with(root.clone(), &[("users", &[("id", "int")])]).await;
    let source = "<?php\nDB::table('user')->get();\n";
    let chains = chains_of(source);

    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert_eq!(diags.len(), 1, "one unknown-table diagnostic: {diags:?}");
    assert_eq!(code_of(&diags[0]), super::CODE_UNKNOWN_TABLE);
    assert!(diags[0].message.contains("Table \"user\" does not exist"));
    assert!(diags[0].message.contains("Did you mean \"users\"?"));
}

#[tokio::test]
async fn valid_table_produces_no_diagnostic() {
    let (_dir, root) = project_with_models(&[("User", USER_MODEL)]);
    let db = provider_with(root.clone(), &[("users", &[("id", "int")])]).await;
    let source = "<?php\nDB::table('users')->get();\n";
    let chains = chains_of(source);

    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert!(
        diags.is_empty(),
        "valid table must not be flagged: {diags:?}"
    );
}

#[tokio::test]
async fn diagnostic_range_targets_the_column_text_not_the_quotes() {
    let (_dir, root) = project_with_models(&[("User", USER_MODEL)]);
    let db = provider_with(root.clone(), &[("users", &[("id", "int")])]).await;
    // Line 2 (0-based): `User::where('emial', 1)->get();`
    let source = "<?php\nUse App\\Models\\User;\nUser::where('emial', 1)->get();\n";
    let chains = chains_of(source);

    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert_eq!(diags.len(), 1);
    let range = diags[0].range;
    // Squiggle covers exactly `emial` (5 chars), inside the quotes.
    assert_eq!(range.start.line, 2);
    assert_eq!(range.end.line, 2);
    let line = source.lines().nth(2).unwrap();
    let start = range.start.character as usize;
    let end = range.end.character as usize;
    assert_eq!(&line[start..end], "emial");
}

#[tokio::test]
async fn severity_is_propagated() {
    let (_dir, root) = project_with_models(&[("User", USER_MODEL)]);
    let db = provider_with(root.clone(), &[("users", &[("id", "int")])]).await;
    let source = "<?php\nuse App\\Models\\User;\nUser::where('emial', 1)->get();\n";
    let chains = chains_of(source);

    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::ERROR).await;
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].severity, Some(DiagnosticSeverity::ERROR));
}

#[tokio::test]
async fn flags_bare_single_arg_where_statement() {
    // Repro of `User::where('emaaail');` — single string arg, no second arg,
    // no terminator, bare expression statement. Must still flag.
    let (_dir, root) = project_with_models(&[("User", USER_MODEL)]);
    let db = provider_with(
        root.clone(),
        &[("users", &[("id", "int"), ("email", "string")])],
    )
    .await;
    let source = "<?php\nuse App\\Models\\User;\nUser::where('emaaail');\n";
    let chains = chains_of(source);
    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert_eq!(
        diags.len(),
        1,
        "bare single-arg where should flag: {diags:?}"
    );
    assert!(diags[0].message.contains("emaaail"));
}

#[tokio::test]
async fn flags_bare_where_without_use_import() {
    // Same, but the file does NOT `use App\Models\User` — the receiver stays
    // the bare name "User". This exercises whether class resolution finds the
    // model from a bare classname (common when testing in scratch files).
    let (_dir, root) = project_with_models(&[("User", USER_MODEL)]);
    let db = provider_with(
        root.clone(),
        &[("users", &[("id", "int"), ("email", "string")])],
    )
    .await;
    let source = "<?php\nUser::where('emaaail');\n";
    let chains = chains_of(source);
    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert_eq!(
        diags.len(),
        1,
        "bare classname should still resolve: {diags:?}"
    );
}

// ---- dynamic where{Column} : pure helpers ---------------------------------

#[test]
fn dynamic_where_finder_recognizes_finders() {
    assert_eq!(dynamic_where_finder("whereEmail"), Some(("where", "Email")));
    assert_eq!(
        dynamic_where_finder("orWhereName"),
        Some(("orWhere", "Name"))
    );
    assert_eq!(
        dynamic_where_finder("whereFirstNameAndEmail"),
        Some(("where", "FirstNameAndEmail"))
    );
}

#[test]
fn dynamic_where_finder_rejects_real_methods_and_non_finders() {
    assert_eq!(dynamic_where_finder("where"), None); // no studly suffix
    assert_eq!(dynamic_where_finder("orWhere"), None);
    assert_eq!(dynamic_where_finder("whereNull"), None); // real method
    assert_eq!(dynamic_where_finder("whereHas"), None); // real method
    assert_eq!(dynamic_where_finder("whereBetween"), None); // real method
    assert_eq!(dynamic_where_finder("whereabouts"), None); // lowercase → not a finder
    assert_eq!(dynamic_where_finder("orderBy"), None); // not where-prefixed
}

#[test]
fn split_dynamic_segments_handles_and_or_and_traps() {
    assert_eq!(split_dynamic_segments("Email"), vec!["Email"]);
    assert_eq!(
        split_dynamic_segments("FirstNameAndEmail"),
        vec!["FirstName", "Email"]
    );
    assert_eq!(split_dynamic_segments("NameOrEmail"), vec!["Name", "Email"]);
    // `Brand` has lowercase "and" — must NOT split.
    assert_eq!(split_dynamic_segments("Brand"), vec!["Brand"]);
    // `Order` starts with "Or" but it's the segment start — must NOT split.
    assert_eq!(split_dynamic_segments("Order"), vec!["Order"]);
}

// ---- dynamic where{Column} : end-to-end -----------------------------------

#[tokio::test]
async fn flags_dynamic_where_unknown_column() {
    let (_dir, root) = project_with_models(&[("User", USER_MODEL)]);
    let db = provider_with(
        root.clone(),
        &[(
            "users",
            &[("id", "int"), ("email", "string"), ("name", "string")],
        )],
    )
    .await;
    let source = "<?php\nuse App\\Models\\User;\nUser::whereEmaaail('x');\n";
    let chains = chains_of(source);

    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert_eq!(diags.len(), 1, "dynamic where typo should flag: {diags:?}");
    assert_eq!(code_of(&diags[0]), super::CODE_UNKNOWN_COLUMN);
    assert!(
        diags[0].message.contains("emaaail"),
        "msg: {}",
        diags[0].message
    );
    assert!(
        diags[0].message.contains("whereEmail"),
        "should suggest corrected method name; msg: {}",
        diags[0].message
    );
    // Squiggle targets the studly column portion of the method name.
    let line = source.lines().nth(2).unwrap();
    let r = diags[0].range;
    let (s, e) = (r.start.character as usize, r.end.character as usize);
    assert_eq!(&line[s..e], "Emaaail");
}

#[tokio::test]
async fn valid_dynamic_where_no_diagnostic() {
    let (_dir, root) = project_with_models(&[("User", USER_MODEL)]);
    let db = provider_with(
        root.clone(),
        &[("users", &[("id", "int"), ("email", "string")])],
    )
    .await;
    let source = "<?php\nuse App\\Models\\User;\nUser::whereEmail('x');\n";
    let chains = chains_of(source);

    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert!(
        diags.is_empty(),
        "valid dynamic finder must not flag: {diags:?}"
    );
}

#[tokio::test]
async fn dynamic_where_matching_scope_is_not_flagged() {
    // `whereActive` would map to a column `active` (absent from the schema),
    // but the model defines `scopeWhereActive` → callable `whereActive`. It's a
    // scope call, not a dynamic column finder. Must stay quiet.
    let (_dir, root) = project_with_models(&[("User", USER_MODEL)]);
    let db = provider_with(
        root.clone(),
        &[("users", &[("id", "int"), ("email", "string")])], // no `active` column
    )
    .await;
    let source = "<?php\nuse App\\Models\\User;\nUser::whereActive();\n";
    let chains = chains_of(source);

    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert!(
        diags.is_empty(),
        "scope call must not be flagged: {diags:?}"
    );
}

#[tokio::test]
async fn dynamic_where_multi_column_flags_bad_segment() {
    let (_dir, root) = project_with_models(&[("User", USER_MODEL)]);
    let db = provider_with(
        root.clone(),
        &[(
            "users",
            &[("id", "int"), ("email", "string"), ("name", "string")],
        )],
    )
    .await;
    // `whereEmailAndNaame` → segments email (ok), naame (typo of name).
    let source = "<?php\nuse App\\Models\\User;\nUser::whereEmailAndNaame('a', 'b');\n";
    let chains = chains_of(source);

    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert_eq!(diags.len(), 1, "bad segment should flag: {diags:?}");
    assert!(diags[0].message.contains("naame"));
}

#[tokio::test]
async fn dynamic_where_on_base_builder_flags() {
    let (_dir, root) = project_with_models(&[("User", USER_MODEL)]);
    let db = provider_with(
        root.clone(),
        &[("users", &[("id", "int"), ("email", "string")])],
    )
    .await;
    let source = "<?php\nDB::table('users')->whereEmaaail('x');\n";
    let chains = chains_of(source);

    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert_eq!(
        diags.len(),
        1,
        "dynamic where on DB::table should flag: {diags:?}"
    );
    assert!(diags[0].message.contains("emaaail"));
}

#[tokio::test]
async fn dynamic_where_on_collection_is_not_flagged() {
    // `User::all()` flips to a hydrated Collection — which has no dynamic
    // `where{Column}`. Don't flag `whereEmaaail` there (it'd be a plain
    // undefined-method, not a column probe).
    let (_dir, root) = project_with_models(&[("User", USER_MODEL)]);
    let db = provider_with(root.clone(), &[("users", &[("id", "int")])]).await;
    let source = "<?php\nuse App\\Models\\User;\nUser::all()->whereEmaaail('x');\n";
    let chains = chains_of(source);

    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert!(
        diags.is_empty(),
        "collection dynamic where must not flag: {diags:?}"
    );
}

// ---- incomplete / mid-typing chains ---------------------------------------
// The linter must not require a "finished" query (a terminator like ->get()).
// Each method call in the chain should be linted on its own.

async fn user_db_root() -> (TempDir, PathBuf, DatabaseSchemaProvider) {
    let (dir, root) = project_with_models(&[("User", USER_MODEL)]);
    let db = provider_with(
        root.clone(),
        &[("users", &[("id", "int"), ("email", "string")])],
    )
    .await;
    (dir, root, db)
}

#[tokio::test]
async fn incomplete_chain_no_semicolon_still_flags() {
    let (_d, root, db) = user_db_root().await;
    let source = "<?php\nUser::where('emaaail')\n";
    let chains = chains_of(source);
    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert_eq!(diags.len(), 1, "no-semicolon should still flag: {diags:?}");
}

#[tokio::test]
async fn incomplete_chain_trailing_arrow_still_flags() {
    let (_d, root, db) = user_db_root().await;
    let source = "<?php\nUser::where('emaaail')->\n";
    let chains = chains_of(source);
    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert_eq!(
        diags.len(),
        1,
        "trailing-arrow should still flag: {diags:?}"
    );
}

#[tokio::test]
async fn incomplete_chain_partial_next_method_still_flags() {
    // `->wher` is a half-typed next call — the completed `where('emaaail')`
    // before it must still be linted.
    let (_d, root, db) = user_db_root().await;
    let source = "<?php\nUser::where('emaaail')->wher\n";
    let chains = chains_of(source);
    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert_eq!(
        diags.len(),
        1,
        "partial next method should still flag the where: {diags:?}"
    );
}

#[tokio::test]
async fn unterminated_chain_assigned_still_flags() {
    // No terminator (`->get()`), just assigned — must lint the `where`.
    let (_d, root, db) = user_db_root().await;
    let source = "<?php\n$q = User::where('emaaail');\n";
    let chains = chains_of(source);
    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert_eq!(
        diags.len(),
        1,
        "assigned chain should still flag: {diags:?}"
    );
}

#[tokio::test]
async fn dynamic_where_range_is_correct_deep_in_file() {
    // Mirror a real controller file: the call is indented, several lines in.
    // The squiggle must cover exactly the studly column portion of the method.
    let (_d, root, db) = user_db_root().await;
    let source = "<?php\n\nnamespace App\\Http\\Controllers;\n\nuse App\\Models\\User;\n\nclass C {\n    public function i() {\n        User::whereEmaaaail('1');\n    }\n}\n";
    let chains = chains_of(source);
    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert_eq!(diags.len(), 1, "should flag once: {diags:?}");
    let r = diags[0].range;
    let line = source.lines().nth(r.start.line as usize).unwrap();
    let (s, e) = (r.start.character as usize, r.end.character as usize);
    assert_eq!(
        &line[s..e],
        "Emaaaail",
        "squiggle must cover the studly column"
    );
}

#[tokio::test]
async fn lints_each_method_call_independently() {
    // Two bad columns in one unterminated chain → two diagnostics. Proves we
    // lint per method call, not once per "finished" query.
    let (_d, root, db) = user_db_root().await;
    let source = "<?php\nUser::where('emaaail')->orderBy('naaame')\n";
    let chains = chains_of(source);
    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert_eq!(diags.len(), 2, "each bad call should flag: {diags:?}");
}

// ---- data payload contract (consumed by code_actions) ---------------------

#[tokio::test]
async fn column_diagnostic_data_carries_replacement_and_table() {
    let (_dir, root) = project_with_models(&[("User", USER_MODEL)]);
    let db = provider_with(
        root.clone(),
        &[("users", &[("id", "int"), ("email", "string")])],
    )
    .await;
    let source = "<?php\nuse App\\Models\\User;\nUser::where('emaaail');\n";
    let chains = chains_of(source);
    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    let data = diags[0].data.as_ref().expect("data payload");
    assert_eq!(data["kind"], "column");
    assert_eq!(data["name"], "emaaail");
    assert_eq!(data["replacement"], "email");
    assert_eq!(data["table"], "users");
}

#[tokio::test]
async fn dynamic_where_data_carries_studly_replacement_and_method_label() {
    let (_dir, root) = project_with_models(&[("User", USER_MODEL)]);
    let db = provider_with(
        root.clone(),
        &[("users", &[("id", "int"), ("email", "string")])],
    )
    .await;
    let source = "<?php\nuse App\\Models\\User;\nUser::whereEmaaail('x');\n";
    let chains = chains_of(source);
    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    let data = diags[0].data.as_ref().expect("data payload");
    assert_eq!(data["replacement"], "Email"); // studly, inserted into the method
    assert_eq!(data["replacementLabel"], "whereEmail"); // shown in the action title
    assert_eq!(data["table"], "users");
}

// ---- array-form args ------------------------------------------------------

#[tokio::test]
async fn array_relation_flags_unknown_element() {
    let (_dir, root) = project_with_models(&[("User", USER_MODEL)]);
    let db = provider_with(root.clone(), &[("users", &[("id", "int")])]).await;
    // posts + comments exist; postss is a typo.
    let source =
        "<?php\nuse App\\Models\\User;\nUser::with(['posts', 'postss', 'comments'])->get();\n";
    let chains = chains_of(source);

    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert_eq!(diags.len(), 1, "only the typo should flag: {diags:?}");
    assert!(diags[0].message.contains("postss"));
    assert_eq!(code_of(&diags[0]), super::CODE_UNKNOWN_RELATION);
}

#[tokio::test]
async fn keyed_relation_array_validates_the_key() {
    let (_dir, root) = project_with_models(&[("User", USER_MODEL)]);
    let db = provider_with(root.clone(), &[("users", &[("id", "int")])]).await;
    // `with(['rel' => closure])` — the key is the relation name.
    let source = "<?php\nuse App\\Models\\User;\nUser::with(['postss' => function ($q) { return $q; }])->get();\n";
    let chains = chains_of(source);

    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert_eq!(diags.len(), 1, "keyed relation key should flag: {diags:?}");
    assert!(diags[0].message.contains("postss"));
}

#[tokio::test]
async fn array_select_flags_unknown_column() {
    let (_dir, root) = project_with_models(&[("User", USER_MODEL)]);
    let db = provider_with(
        root.clone(),
        &[("users", &[("id", "int"), ("email", "string")])],
    )
    .await;
    let source = "<?php\nuse App\\Models\\User;\nUser::select(['id', 'emial', 'email'])->get();\n";
    let chains = chains_of(source);

    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert_eq!(
        diags.len(),
        1,
        "only the typo column should flag: {diags:?}"
    );
    assert!(diags[0].message.contains("emial"));
}

#[tokio::test]
async fn array_select_skips_aliased_and_qualified_elements() {
    let (_dir, root) = project_with_models(&[("User", USER_MODEL)]);
    let db = provider_with(root.clone(), &[("users", &[("id", "int")])]).await;
    let source =
        "<?php\nuse App\\Models\\User;\nUser::select(['id', 'name as n', 'users.id'])->get();\n";
    let chains = chains_of(source);

    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert!(
        diags.is_empty(),
        "aliased/qualified elements must be skipped: {diags:?}"
    );
}

#[tokio::test]
async fn where_array_values_are_not_flagged() {
    // `where(['email' => 'somevalue'])` — the array holds a `col => value`
    // pair. We don't descend `where`-family arrays, so neither the key nor the
    // value is validated (the value side would otherwise false-positive).
    let (_dir, root) = project_with_models(&[("User", USER_MODEL)]);
    let db = provider_with(
        root.clone(),
        &[("users", &[("id", "int"), ("email", "string")])],
    )
    .await;
    let source = "<?php\nuse App\\Models\\User;\nUser::where(['email' => 'somevalue'])->get();\n";
    let chains = chains_of(source);

    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert!(
        diags.is_empty(),
        "where-array values must not be flagged: {diags:?}"
    );
}

// ---- joined-table columns (issue #24) ---------------------------------

#[tokio::test]
async fn bare_column_from_joined_table_is_not_flagged() {
    // `status` lives only on `orders`, but the join makes it a legal bare
    // column — diagnostics must not flag it as missing on `users`.
    let (_dir, root) = project_with_models(&[]);
    let db = provider_with(
        root.clone(),
        &[
            ("users", &[("id", "int"), ("name", "string")]),
            ("orders", &[("id", "int"), ("status", "string")]),
        ],
    )
    .await;
    let source = "<?php\nDB::table('users')->join('orders', 'orders.user_id', '=', 'users.id')->where('status', 'active')->get();\n";
    let chains = chains_of(source);

    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert!(
        diags.is_empty(),
        "bare joined-table column must not be flagged: {diags:?}"
    );
}

#[tokio::test]
async fn typo_still_flagged_with_joins_present() {
    // A genuine typo absent from EVERY accessible table is still caught.
    let (_dir, root) = project_with_models(&[]);
    let db = provider_with(
        root.clone(),
        &[
            ("users", &[("id", "int"), ("name", "string")]),
            ("orders", &[("id", "int"), ("status", "string")]),
        ],
    )
    .await;
    let source = "<?php\nDB::table('users')->join('orders', 'a', '=', 'b')->where('stattus', 'active')->get();\n";
    let chains = chains_of(source);

    let diags = chain_diagnostics(&chains, &db, &root, source, DiagnosticSeverity::WARNING).await;
    assert_eq!(
        diags.len(),
        1,
        "real typo should still be flagged: {diags:?}"
    );
    assert_eq!(code_of(&diags[0]), super::CODE_UNKNOWN_COLUMN);
}
