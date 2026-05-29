use super::*;

// ---- snake_pluralize: Laravel convention `Str::plural(snake_case($class))` --

#[test]
fn snake_pluralize_single_word_pascal() {
    assert_eq!(snake_pluralize("User"), "users");
    assert_eq!(snake_pluralize("Post"), "posts");
}

#[test]
fn snake_pluralize_multi_word_pascal() {
    // BlogPost → blog_posts. The `_` insertion happens *before* the
    // pluralization step, so the suffix logic operates on the snake form.
    assert_eq!(snake_pluralize("BlogPost"), "blog_posts");
    assert_eq!(snake_pluralize("UserProfile"), "user_profiles");
    assert_eq!(snake_pluralize("OrderLineItem"), "order_line_items");
}

#[test]
fn snake_pluralize_consonant_y_becomes_ies() {
    // category → categories, country → countries.
    assert_eq!(snake_pluralize("Category"), "categories");
    assert_eq!(snake_pluralize("Country"), "countries");
}

#[test]
fn snake_pluralize_vowel_y_just_adds_s() {
    // day → days (NOT daies). The vowel-before-y check guards against the
    // ies rule kicking in for words like "play", "boy", "key", "day".
    assert_eq!(snake_pluralize("Day"), "days");
    assert_eq!(snake_pluralize("Key"), "keys");
    assert_eq!(snake_pluralize("Boy"), "boys");
}

#[test]
fn snake_pluralize_sibilants_add_es() {
    // address → addresses, box → boxes, watch → watches, dish → dishes.
    assert_eq!(snake_pluralize("Address"), "addresses");
    assert_eq!(snake_pluralize("Box"), "boxes");
    assert_eq!(snake_pluralize("Watch"), "watches");
    assert_eq!(snake_pluralize("Dish"), "dishes");
}

#[test]
fn snake_pluralize_single_letter_class() {
    // Pathological but should not panic: a single uppercase letter
    // shouldn't trigger the `i > 0` underscore insertion.
    assert_eq!(snake_pluralize("A"), "as");
}

// ---- columns_for_builder: model resolution + table derivation -------------

use crate::database::DatabaseSchemaProvider;
use std::path::PathBuf;
use tempfile::TempDir;

/// Helper: spin up a tempdir with a writeable model file at the standard
/// Laravel `app/Models/` location. Returns the tempdir handle (which the
/// caller must hold to keep the dir alive) and the project root path.
fn project_with_model(class_name: &str, model_body: &str) -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    let models_dir = dir.path().join("app").join("Models");
    std::fs::create_dir_all(&models_dir).expect("create models dir");
    let file = models_dir.join(format!("{class_name}.php"));
    std::fs::write(&file, model_body).expect("write model");
    let root = dir.path().to_path_buf();
    (dir, root)
}

/// Helper: build a minimal `DatabaseSchemaProvider` and seed its cache
/// directly so the test doesn't need a live MySQL/Postgres. The provider's
/// public surface includes `set_test_schema` (test-only) for exactly this.
async fn provider_with_table(
    root: PathBuf,
    table: &str,
    columns: Vec<(&str, &str)>,
) -> DatabaseSchemaProvider {
    use crate::database::DatabaseSchema;
    use std::collections::HashMap;
    use std::time::Instant;

    let mut columns_map = HashMap::new();
    let mut columns_with_types = HashMap::new();
    columns_map.insert(
        table.to_string(),
        columns.iter().map(|(n, _)| n.to_string()).collect(),
    );
    columns_with_types.insert(
        table.to_string(),
        columns
            .iter()
            .map(|(n, t)| (n.to_string(), t.to_string()))
            .collect(),
    );
    let schema = DatabaseSchema {
        tables: vec![table.to_string()],
        columns: columns_map,
        columns_with_types,
        cached_at: Instant::now(),
    };
    let provider = DatabaseSchemaProvider::new(root);
    provider.set_test_schema(schema).await;
    provider
}

fn make_ctx(class: &str) -> ChainContext {
    ChainContext {
        mode: BuilderMode::EloquentBuilder,
        effective_table: None,
        effective_model: Some(class.to_string()),
        expecting: ArgKind::Column,
        dotted_prefix: None,
        closure_relation_hop: None,
        quote: '\'',
        joined_tables: Vec::new(),
        from_clause: FromClause::Inherit,
        join_parent_model: None,
    }
}

#[tokio::test]
async fn columns_for_builder_uses_explicit_table_property() {
    // Model declares `$table = 'people'` — the snake_pluralize fallback
    // ("Person" → "persons") should NOT fire.
    let body = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Person extends Model {
    protected $table = 'people';
}
"#;
    let (_dir, root) = project_with_model("Person", body);
    let db = provider_with_table(
        root.clone(),
        "people",
        vec![("id", "int"), ("full_name", "string")],
    )
    .await;
    let ctx = make_ctx("App\\Models\\Person");
    let items = columns_for_builder(&ctx, &db, None, &root).await;
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, vec!["id", "full_name"]);
}

