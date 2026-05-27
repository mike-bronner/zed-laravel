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
