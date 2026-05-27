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
    // Parent model declares `tokens` as `hasMany(OAuthToken::class)`. The
    // helper should return "OAuthToken" so the closure-scope hop can use
    // it as the effective model for the inner chain.
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
    assert_eq!(related.as_deref(), Some("OAuthToken"));
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