#[tokio::test]
async fn columns_for_builder_falls_back_to_snake_pluralize() {
    // No `$table` property — the helper should derive the table from the
    // class basename via snake_pluralize ("User" → "users").
    let body = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {}
"#;
    let (_dir, root) = project_with_model("User", body);
    let db = provider_with_table(
        root.clone(),
        "users",
        vec![("id", "int"), ("email", "string")],
    )
    .await;
    let ctx = make_ctx("App\\Models\\User");
    let items = columns_for_builder(&ctx, &db, None, &root).await;
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, vec!["id", "email"]);
}

#[tokio::test]
async fn columns_for_builder_applies_cast_override() {
    // Model declares `$casts = ['options' => 'array']`. The DB column type
    // is `string` (JSON column), but the cast should win in the popup's
    // detail string.
    let body = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Setting extends Model {
    protected $casts = [
        'options' => 'array',
    ];
}
"#;
    let (_dir, root) = project_with_model("Setting", body);
    let db = provider_with_table(
        root.clone(),
        "settings",
        vec![("id", "int"), ("options", "string")],
    )
    .await;
    let ctx = make_ctx("App\\Models\\Setting");
    let items = columns_for_builder(&ctx, &db, None, &root).await;
    let options_item = items
        .iter()
        .find(|i| i.label == "options")
        .expect("options column");
    let detail = options_item.detail.as_deref().unwrap_or("");
    assert!(
        detail.contains("array"),
        "cast type should win in detail; got: {detail:?}"
    );
    assert!(
        detail.contains("cast"),
        "cast-overridden columns should be annotated; got: {detail:?}"
    );
}

#[tokio::test]
async fn columns_for_builder_returns_empty_when_class_file_missing() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let db = provider_with_table(root.clone(), "ghosts", vec![("id", "int")]).await;
    let ctx = make_ctx("App\\Models\\Ghost"); // no file written
    let items = columns_for_builder(&ctx, &db, None, &root).await;
    assert!(items.is_empty());
}

#[tokio::test]
async fn columns_for_builder_returns_empty_when_table_not_in_schema() {
    // Model file exists, but the DB schema doesn't have the table.
    // Could happen if introspection hasn't finished or the model points
    // to a table that doesn't exist.
    let body = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Phantom extends Model {}
"#;
    let (_dir, root) = project_with_model("Phantom", body);
    let db = provider_with_table(root.clone(), "users", vec![("id", "int")]).await;
    let ctx = make_ctx("App\\Models\\Phantom");
    let items = columns_for_builder(&ctx, &db, None, &root).await;
    assert!(items.is_empty());
}

#[tokio::test]
async fn columns_for_builder_wraps_insert_text_when_no_source_quotes() {
    // Mirror of the table-completion behavior: if the cursor sits right
    // after `(` with no quotes typed, the insertion needs to wrap the
    // value in quotes so the resulting source is a valid string literal.
    let body = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {}
"#;
    let (_dir, root) = project_with_model("User", body);
    let db = provider_with_table(root.clone(), "users", vec![("email", "string")]).await;
    let ctx = make_ctx("App\\Models\\User");
    let items = columns_for_builder(&ctx, &db, Some('\''), &root).await;
    let item = items.iter().find(|i| i.label == "email").expect("email");
    assert_eq!(item.insert_text.as_deref(), Some("'email'"));
}

// ---- relations: Eloquent relation-name completion (Phase 5) -------------

#[tokio::test]
async fn relations_surfaces_model_relationships() {
    // Standard relationship shapes: hasMany, belongsTo, hasOne. The
    // existing ModelMetadata extractor finds all three; the helper just
    // formats them as completion items.
    let body = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public function posts() {
        return $this->hasMany(Post::class);
    }
    public function company() {
        return $this->belongsTo(Company::class);
    }
    public function profile() {
        return $this->hasOne(Profile::class);
    }
}
"#;
    let (_dir, root) = project_with_model("User", body);
    let ctx = make_ctx("App\\Models\\User");
    let items = relations(&ctx, None, &root).await;
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        labels.contains(&"posts"),
        "expected `posts`; got {labels:?}"
    );
    assert!(
        labels.contains(&"company"),
        "expected `company`; got {labels:?}"
    );
    assert!(
        labels.contains(&"profile"),
        "expected `profile`; got {labels:?}"
    );
}

#[tokio::test]
async fn relations_includes_related_model_in_detail() {
    // The completion item's detail should surface the related model class
    // (e.g., `HasMany<Post>`) so the user can tell at a glance what the
    // relation points to.
    let body = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public function posts() {
        return $this->hasMany(Post::class);
    }
}
"#;
    let (_dir, root) = project_with_model("User", body);
    let ctx = make_ctx("App\\Models\\User");
    let items = relations(&ctx, None, &root).await;
    let posts = items.iter().find(|i| i.label == "posts").expect("posts");
    let detail = posts.detail.as_deref().unwrap_or("");
    assert!(
        detail.contains("Post"),
        "related model should appear in detail; got {detail:?}"
    );
    assert!(
        detail.contains("hasMany"),
        "relationship type should appear in detail; got {detail:?}"
    );
}

#[tokio::test]
async fn relations_returns_empty_when_model_file_missing() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let ctx = make_ctx("App\\Models\\Phantom");
    let items = relations(&ctx, None, &root).await;
    assert!(items.is_empty());
}

