use super::*;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

/// Build a fake vendor layout at `root` with the given file contents.
/// Returns the temp dir so the caller keeps the files alive for the test.
fn fake_vendor(
    root: &TempDir,
    eloquent_builder: Option<&str>,
    query_builder: Option<&str>,
) {
    if let Some(content) = eloquent_builder {
        let path = root.path().join(ELOQUENT_BUILDER_REL_PATH);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
    if let Some(content) = query_builder {
        let path = root.path().join(QUERY_BUILDER_REL_PATH);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
}

const MINIMAL_BUILDER: &str = r#"<?php

namespace Illuminate\Database\Eloquent;

/**
 * @mixin \Illuminate\Database\Query\Builder
 */
class Builder
{
    /**
     * Add a basic where clause to the query.
     *
     * @param  string  $column
     * @return $this
     */
    public function where($column, $operator = null, $value = null, $boolean = 'and')
    {
        return $this;
    }

    /**
     * Find a model by its primary key.
     */
    public function find($id, $columns = ['*'])
    {
        return null;
    }

    protected function shouldNotSurface(): void
    {
    }

    private function alsoShouldNotSurface(): void
    {
    }

    public function __callStatic($method, $args)
    {
    }

    /**
     * @internal
     * This method is internal.
     */
    public function internalMethod(): void
    {
    }
}
"#;

const MINIMAL_QUERY_BUILDER: &str = r#"<?php

namespace Illuminate\Database\Query;

class Builder
{
    /**
     * Add a basic where clause to the query.
     */
    public function where($column, $operator = null, $value = null, $boolean = 'and')
    {
    }

    /**
     * Set the columns to be selected.
     */
    public function select($columns = ['*'])
    {
    }
}
"#;

// ---- parse_builder_methods ---------------------------------------------

#[test]
fn returns_none_when_no_vendor_files_exist() {
    let dir = TempDir::new().unwrap();
    assert!(parse_builder_methods(dir.path()).is_none());
}

#[test]
fn parses_eloquent_builder_methods() {
    let dir = TempDir::new().unwrap();
    fake_vendor(&dir, Some(MINIMAL_BUILDER), None);

    let index = parse_builder_methods(dir.path()).expect("should parse");
    let names: Vec<&str> = index
        .eloquent_builder
        .iter()
        .map(|m| m.name.as_str())
        .collect();

    assert!(names.contains(&"where"), "where should surface: {names:?}");
    assert!(names.contains(&"find"), "find should surface: {names:?}");
    assert!(
        !names.contains(&"shouldNotSurface"),
        "protected methods must not surface: {names:?}"
    );
    assert!(
        !names.contains(&"alsoShouldNotSurface"),
        "private methods must not surface: {names:?}"
    );
    assert!(
        !names.contains(&"__callStatic"),
        "magic methods must not surface: {names:?}"
    );
    assert!(
        !names.contains(&"internalMethod"),
        "@internal methods must not surface: {names:?}"
    );
}

#[test]
fn extracts_return_type_from_phpdoc_return_tag() {
    let dir = TempDir::new().unwrap();
    fake_vendor(&dir, Some(MINIMAL_BUILDER), None);
    let index = parse_builder_methods(dir.path()).expect("should parse");
    let where_method = index
        .eloquent_builder
        .iter()
        .find(|m| m.name == "where")
        .expect("where method");
    assert_eq!(
        where_method.return_type.as_deref(),
        Some("Builder<static>"),
        "PHPDoc `@return $this` should resolve to the entry class with <static>"
    );
}

#[test]
fn resolves_self_token_to_builder_static() {
    // `@return self` should be rewritten the same way `$this` is.
    let dir = TempDir::new().unwrap();
    fake_vendor(
        &dir,
        Some(
            r#"<?php
namespace Illuminate\Database\Eloquent;
class Builder {
    /**
     * @return self
     */
    public function clone(): static {}
}"#,
        ),
        None,
    );
    let index = parse_builder_methods(dir.path()).expect("should parse");
    let clone_method = index
        .eloquent_builder
        .iter()
        .find(|m| m.name == "clone")
        .expect("clone method");
    assert_eq!(clone_method.return_type.as_deref(), Some("Builder<static>"));
}

#[test]
fn resolves_self_token_inside_union_type() {
    // `@return $this|null` should keep the union but resolve the
    // self-token half.
    let dir = TempDir::new().unwrap();
    fake_vendor(
        &dir,
        Some(
            r#"<?php
namespace Illuminate\Database\Eloquent;
class Builder {
    /**
     * @return $this|null
     */
    public function maybe() {}
}"#,
        ),
        None,
    );
    let index = parse_builder_methods(dir.path()).expect("should parse");
    let m = index
        .eloquent_builder
        .iter()
        .find(|m| m.name == "maybe")
        .expect("maybe method");
    assert_eq!(m.return_type.as_deref(), Some("Builder<static>|null"));
}

#[test]
fn passes_through_concrete_return_types_unchanged() {
    // Types that don't reference `$this`/`self`/`static` should be
    // emitted verbatim.
    let dir = TempDir::new().unwrap();
    fake_vendor(
        &dir,
        Some(
            r#"<?php
namespace Illuminate\Database\Eloquent;
class Builder {
    /**
     * @return \Illuminate\Support\Collection<int, mixed>
     */
    public function get() {}
}"#,
        ),
        None,
    );
    let index = parse_builder_methods(dir.path()).expect("should parse");
    let m = index
        .eloquent_builder
        .iter()
        .find(|m| m.name == "get")
        .expect("get method");
    assert_eq!(
        m.return_type.as_deref(),
        Some("\\Illuminate\\Support\\Collection<int,")
    );
    // ^ Trails after the first whitespace token, which is a known limitation
    // documented elsewhere — types with literal spaces get truncated. The
    // key invariant here is that nothing was rewritten to `Builder<static>`.
}

#[test]
fn extracts_return_type_from_php_declaration_when_no_phpdoc() {
    let dir = TempDir::new().unwrap();
    fake_vendor(
        &dir,
        Some(
            r#"<?php
namespace Illuminate\Database\Eloquent;
class Builder {
    public function toBase(): \Illuminate\Database\Query\Builder
    {
    }
}"#,
        ),
        None,
    );
    let index = parse_builder_methods(dir.path()).expect("should parse");
    let to_base = index
        .eloquent_builder
        .iter()
        .find(|m| m.name == "toBase")
        .expect("toBase method");
    assert_eq!(
        to_base.return_type.as_deref(),
        Some("\\Illuminate\\Database\\Query\\Builder"),
        "PHP return-type declaration should populate return_type when PHPDoc is absent"
    );
}

#[test]
fn returns_none_when_no_return_type_anywhere() {
    let dir = TempDir::new().unwrap();
    fake_vendor(
        &dir,
        Some(
            r#"<?php
namespace Illuminate\Database\Eloquent;
class Builder {
    public function untyped($x) {}
}"#,
        ),
        None,
    );
    let index = parse_builder_methods(dir.path()).expect("should parse");
    let untyped = index
        .eloquent_builder
        .iter()
        .find(|m| m.name == "untyped")
        .expect("untyped method");
    assert_eq!(untyped.return_type, None);
}

#[test]
fn extracts_phpdoc_summary_first_line() {
    let dir = TempDir::new().unwrap();
    fake_vendor(&dir, Some(MINIMAL_BUILDER), None);

    let index = parse_builder_methods(dir.path()).expect("should parse");
    let where_method = index
        .eloquent_builder
        .iter()
        .find(|m| m.name == "where")
        .expect("where method");

    assert_eq!(
        where_method.summary.as_deref(),
        Some("Add a basic where clause to the query.")
    );
}

#[test]
fn extracts_doc_body_with_markers_stripped() {
    let dir = TempDir::new().unwrap();
    fake_vendor(&dir, Some(MINIMAL_BUILDER), None);

    let index = parse_builder_methods(dir.path()).expect("should parse");
    let where_method = index
        .eloquent_builder
        .iter()
        .find(|m| m.name == "where")
        .expect("where method");

    let doc = where_method.doc_body.as_deref().expect("doc body");
    assert!(
        doc.contains("Add a basic where clause to the query."),
        "doc body should contain the summary: {doc}"
    );
    assert!(
        doc.contains("@param  string  $column"),
        "doc body should preserve @param lines: {doc}"
    );
    assert!(
        !doc.contains("/**") && !doc.contains("*/"),
        "doc body should have outer markers stripped: {doc}"
    );
}

#[test]
fn method_without_docblock_has_no_summary() {
    let dir = TempDir::new().unwrap();
    fake_vendor(
        &dir,
        Some(
            r#"<?php
namespace Illuminate\Database\Eloquent;
class Builder {
    public function bare() {}
}"#,
        ),
        None,
    );

    let index = parse_builder_methods(dir.path()).expect("should parse");
    let bare = index
        .eloquent_builder
        .iter()
        .find(|m| m.name == "bare")
        .expect("bare method");
    assert_eq!(bare.summary, None);
    assert_eq!(bare.doc_body, None);
}

#[test]
fn parses_query_builder_separately() {
    let dir = TempDir::new().unwrap();
    fake_vendor(&dir, None, Some(MINIMAL_QUERY_BUILDER));

    let index = parse_builder_methods(dir.path()).expect("should parse");
    let q_names: Vec<&str> = index.query_builder.iter().map(|m| m.name.as_str()).collect();
    assert!(q_names.contains(&"where"));
    assert!(q_names.contains(&"select"));
    assert!(
        index.eloquent_builder.is_empty(),
        "Eloquent shouldn't have parsed when only Query exists"
    );
}

#[test]
fn missing_one_file_still_returns_index() {
    // If Eloquent\Builder.php is missing but Query\Builder.php exists, we
    // degrade gracefully — partial surface is better than nothing.
    let dir = TempDir::new().unwrap();
    fake_vendor(&dir, None, Some(MINIMAL_QUERY_BUILDER));

    let index = parse_builder_methods(dir.path()).expect("should parse");
    assert!(index.eloquent_builder.is_empty());
    assert!(!index.query_builder.is_empty());
}

// ---- merged_surface ----------------------------------------------------

#[test]
fn merged_surface_eloquent_wins_collisions() {
    let dir = TempDir::new().unwrap();
    fake_vendor(&dir, Some(MINIMAL_BUILDER), Some(MINIMAL_QUERY_BUILDER));

    let index = parse_builder_methods(dir.path()).expect("should parse");
    let merged = index.merged_surface();

    // `where` exists in both files — make sure we get exactly one entry.
    let where_count = merged.iter().filter(|m| m.name == "where").count();
    assert_eq!(
        where_count, 1,
        "duplicate where in merged surface: {:?}",
        merged.iter().map(|m| &m.name).collect::<Vec<_>>()
    );

    // `select` only exists on Query — should come through.
    assert!(
        merged.iter().any(|m| m.name == "select"),
        "select (Query-only) should appear in merged surface"
    );

    // `find` only exists on Eloquent — should come through.
    assert!(
        merged.iter().any(|m| m.name == "find"),
        "find (Eloquent-only) should appear in merged surface"
    );
}

#[test]
fn merged_surface_is_sorted_alphabetically() {
    let dir = TempDir::new().unwrap();
    fake_vendor(&dir, Some(MINIMAL_BUILDER), Some(MINIMAL_QUERY_BUILDER));

    let index = parse_builder_methods(dir.path()).expect("should parse");
    let merged = index.merged_surface();
    let names: Vec<&str> = merged.iter().map(|m| m.name.as_str()).collect();

    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted, "merged surface should be alphabetical");
}

// ---- against the user's real Laravel install ---------------------------

// ---- Model-static collision suppression -------------------------------

#[test]
fn parses_model_static_method_names_from_fake_vendor() {
    let dir = TempDir::new().unwrap();
    fake_vendor(&dir, Some(MINIMAL_BUILDER), Some(MINIMAL_QUERY_BUILDER));
    // Also drop a minimal Model.php with two real public statics.
    let model_content = r#"<?php
namespace Illuminate\Database\Eloquent;

abstract class Model {
    public static function all($columns = ['*']) {}
    public static function with($relations) {
        return static::query()->with(is_string($relations) ? func_get_args() : $relations);
    }
    public function nonStatic() {}
    public static function __callStatic($method, $params) {}
}
"#;
    let model_path = dir.path().join(ELOQUENT_MODEL_REL_PATH);
    fs::create_dir_all(model_path.parent().unwrap()).unwrap();
    fs::write(model_path, model_content).unwrap();

    let index = parse_builder_methods(dir.path()).expect("should parse");

    assert!(
        index.model_static_method_names.contains("all"),
        "Model::all should be picked up"
    );
    assert!(
        index.model_static_method_names.contains("with"),
        "Model::with should be picked up"
    );
    assert!(
        !index.model_static_method_names.contains("nonStatic"),
        "non-static methods must not appear"
    );
    assert!(
        !index.model_static_method_names.contains("__callStatic"),
        "magic methods must not appear"
    );
}

#[test]
fn merged_surface_suppresses_methods_that_collide_with_model_statics() {
    // Build an index where Builder has `where` (which doesn't really
    // collide in Laravel, but the suppression mechanism doesn't care
    // about the specific name). Model's static set claims `where`.
    // `where` must not appear in the merged surface.
    use std::collections::HashSet;
    let dir = TempDir::new().unwrap();
    fake_vendor(&dir, Some(MINIMAL_BUILDER), Some(MINIMAL_QUERY_BUILDER));
    let model_content = r#"<?php
namespace Illuminate\Database\Eloquent;
abstract class Model {
    public static function where($column) {}
}
"#;
    let model_path = dir.path().join(ELOQUENT_MODEL_REL_PATH);
    fs::create_dir_all(model_path.parent().unwrap()).unwrap();
    fs::write(model_path, model_content).unwrap();

    let index = parse_builder_methods(dir.path()).expect("should parse");
    assert!(
        index.model_static_method_names.contains("where"),
        "test setup: Model::where should be in the static set"
    );
    let merged = index.merged_surface();
    let names: HashSet<&str> = merged.iter().map(|m| m.name.as_str()).collect();
    assert!(
        !names.contains("where"),
        "`where` should be suppressed because Model has a real static of the same name"
    );
    // Other methods still come through.
    assert!(names.contains("find"), "find should still appear");
}

#[test]
fn missing_model_php_does_not_break_index() {
    // Half-installed vendor (Builder files present, Model absent) should
    // still build an index — model_static_method_names is just empty.
    let dir = TempDir::new().unwrap();
    fake_vendor(&dir, Some(MINIMAL_BUILDER), Some(MINIMAL_QUERY_BUILDER));
    let index = parse_builder_methods(dir.path()).expect("should parse");
    assert!(index.model_static_method_names.is_empty());
    // Nothing is suppressed when the set is empty.
    let merged = index.merged_surface();
    assert!(merged.iter().any(|m| m.name == "where"));
}

/// End-to-end check that real `where()` from Laravel renders with
/// `$this` resolved to `Builder<static>` in the panel's @return tag.
/// This is the bug Mike reported — tests pass on synthetic input but
/// the real install might have something different.
#[test]
fn real_laravel_where_method_panel_resolves_self() {
    use crate::completion_format::{format_phpdoc_tag_with, split_phpdoc};

    let candidate_roots = [PathBuf::from("/Users/mike/Developer/Sites/decisioncloud")];
    let Some(root) = candidate_roots
        .iter()
        .find(|p| p.join(ELOQUENT_BUILDER_REL_PATH).exists())
    else {
        eprintln!("[skip] no real Laravel vendor available");
        return;
    };

    let index = parse_builder_methods(root).expect("real install should parse");
    let where_method = index
        .eloquent_builder
        .iter()
        .find(|m| m.name == "where")
        .expect("where method");

    eprintln!("[real-where] source_class = {:?}", where_method.source_class);
    eprintln!("[real-where] return_type = {:?}", where_method.return_type);
    eprintln!(
        "[real-where] doc_body (first 200 chars) = {:?}",
        where_method.doc_body.as_deref().map(|s| &s[..s.len().min(200)])
    );

    let doc_body = where_method.doc_body.as_deref().expect("where should have docblock");
    let (_summary, tags) = split_phpdoc(doc_body);
    eprintln!("[real-where] tags = {:#?}", tags);

    // Find the @return tag
    let return_tag = tags
        .iter()
        .find(|t| t.starts_with("@return"))
        .expect("@return tag should exist");
    let formatted =
        format_phpdoc_tag_with(return_tag, Some(&where_method.source_class));
    eprintln!("[real-where] formatted @return = {:?}", formatted);
    assert!(
        formatted.contains("Builder<static>"),
        "expected @return resolved to Builder<static>, got: {formatted}"
    );
}

/// Integration-style: parse the actual vendored Eloquent\Builder.php at
/// the path that the developer has checked out. Skipped if vendor isn't
/// installed (CI on a fresh checkout). Asserts the shape we expect — at
/// least the `where` family + some terminators.
#[test]
fn parses_real_laravel_install_when_available() {
    // Best-effort: walk up from CWD looking for a sibling Laravel project's
    // vendor dir. The dev branch's known path; skips silently if missing
    // so this passes on CI / fresh checkouts.
    let candidate_roots = [
        PathBuf::from("/Users/mike/Developer/Sites/decisioncloud"),
    ];
    let Some(root) = candidate_roots
        .iter()
        .find(|p| p.join(ELOQUENT_BUILDER_REL_PATH).exists())
    else {
        eprintln!("[skip] no real Laravel vendor available; skipping integration check");
        return;
    };

    let index = parse_builder_methods(root).expect("real install should parse");

    // Print counts to stderr — visible with `cargo test ... -- --nocapture`,
    // silent in normal runs. Useful when iterating on the parser.
    eprintln!(
        "[real-install] Eloquent: {}, Query: {}, merged: {}",
        index.eloquent_builder.len(),
        index.query_builder.len(),
        index.merged_surface().len()
    );

    // Check the *merged* surface (Eloquent + Query Builder) because that's
    // what completion actually emits. Real-world distinction worth noting:
    // `whereIn` is defined on Query\Builder only — Eloquent\Builder doesn't
    // override it because the base version already returns the right type.
    // A test against just `index.eloquent_builder` would miss `whereIn`,
    // `select`, `orderBy`, etc.
    let merged = index.merged_surface();
    let names: Vec<&str> = merged.iter().map(|m| m.name.as_str()).collect();

    // Note: `with` is deliberately NOT in this list — it's the one
    // documented collision with Model's real static, so it's suppressed
    // from the merged surface. See the explicit assertion further down.
    for expected in [
        "where", "whereIn", "whereNotIn", "whereNull", "find", "first", "get",
        "orderBy", "select", "count",
    ] {
        assert!(
            names.contains(&expected),
            "expected {expected} in real Laravel install's merged surface, \
             got {} total methods (Eloquent: {}, Query: {})",
            merged.len(),
            index.eloquent_builder.len(),
            index.query_builder.len(),
        );
    }

    assert!(
        merged.len() > 100,
        "expected at least 100 methods in merged surface, got {} (Eloquent: {}, Query: {})",
        merged.len(),
        index.eloquent_builder.len(),
        index.query_builder.len()
    );

    // Real-world suppression check: `with` exists on both Model (as a
    // real public static) and Builder. PHP resolves the static call to
    // Model's version, so our emission must NOT include `with` —
    // shipping Builder's two-arg signature would mislead users into
    // writing `Model::with('rel', $closure)` which silently doesn't
    // work (the closure gets packed into the relations array by Model's
    // func_get_args dispatch).
    assert!(
        index.model_static_method_names.contains("with"),
        "real Laravel install: Model::with should be in the static set"
    );
    assert!(
        !names.contains(&"with"),
        "real Laravel install: `with` should be suppressed from the merged Builder surface"
    );
}