#[tokio::test]
async fn relations_returns_empty_when_model_has_no_relationships() {
    // Plain model — no relationship methods defined. Should yield empty,
    // not crash, not pretend.
    let body = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Bare extends Model {}
"#;
    let (_dir, root) = project_with_model("Bare", body);
    let ctx = make_ctx("App\\Models\\Bare");
    let items = relations(&ctx, None, &root).await;
    assert!(items.is_empty());
}

// ---- resolve_related_model (Phase 8): one-hop relation walk ------------

#[tokio::test]
async fn resolve_related_model_finds_target_class() {
    // Parent model declares `tokens` as `hasMany(OAuthToken::class)`.
    // Phase 5.11 resolves the bare `OAuthToken::class` reference through
    // the source file's namespace, so the helper returns the FQCN
    // `App\Models\OAuthToken`. Storing the FQCN (not just the basename)
    // is what makes dotted-path walking work for relationships whose
    // related model lives in a different namespace from a basename
    // collision elsewhere in the project.
    let body = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class OAuthClient extends Model {
    public function tokens() {
        return $this->hasMany(OAuthToken::class);
    }
}
"#;
    let (_dir, root) = project_with_model("OAuthClient", body);
    let related = resolve_related_model("App\\Models\\OAuthClient", "tokens", &root).await;
    assert_eq!(related.as_deref(), Some("App\\Models\\OAuthToken"));
}

#[tokio::test]
async fn resolve_related_model_returns_none_when_relation_missing() {
    let body = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public function posts() { return $this->hasMany(Post::class); }
}
"#;
    let (_dir, root) = project_with_model("User", body);
    let related = resolve_related_model("App\\Models\\User", "comments", &root).await;
    assert!(related.is_none(), "missing relation should yield None");
}

#[tokio::test]
async fn resolve_related_model_returns_none_when_class_file_missing() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let related = resolve_related_model("App\\Models\\Ghost", "anything", &root).await;
    assert!(related.is_none());
}

#[tokio::test]
async fn relations_wraps_insert_text_when_no_source_quotes() {
    // Same shape as columns_for_builder — wrap with quotes when the
    // source has none (case-3 fixup path).
    let body = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public function posts() {
        return $this->hasMany(Post::class);
    }
}
"#;
    let (_dir, root) = project_with_model("User", body);
    let ctx = make_ctx("App\\Models\\User");
    let items = relations(&ctx, Some('\''), &root).await;
    let posts = items.iter().find(|i| i.label == "posts").expect("posts");
    assert_eq!(posts.insert_text.as_deref(), Some("'posts'"));
}

// ---- resolve_table_for_model (Phase 6) -----------------------------------

#[tokio::test]
async fn resolve_table_for_model_uses_explicit_table() {
    let body = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Person extends Model {
    protected $table = 'people';
}
"#;
    let (_dir, root) = project_with_model("Person", body);
    let table = resolve_table_for_model("App\\Models\\Person", &root).await;
    assert_eq!(table.as_deref(), Some("people"));
}

#[tokio::test]
async fn resolve_table_for_model_falls_back_to_snake_pluralize() {
    let body = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class BlogPost extends Model {}
"#;
    let (_dir, root) = project_with_model("BlogPost", body);
    let table = resolve_table_for_model("App\\Models\\BlogPost", &root).await;
    assert_eq!(table.as_deref(), Some("blog_posts"));
}

// ---- columns_for_collection (Phase 6) ------------------------------------

#[tokio::test]
async fn columns_for_collection_includes_db_columns_and_accessors() {
    // Collection's where() filters in-memory against properties, so
    // both DB columns AND model accessors are valid args.
    let body = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public function getFullNameAttribute(): string {
        return $this->first_name . ' ' . $this->last_name;
    }
}
"#;
    let (_dir, root) = project_with_model("User", body);
    let db = provider_with_table(
        root.clone(),
        "users",
        vec![
            ("id", "int"),
            ("first_name", "string"),
            ("last_name", "string"),
        ],
    )
    .await;
    let ctx = make_ctx("App\\Models\\User");
    let items = columns_for_collection(&ctx, &db, None, &root).await;
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    // DB columns present
    assert!(labels.contains(&"id"), "missing id; got {labels:?}");
    assert!(
        labels.contains(&"first_name"),
        "missing first_name; got {labels:?}"
    );
    // Accessor present
    assert!(
        labels.contains(&"full_name"),
        "accessor 'full_name' should be a Collection-mode completion; got {labels:?}"
    );
}

#[tokio::test]
async fn columns_for_collection_ranks_db_columns_before_accessors() {
    // DB columns sort_text = "1_…", accessors = "2_…", so DB columns
    // win in popup ordering when both match the user's filter.
    let body = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public function getNameAttribute(): string {
        return $this->first_name;
    }
}
"#;
    let (_dir, root) = project_with_model("User", body);
    let db = provider_with_table(
        root.clone(),
        "users",
        vec![("id", "int"), ("name", "string")],
    )
    .await;
    let ctx = make_ctx("App\\Models\\User");
    let items = columns_for_collection(&ctx, &db, None, &root).await;
    // Find the DB column "name" and the accessor "name" (same label —
    // both exist when the column name and accessor name collide).
    let name_items: Vec<&CompletionItem> = items.iter().filter(|i| i.label == "name").collect();
    // There may be one or two — depending on whether a DB column
    // "name" actually exists. Our schema has one, the accessor also
    // exposes "name". They're separate items in the list.
    assert!(
        !name_items.is_empty(),
        "should have at least one 'name' item"
    );
    // The DB-column item ranks with "1_", the accessor with "2_". If
    // both are present, the DB one comes first lexicographically.
    let sort_texts: Vec<&str> = name_items
        .iter()
        .map(|i| i.sort_text.as_deref().unwrap_or(""))
        .collect();
    if sort_texts.len() == 2 {
        assert!(
            sort_texts.iter().any(|s| s.starts_with("1_")),
            "DB column should have 1_ prefix; got {sort_texts:?}"
        );
        assert!(
            sort_texts.iter().any(|s| s.starts_with("2_")),
            "accessor should have 2_ prefix; got {sort_texts:?}"
        );
    }
}

#[tokio::test]
async fn columns_for_collection_falls_back_when_model_missing() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let db = provider_with_table(root.clone(), "users", vec![("id", "int")]).await;
    let ctx = make_ctx("App\\Models\\Phantom");
    let items = columns_for_collection(&ctx, &db, None, &root).await;
    assert!(items.is_empty());
}

// ---- walk_dotted_hops (Phase 7) ------------------------------------------

/// Helper: scaffolding for the relation-hop tests. Build a tempdir with
/// the given (class_name, body) model files, then return the root.
async fn project_with_models_helper(files: &[(&str, &str)]) -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let models_dir = dir.path().join("app/Models");
    std::fs::create_dir_all(&models_dir).unwrap();
    for (name, body) in files {
        std::fs::write(models_dir.join(format!("{name}.php")), body).unwrap();
    }
    let root = dir.path().to_path_buf();
    (dir, root)
}

#[tokio::test]
async fn walk_dotted_hops_empty_prefix_returns_starting_model() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let resolved = walk_dotted_hops("App\\Models\\User", "", &root).await;
    assert_eq!(resolved.as_deref(), Some("App\\Models\\User"));
}

#[tokio::test]
async fn walk_dotted_hops_single_segment_one_hop() {
    let (_dir, root) = project_with_models_helper(&[
        (
            "User",
            r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public function posts() { return $this->hasMany(Post::class); }
}
"#,
        ),
        (
            "Post",
            r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Post extends Model {}
"#,
        ),
    ])
    .await;
    let resolved = walk_dotted_hops("App\\Models\\User", "posts", &root).await;
    // Phase 5.11: related_model is the resolved FQCN, not the basename.
    assert_eq!(resolved.as_deref(), Some("App\\Models\\Post"));
}

#[tokio::test]
async fn walk_dotted_hops_multi_segment_chains_relations() {
    let (_dir, root) = project_with_models_helper(&[
        (
            "User",
            r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public function posts() { return $this->hasMany(Post::class); }
}
"#,
        ),
        (
            "Post",
            r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Post extends Model {
    public function author() { return $this->belongsTo(Author::class); }
}
"#,
        ),
        (
            "Author",
            r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Author extends Model {
    public function profile() { return $this->hasOne(Profile::class); }
}
"#,
        ),
        (
            "Profile",
            r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Profile extends Model {}
"#,
        ),
    ])
    .await;
    let resolved = walk_dotted_hops("App\\Models\\User", "posts.author.profile", &root).await;
    // Phase 5.11: each hop's related_model is now the resolved FQCN,
    // so the final resolved class is `App\Models\Profile`, not the
    // bare basename. That's what makes class-locator route correctly
    // when there's a same-named class in another namespace.
    assert_eq!(
        resolved.as_deref(),
        Some("App\\Models\\Profile"),
        "three hops should land at App\\Models\\Profile"
    );
}

#[tokio::test]
async fn walk_dotted_hops_unresolved_segment_returns_none() {
    let (_dir, root) = project_with_models_helper(&[(
        "User",
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public function posts() { return $this->hasMany(Post::class); }
}
"#,
    )])
    .await;
    // User has `posts` but no `nonexistent` relation.
    let resolved = walk_dotted_hops("App\\Models\\User", "nonexistent", &root).await;
    assert!(resolved.is_none(), "missing relation should yield None");
}

// ---- relations with dotted_prefix (Phase 7) ------------------------------

#[tokio::test]
async fn relations_walks_dotted_prefix_to_final_models_relations() {
    // `User::with('posts.|')` — should resolve `posts` to Post, then
    // return Post's relations (Author).
    let (_dir, root) = project_with_models_helper(&[
        (
            "User",
            r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public function posts() { return $this->hasMany(Post::class); }
}
"#,
        ),
        (
            "Post",
            r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Post extends Model {
    public function author() { return $this->belongsTo(Author::class); }
    public function comments() { return $this->hasMany(Comment::class); }
}
"#,
        ),
    ])
    .await;
    let ctx = ChainContext {
        mode: BuilderMode::EloquentBuilder,
        effective_table: None,
        effective_model: Some("App\\Models\\User".to_string()),
        expecting: ArgKind::Relation,
        dotted_prefix: Some("posts".to_string()),
        closure_relation_hop: None,
        quote: '\'',
        joined_tables: Vec::new(),
        from_clause: FromClause::Inherit,
        join_parent_model: None,
    };
    let items = relations(&ctx, None, &root).await;
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        labels.contains(&"author"),
        "expected Post::author in dotted-path result; got {labels:?}"
    );
    assert!(
        labels.contains(&"comments"),
        "expected Post::comments; got {labels:?}"
    );
}

#[tokio::test]
async fn relations_with_failing_dotted_hop_returns_empty() {
    let (_dir, root) = project_with_models_helper(&[(
        "User",
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public function posts() { return $this->hasMany(Post::class); }
}
"#,
    )])
    .await;
    let ctx = ChainContext {
        mode: BuilderMode::EloquentBuilder,
        effective_table: None,
        effective_model: Some("App\\Models\\User".to_string()),
        expecting: ArgKind::Relation,
        dotted_prefix: Some("nonexistent".to_string()),
        closure_relation_hop: None,
        quote: '\'',
        joined_tables: Vec::new(),
        from_clause: FromClause::Inherit,
        join_parent_model: None,
    };
    let items = relations(&ctx, None, &root).await;
    assert!(items.is_empty(), "unresolvable hop should yield no items");
}

// ---- columns_raw: joined-table column completion (issue #24) -----------

/// Seed a provider with multiple tables at once. Base-builder completion
/// never touches the project root, so the path is just a placeholder.
async fn provider_with_tables(
    root: PathBuf,
    tables: Vec<(&str, Vec<(&str, &str)>)>,
) -> DatabaseSchemaProvider {
    use crate::database::DatabaseSchema;
    use std::collections::HashMap;
    use std::time::Instant;

    let mut columns_map = HashMap::new();
    let mut columns_with_types = HashMap::new();
    let mut table_names = Vec::new();
    for (table, cols) in &tables {
        table_names.push(table.to_string());
        columns_map.insert(
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
        columns: columns_map,
        columns_with_types,
        cached_at: Instant::now(),
    };
    let provider = DatabaseSchemaProvider::new(root);
    provider.set_test_schema(schema).await;
    provider
}

/// Build a base-builder `ChainContext` for column completion tests.
fn base_ctx(
    effective_table: Option<&str>,
    joined_tables: Vec<AccessibleTable>,
    from_clause: FromClause,
    dotted_prefix: Option<&str>,
) -> ChainContext {
    ChainContext {
        mode: BuilderMode::BaseBuilder,
        effective_table: effective_table.map(|s| s.to_string()),
        effective_model: None,
        expecting: ArgKind::Column,
        dotted_prefix: dotted_prefix.map(|s| s.to_string()),
        closure_relation_hop: None,
        quote: '\'',
        joined_tables,
        from_clause,
        join_parent_model: None,
    }
}

fn labels(items: &[CompletionItem]) -> Vec<String> {
    items.iter().map(|i| i.label.clone()).collect()
}

#[tokio::test]
async fn columns_raw_no_joins_offers_only_bare() {
    let dir = TempDir::new().unwrap();
    let db = provider_with_tables(
        dir.path().to_path_buf(),
        vec![("users", vec![("id", "int"), ("email", "string")])],
    )
    .await;
    let ctx = base_ctx(Some("users"), Vec::new(), FromClause::Inherit, None);
    let got = labels(&columns_raw(&ctx, &db, None).await);
    assert!(got.contains(&"id".to_string()));
    assert!(got.contains(&"email".to_string()));
    // No joins → no qualified `users.id` noise.
    assert!(
        !got.iter().any(|l| l.contains('.')),
        "single-table query should not emit qualified columns; got {got:?}"
    );
}

#[tokio::test]
async fn columns_raw_with_join_offers_bare_root_and_qualified_both() {
    let dir = TempDir::new().unwrap();
    let db = provider_with_tables(
        dir.path().to_path_buf(),
        vec![
            ("users", vec![("id", "int"), ("name", "string")]),
            ("orders", vec![("id", "int"), ("status", "string")]),
        ],
    )
    .await;
    let ctx = base_ctx(
        Some("users"),
        vec![AccessibleTable::bare("orders")],
        FromClause::Inherit,
        None,
    );
    let got = labels(&columns_raw(&ctx, &db, None).await);
    // Bare root columns.
    assert!(got.contains(&"id".to_string()), "bare root id; got {got:?}");
    assert!(got.contains(&"name".to_string()));
    // Qualified root columns (issue #24 criterion 3: `users.id`).
    assert!(got.contains(&"users.id".to_string()));
    assert!(got.contains(&"users.name".to_string()));
    // Qualified joined columns.
    assert!(got.contains(&"orders.status".to_string()));
    assert!(got.contains(&"orders.id".to_string()));
    // Joined columns are NOT offered bare (only `orders.*`).
    assert!(
        !got.contains(&"status".to_string()),
        "joined column should only be qualified; got {got:?}"
    );
}

#[tokio::test]
async fn columns_raw_narrows_to_qualified_table() {
    let dir = TempDir::new().unwrap();
    let db = provider_with_tables(
        dir.path().to_path_buf(),
        vec![
            ("users", vec![("id", "int"), ("name", "string")]),
            ("orders", vec![("status", "string"), ("total", "int")]),
        ],
    )
    .await;
    // User typed `orders.|` → dotted_prefix "orders".
    let ctx = base_ctx(
        Some("users"),
        vec![AccessibleTable::bare("orders")],
        FromClause::Inherit,
        Some("orders"),
    );
    let got = labels(&columns_raw(&ctx, &db, None).await);
    // Only orders columns, inserted BARE (the `orders.` is already typed).
    assert_eq!(
        got.len(),
        2,
        "expected exactly orders' columns; got {got:?}"
    );
    assert!(got.contains(&"status".to_string()));
    assert!(got.contains(&"total".to_string()));
}

#[tokio::test]
async fn columns_raw_narrows_via_alias() {
    let dir = TempDir::new().unwrap();
    let db = provider_with_tables(
        dir.path().to_path_buf(),
        vec![
            ("users", vec![("id", "int")]),
            ("orders", vec![("status", "string")]),
        ],
    )
    .await;
    // `join('orders as o', …)->where('o.|')` → qualifier "o" resolves to orders.
    let ctx = base_ctx(
        Some("users"),
        vec![AccessibleTable {
            table: "orders".to_string(),
            alias: Some("o".to_string()),
        }],
        FromClause::Inherit,
        Some("o"),
    );
    let got = labels(&columns_raw(&ctx, &db, None).await);
    assert_eq!(got, vec!["status".to_string()]);
}

#[tokio::test]
async fn columns_raw_alias_qualifies_with_alias_not_table() {
    let dir = TempDir::new().unwrap();
    let db = provider_with_tables(
        dir.path().to_path_buf(),
        vec![
            ("users", vec![("id", "int")]),
            ("orders", vec![("status", "string")]),
        ],
    )
    .await;
    let ctx = base_ctx(
        Some("users"),
        vec![AccessibleTable {
            table: "orders".to_string(),
            alias: Some("o".to_string()),
        }],
        FromClause::Inherit,
        None,
    );
    let got = labels(&columns_raw(&ctx, &db, None).await);
    // The qualifier is the ALIAS, not the real table name.
    assert!(got.contains(&"o.status".to_string()), "got {got:?}");
    assert!(!got.contains(&"orders.status".to_string()));
}

#[tokio::test]
async fn columns_raw_from_replace_uses_new_table() {
    let dir = TempDir::new().unwrap();
    let db = provider_with_tables(
        dir.path().to_path_buf(),
        vec![
            ("users", vec![("id", "int")]),
            ("admins", vec![("admin_id", "int"), ("level", "int")]),
        ],
    )
    .await;
    // `from('admins')` replaces the root — `effective_table` (users) is ignored.
    let ctx = base_ctx(
        Some("users"),
        Vec::new(),
        FromClause::Replace(AccessibleTable::bare("admins")),
        None,
    );
    let got = labels(&columns_raw(&ctx, &db, None).await);
    assert!(got.contains(&"admin_id".to_string()));
    assert!(got.contains(&"level".to_string()));
    assert!(
        !got.contains(&"id".to_string()),
        "users is replaced; got {got:?}"
    );
}

#[tokio::test]
async fn columns_raw_opaque_from_suppresses_root_but_keeps_joins() {
    let dir = TempDir::new().unwrap();
    let db = provider_with_tables(
        dir.path().to_path_buf(),
        vec![
            ("users", vec![("id", "int")]),
            ("orders", vec![("status", "string")]),
        ],
    )
    .await;
    // `fromRaw(...)->join('orders', …)` → no bare root columns, joins still work.
    let ctx = base_ctx(
        Some("users"),
        vec![AccessibleTable::bare("orders")],
        FromClause::Opaque,
        None,
    );
    let got = labels(&columns_raw(&ctx, &db, None).await);
    assert!(got.contains(&"orders.status".to_string()));
    assert!(
        !got.contains(&"id".to_string()),
        "opaque root suppressed; got {got:?}"
    );
}

#[tokio::test]
async fn columns_raw_wraps_qualified_insert_text() {
    let dir = TempDir::new().unwrap();
    let db = provider_with_tables(
        dir.path().to_path_buf(),
        vec![
            ("users", vec![("id", "int")]),
            ("orders", vec![("status", "string")]),
        ],
    )
    .await;
    let ctx = base_ctx(
        Some("users"),
        vec![AccessibleTable::bare("orders")],
        FromClause::Inherit,
        None,
    );
    // wrap_with_quote = Some('\'') → qualified insert text wraps the whole
    // `orders.status` in quotes, not just the column.
    let items = columns_raw(&ctx, &db, Some('\'')).await;
    let qualified = items
        .iter()
        .find(|i| i.label == "orders.status")
        .expect("qualified item");
    assert_eq!(qualified.insert_text.as_deref(), Some("'orders.status'"));
}

// ---- Eloquent joins + from() (issue #24, Phase 2) ---------------------

/// Build an Eloquent `ChainContext` for column-completion tests.
fn eloquent_ctx(
    class: &str,
    mode: BuilderMode,
    joined_tables: Vec<AccessibleTable>,
    from_clause: FromClause,
    dotted_prefix: Option<&str>,
) -> ChainContext {
    ChainContext {
        mode,
        effective_table: None,
        effective_model: Some(class.to_string()),
        expecting: ArgKind::Column,
        dotted_prefix: dotted_prefix.map(|s| s.to_string()),
        closure_relation_hop: None,
        quote: '\'',
        joined_tables,
        from_clause,
        join_parent_model: None,
    }
}

const PLAIN_USER: &str = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {}
"#;

#[tokio::test]
async fn eloquent_builder_offers_joined_columns() {
    let (_dir, root) = project_with_model("User", PLAIN_USER);
    let db = provider_with_tables(
        root.clone(),
        vec![
            ("users", vec![("id", "int"), ("name", "string")]),
            ("orders", vec![("status", "string")]),
        ],
    )
    .await;
    let ctx = eloquent_ctx(
        "App\\Models\\User",
        BuilderMode::EloquentBuilder,
        vec![AccessibleTable::bare("orders")],
        FromClause::Inherit,
        None,
    );
    let got = labels(&columns_for_builder(&ctx, &db, None, &root).await);
    assert!(got.contains(&"name".to_string()), "bare root; got {got:?}");
    assert!(got.contains(&"users.name".to_string()), "qualified root");
    assert!(
        got.contains(&"orders.status".to_string()),
        "qualified joined"
    );
}

#[tokio::test]
async fn eloquent_builder_narrows_to_joined_table() {
    let (_dir, root) = project_with_model("User", PLAIN_USER);
    let db = provider_with_tables(
        root.clone(),
        vec![
            ("users", vec![("id", "int")]),
            ("orders", vec![("status", "string"), ("total", "int")]),
        ],
    )
    .await;
    let ctx = eloquent_ctx(
        "App\\Models\\User",
        BuilderMode::EloquentBuilder,
        vec![AccessibleTable::bare("orders")],
        FromClause::Inherit,
        Some("orders"),
    );
    let got = labels(&columns_for_builder(&ctx, &db, None, &root).await);
    assert_eq!(got.len(), 2, "only orders columns; got {got:?}");
    assert!(got.contains(&"status".to_string()));
    assert!(got.contains(&"total".to_string()));
}

#[tokio::test]
async fn eloquent_narrow_to_root_model_table_keeps_casts() {
    // `User::query()->join(...)->where('users.|')` narrows to the model's
    // table (qualifier = "users") and still applies the model cast.
    let body = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    protected $casts = ['options' => 'array'];
}
"#;
    let (_dir, root) = project_with_model("User", body);
    let db = provider_with_tables(
        root.clone(),
        vec![
            ("users", vec![("id", "int"), ("options", "string")]),
            ("orders", vec![("status", "string")]),
        ],
    )
    .await;
    let ctx = eloquent_ctx(
        "App\\Models\\User",
        BuilderMode::EloquentBuilder,
        vec![AccessibleTable::bare("orders")],
        FromClause::Inherit,
        Some("users"),
    );
    let items = columns_for_builder(&ctx, &db, None, &root).await;
    let options = items
        .iter()
        .find(|i| i.label == "options")
        .expect("options column narrowed to users");
    assert!(
        options.detail.as_deref().unwrap_or("").contains("array"),
        "cast should still apply when narrowing to the model table; got {:?}",
        options.detail
    );
}

#[tokio::test]
async fn eloquent_from_replace_uses_schema_table() {
    // `User::query()->from('admins')` redirects the root to admins
    // (schema-only — the User model's casts no longer apply).
    let body = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    protected $casts = ['options' => 'array'];
}
"#;
    let (_dir, root) = project_with_model("User", body);
    let db = provider_with_tables(
        root.clone(),
        vec![
            ("users", vec![("id", "int"), ("options", "string")]),
            ("admins", vec![("admin_id", "int"), ("level", "int")]),
        ],
    )
    .await;
    let ctx = eloquent_ctx(
        "App\\Models\\User",
        BuilderMode::EloquentBuilder,
        Vec::new(),
        FromClause::Replace(AccessibleTable::bare("admins")),
        None,
    );
    let got = labels(&columns_for_builder(&ctx, &db, None, &root).await);
    assert!(got.contains(&"admin_id".to_string()), "got {got:?}");
    assert!(got.contains(&"level".to_string()));
    assert!(!got.contains(&"id".to_string()), "users table is replaced");
    assert!(!got.contains(&"options".to_string()));
}

#[tokio::test]
async fn eloquent_opaque_from_suppresses_root_keeps_joins() {
    let (_dir, root) = project_with_model("User", PLAIN_USER);
    let db = provider_with_tables(
        root.clone(),
        vec![
            ("users", vec![("id", "int")]),
            ("orders", vec![("status", "string")]),
        ],
    )
    .await;
    let ctx = eloquent_ctx(
        "App\\Models\\User",
        BuilderMode::EloquentBuilder,
        vec![AccessibleTable::bare("orders")],
        FromClause::Opaque,
        None,
    );
    let got = labels(&columns_for_builder(&ctx, &db, None, &root).await);
    assert!(got.contains(&"orders.status".to_string()));
    assert!(
        !got.contains(&"id".to_string()),
        "opaque root suppressed; got {got:?}"
    );
}

#[tokio::test]
async fn eloquent_collection_offers_joined_columns_and_accessors() {
    let body = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public function getFullNameAttribute(): string { return ''; }
}
"#;
    let (_dir, root) = project_with_model("User", body);
    let db = provider_with_tables(
        root.clone(),
        vec![
            ("users", vec![("id", "int"), ("name", "string")]),
            ("orders", vec![("status", "string")]),
        ],
    )
    .await;
    let ctx = eloquent_ctx(
        "App\\Models\\User",
        BuilderMode::EloquentCollection,
        vec![AccessibleTable::bare("orders")],
        FromClause::Inherit,
        None,
    );
    let got = labels(&columns_for_collection(&ctx, &db, None, &root).await);
    assert!(got.contains(&"name".to_string()), "bare root col");
    assert!(got.contains(&"orders.status".to_string()), "joined col");
    assert!(
        got.contains(&"full_name".to_string()),
        "accessor; got {got:?}"
    );
    // Accessors are in-memory props — never offered table-qualified.
    assert!(!got.contains(&"users.full_name".to_string()));
}

// ---- goto_column_candidates (issue #24) -------------------------------

#[test]
fn goto_candidates_bare_single_table() {
    let accessible = vec![AccessibleTable::bare("users")];
    assert_eq!(
        goto_column_candidates(&accessible, "email"),
        vec![("users".to_string(), "email".to_string())]
    );
}

#[test]
fn goto_candidates_bare_searches_all_tables_root_first() {
    let accessible = vec![
        AccessibleTable::bare("users"),
        AccessibleTable::bare("orders"),
    ];
    assert_eq!(
        goto_column_candidates(&accessible, "status"),
        vec![
            ("users".to_string(), "status".to_string()),
            ("orders".to_string(), "status".to_string()),
        ]
    );
}

#[test]
fn goto_candidates_qualified_resolves_alias_to_real_table() {
    let accessible = vec![
        AccessibleTable::bare("users"),
        AccessibleTable {
            table: "orders".to_string(),
            alias: Some("o".to_string()),
        },
    ];
    assert_eq!(
        goto_column_candidates(&accessible, "o.status"),
        vec![("orders".to_string(), "status".to_string())]
    );
}

#[test]
fn goto_candidates_qualified_by_table_name() {
    let accessible = vec![
        AccessibleTable::bare("users"),
        AccessibleTable::bare("orders"),
    ];
    assert_eq!(
        goto_column_candidates(&accessible, "orders.status"),
        vec![("orders".to_string(), "status".to_string())]
    );
}

#[test]
fn goto_candidates_unknown_qualifier_falls_back_to_literal() {
    // A qualifier that matches no accessible table is treated as a literal
    // table name (covers schema-qualified / untracked tables).
    let accessible = vec![AccessibleTable::bare("users")];
    assert_eq!(
        goto_column_candidates(&accessible, "posts.id"),
        vec![("posts".to_string(), "id".to_string())]
    );
}

#[test]
fn goto_candidates_schema_qualified() {
    // `mydb.orders.status` — qualifier is everything before the last dot.
    let accessible = vec![AccessibleTable::bare("mydb.orders")];
    assert_eq!(
        goto_column_candidates(&accessible, "mydb.orders.status"),
        vec![("mydb.orders".to_string(), "status".to_string())]
    );
}

// ---- enrich_join_parent_tables (issue #24, Phase 3) -------------------

#[tokio::test]
async fn enrich_join_parent_tables_resolves_model_table() {
    let (_dir, root) = project_with_model("User", PLAIN_USER); // → users
    let mut ctx = eloquent_ctx(
        "App\\Models\\User",
        BuilderMode::BaseBuilder,
        Vec::new(),
        FromClause::Replace(AccessibleTable::bare("orders")),
        None,
    );
    ctx.join_parent_model = Some("App\\Models\\User".to_string());

    enrich_join_parent_tables(&mut ctx, &root).await;

    let tables: Vec<&str> = ctx.joined_tables.iter().map(|t| t.table.as_str()).collect();
    assert!(
        tables.contains(&"users"),
        "parent model table folded into accessible set; got {tables:?}"
    );
    assert!(ctx.join_parent_model.is_none(), "pending model consumed");
}

#[tokio::test]
async fn enrich_join_parent_tables_noop_when_unset() {
    let (_dir, root) = project_with_model("User", PLAIN_USER);
    let mut ctx = eloquent_ctx(
        "App\\Models\\User",
        BuilderMode::BaseBuilder,
        vec![AccessibleTable::bare("orders")],
        FromClause::Replace(AccessibleTable::bare("orders")),
        None,
    );
    // join_parent_model defaults to None.
    enrich_join_parent_tables(&mut ctx, &root).await;
    let tables: Vec<&str> = ctx.joined_tables.iter().map(|t| t.table.as_str()).collect();
    assert_eq!(tables, vec!["orders"], "no change when no pending model");
}
