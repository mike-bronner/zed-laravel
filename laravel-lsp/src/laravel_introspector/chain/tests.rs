use super::*;
use std::fs;
use tempfile::TempDir;

/// Drop a model + composer.json into a temp dir's PSR-4 layout. Returns
/// (temp_dir, path_to_Portfolio.php). Keeps the temp dir alive for the
/// caller — drop it and the files disappear.
fn fixture(model_php: &str) -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("app/Models/Portfolio.php");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, model_php).unwrap();
    let composer = r#"{
        "autoload": { "psr-4": { "App\\": "app/" } }
    }"#;
    fs::write(dir.path().join("composer.json"), composer).unwrap();
    (dir, path)
}

// ---- Basic shape -------------------------------------------------------

#[test]
fn analyzes_simple_model() {
    let (dir, path) = fixture(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Portfolio extends Model {}
"#,
    );
    let view = analyze(&path, dir.path()).expect("should analyze");
    assert_eq!(view.fqcn, "App\\Models\\Portfolio");
    assert_eq!(view.class_name, "Portfolio");
    assert_eq!(view.namespace.as_deref(), Some("App\\Models"));
    assert_eq!(view.kind, LaravelClassKind::Model);
}

#[test]
fn classifies_non_model_as_other() {
    let (dir, path) = fixture(
        r#"<?php
namespace App\Models;
class JustAClass {}
"#,
    );
    let view = analyze(&path, dir.path()).expect("should analyze");
    assert_eq!(view.kind, LaravelClassKind::Other);
    assert!(view.scopes.is_empty());
}

// ---- Scopes (both patterns) --------------------------------------------

#[test]
fn detects_prefix_style_scope() {
    let (dir, path) = fixture(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Builder;
use Illuminate\Database\Eloquent\Model;
class Portfolio extends Model {
    public function scopeActive(Builder $query): Builder {
        return $query->where('status', 'active');
    }
}
"#,
    );
    let view = analyze(&path, dir.path()).unwrap();
    let active = view.scopes.iter().find(|s| s.name == "active").unwrap();
    assert_eq!(active.style, ScopeStyle::Prefix);
    assert_eq!(active.source_class, "App\\Models\\Portfolio");
    assert!(active.signature.contains("function active("));
    assert!(!active.signature.contains("scopeActive"));
}

#[test]
fn detects_attribute_style_scope() {
    let (dir, path) = fixture(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Attributes\Scope;
use Illuminate\Database\Eloquent\Builder;
use Illuminate\Database\Eloquent\Model;
class Portfolio extends Model {
    #[Scope]
    public function published(Builder $query): Builder {
        return $query->whereNotNull('published_at');
    }
}
"#,
    );
    let view = analyze(&path, dir.path()).unwrap();
    let published = view.scopes.iter().find(|s| s.name == "published").unwrap();
    assert_eq!(published.style, ScopeStyle::Attribute);
}

#[test]
fn detects_attribute_style_scope_with_fqcn() {
    let (dir, path) = fixture(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Builder;
use Illuminate\Database\Eloquent\Model;
class Portfolio extends Model {
    #[\Illuminate\Database\Eloquent\Attributes\Scope]
    public function archived(Builder $query): Builder { return $query; }
}
"#,
    );
    let view = analyze(&path, dir.path()).unwrap();
    assert!(view
        .scopes
        .iter()
        .any(|s| s.name == "archived" && s.style == ScopeStyle::Attribute));
}

#[test]
fn skips_non_scope_methods() {
    let (dir, path) = fixture(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Builder;
use Illuminate\Database\Eloquent\Model;
class Portfolio extends Model {
    public function scope() {}      // bare "scope" — not a scope
    public function scoped() {}     // "scoped" — not a scope
    public function regular() {}    // no prefix, no attribute — not a scope
    protected function scopeHidden(Builder $q) {} // non-public — not a scope
}
"#,
    );
    let view = analyze(&path, dir.path()).unwrap();
    assert!(
        view.scopes.is_empty(),
        "unexpected scopes: {:?}",
        view.scopes
    );
}

// ---- Inheritance + traits ----------------------------------------------

#[test]
fn picks_up_scopes_from_traits() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("composer.json"),
        r#"{"autoload":{"psr-4":{"App\\":"app/"}}}"#,
    )
    .unwrap();
    let trait_path = dir.path().join("app/Traits/HasArchive.php");
    fs::create_dir_all(trait_path.parent().unwrap()).unwrap();
    fs::write(
        &trait_path,
        r#"<?php
namespace App\Traits;
use Illuminate\Database\Eloquent\Builder;
trait HasArchive {
    public function scopeArchived(Builder $query): Builder { return $query; }
}
"#,
    )
    .unwrap();
    let model_path = dir.path().join("app/Models/Portfolio.php");
    fs::create_dir_all(model_path.parent().unwrap()).unwrap();
    fs::write(
        &model_path,
        r#"<?php
namespace App\Models;
use App\Traits\HasArchive;
use Illuminate\Database\Eloquent\Model;
class Portfolio extends Model {
    use HasArchive;
}
"#,
    )
    .unwrap();

    let view = analyze(&model_path, dir.path()).unwrap();
    let archived = view
        .scopes
        .iter()
        .find(|s| s.name == "archived")
        .expect("trait scope should surface");
    assert!(archived.source_class.ends_with("HasArchive"));
}

// ---- Casts / accessors / relationships / table -------------------------

#[test]
fn extracts_casts_from_property() {
    let (dir, path) = fixture(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Portfolio extends Model {
    protected $casts = [
        'options' => 'array',
        'published_at' => 'datetime',
    ];
}
"#,
    );
    let view = analyze(&path, dir.path()).unwrap();
    assert_eq!(view.casts.get("options").map(String::as_str), Some("array"));
    assert_eq!(
        view.casts.get("published_at").map(String::as_str),
        Some("datetime")
    );
}

#[test]
fn extracts_table_name() {
    let (dir, path) = fixture(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Portfolio extends Model {
    protected $table = 'user_portfolios';
}
"#,
    );
    let view = analyze(&path, dir.path()).unwrap();
    assert_eq!(view.table_name.as_deref(), Some("user_portfolios"));
}

#[test]
fn detects_relationships() {
    let (dir, path) = fixture(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
use Illuminate\Database\Eloquent\Relations\HasMany;
class Portfolio extends Model {
    public function items(): HasMany {
        return $this->hasMany(Item::class);
    }
    public function owner() {
        return $this->belongsTo(User::class);
    }
}
"#,
    );
    let view = analyze(&path, dir.path()).unwrap();
    let items = view
        .relationships
        .iter()
        .find(|r| r.method_name == "items")
        .unwrap();
    assert_eq!(items.relationship_type, "hasMany");
    let owner = view
        .relationships
        .iter()
        .find(|r| r.method_name == "owner")
        .unwrap();
    assert_eq!(owner.relationship_type, "belongsTo");
}

#[test]
fn detects_old_style_accessor() {
    let (dir, path) = fixture(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Portfolio extends Model {
    public function getFullNameAttribute(): string { return ''; }
}
"#,
    );
    let view = analyze(&path, dir.path()).unwrap();
    let acc = view
        .accessors
        .iter()
        .find(|a| a.property_name == "full_name")
        .unwrap();
    assert!(!acc.is_attribute_style);
    assert_eq!(acc.return_type.as_deref(), Some("string"));
}

#[test]
fn detects_new_style_accessor() {
    let (dir, path) = fixture(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Casts\Attribute;
use Illuminate\Database\Eloquent\Model;
class Portfolio extends Model {
    public function firstName(): Attribute {
        return Attribute::make(get: fn ($value) => ucfirst($value));
    }
}
"#,
    );
    let view = analyze(&path, dir.path()).unwrap();
    let acc = view
        .accessors
        .iter()
        .find(|a| a.property_name == "first_name")
        .unwrap();
    assert!(acc.is_attribute_style);
}

// ---- Builder-classified files have callstatic_surface ------------------

#[test]
fn builder_files_populate_callstatic_surface_not_scopes() {
    // For a file representing Eloquent\Builder itself, we don't compute
    // scopes — that's a model-only concept. Instead callstatic_surface
    // is populated.
    let dir = TempDir::new().unwrap();
    let path = dir
        .path()
        .join("vendor/laravel/framework/src/Illuminate/Database/Eloquent/Builder.php");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        r#"<?php
namespace Illuminate\Database\Eloquent;
class Builder {
    /**
     * Add a basic where clause.
     * @return $this
     */
    public function where($column, $operator = null) { return $this; }
}
"#,
    )
    .unwrap();

    let view = analyze(&path, dir.path()).unwrap();
    assert_eq!(view.kind, LaravelClassKind::EloquentBuilder);
    assert!(view.scopes.is_empty());
    let where_method = view
        .callstatic_surface
        .iter()
        .find(|m| m.name == "where")
        .expect("where should be in callstatic surface");
    assert_eq!(where_method.return_type.as_deref(), Some("Builder<static>"));
}

// ---- Column-surface computation (Phase 3, source-derived path) ---------

/// Helper: turn a column_surface into a `name → (php_type, source)`
/// map so individual assertions stay readable.
fn cols(view: &ClassView) -> std::collections::HashMap<String, (String, ColumnSource)> {
    view.column_surface
        .iter()
        .map(|c| (c.name.clone(), (c.php_type.clone(), c.source)))
        .collect()
}

#[test]
fn conventional_columns_present_on_bare_model() {
    let (dir, path) = fixture(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Portfolio extends Model {}
"#,
    );
    let view = analyze(&path, dir.path()).unwrap();
    let map = cols(&view);
    assert_eq!(
        map.get("id"),
        Some(&("int".to_string(), ColumnSource::Convention))
    );
    assert_eq!(
        map.get("created_at"),
        Some(&("Carbon".to_string(), ColumnSource::Convention))
    );
    assert_eq!(
        map.get("updated_at"),
        Some(&("Carbon".to_string(), ColumnSource::Convention))
    );
}

#[test]
fn timestamps_false_suppresses_created_and_updated_at() {
    let (dir, path) = fixture(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Portfolio extends Model {
    public $timestamps = false;
}
"#,
    );
    let view = analyze(&path, dir.path()).unwrap();
    let names: Vec<&str> = view
        .column_surface
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert!(!names.contains(&"created_at"), "got: {names:?}");
    assert!(!names.contains(&"updated_at"), "got: {names:?}");
    // `id` convention still fires.
    assert!(names.contains(&"id"));
}

#[test]
fn fillable_columns_pick_up_conventional_types() {
    let (dir, path) = fixture(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Portfolio extends Model {
    protected $fillable = ['name', 'user_id', 'published_at', 'description'];
}
"#,
    );
    let view = analyze(&path, dir.path()).unwrap();
    let map = cols(&view);
    assert_eq!(
        map.get("name"),
        Some(&("mixed".to_string(), ColumnSource::Fillable))
    );
    assert_eq!(
        map.get("user_id"),
        Some(&("int".to_string(), ColumnSource::Fillable))
    );
    assert_eq!(
        map.get("published_at"),
        Some(&("Carbon".to_string(), ColumnSource::Fillable))
    );
    assert_eq!(
        map.get("description"),
        Some(&("mixed".to_string(), ColumnSource::Fillable))
    );
}

#[test]
fn casts_beat_fillable_on_name_collision() {
    // `options` appears in both — the cast's explicit `array` type should
    // win over the fillable's conventional `mixed`. This is the
    // motivating case for cast-first iteration in compute_column_surface.
    let (dir, path) = fixture(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Portfolio extends Model {
    protected $fillable = ['options', 'name'];
    protected $casts = ['options' => 'array'];
}
"#,
    );
    let view = analyze(&path, dir.path()).unwrap();
    let map = cols(&view);
    assert_eq!(
        map.get("options"),
        Some(&("array".to_string(), ColumnSource::Cast)),
        "cast should win over fillable for `options`"
    );
    // `name` only in fillable.
    assert_eq!(
        map.get("name"),
        Some(&("mixed".to_string(), ColumnSource::Fillable))
    );
}

#[test]
fn legacy_dates_array_adds_carbon_columns() {
    let (dir, path) = fixture(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Portfolio extends Model {
    protected $dates = ['scheduled_at', 'published_on'];
}
"#,
    );
    let view = analyze(&path, dir.path()).unwrap();
    let map = cols(&view);
    assert_eq!(
        map.get("scheduled_at"),
        Some(&("Carbon".to_string(), ColumnSource::Dates))
    );
    assert_eq!(
        map.get("published_on"),
        Some(&("Carbon".to_string(), ColumnSource::Dates))
    );
}

#[test]
fn attributes_array_keys_become_columns() {
    let (dir, path) = fixture(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Portfolio extends Model {
    protected $attributes = [
        'status' => 'draft',
        'is_featured' => false,
    ];
}
"#,
    );
    let view = analyze(&path, dir.path()).unwrap();
    let map = cols(&view);
    assert_eq!(
        map.get("status"),
        Some(&("mixed".to_string(), ColumnSource::Attributes))
    );
    assert_eq!(
        map.get("is_featured"),
        Some(&("mixed".to_string(), ColumnSource::Attributes))
    );
}

#[test]
fn soft_deletes_trait_adds_deleted_at() {
    // Stand up a minimal SoftDeletes trait file in the vendor PSR-4 path
    // so the walker can compose it in. The trait body doesn't have to be
    // real — just needs the right FQCN so chain_contains fires.
    let dir = TempDir::new().unwrap();
    let model_path = dir.path().join("app/Models/Portfolio.php");
    fs::create_dir_all(model_path.parent().unwrap()).unwrap();
    fs::write(
        &model_path,
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
use Illuminate\Database\Eloquent\SoftDeletes;
class Portfolio extends Model {
    use SoftDeletes;
}
"#,
    )
    .unwrap();

    // class_locator's vendor heuristic looks at
    // `vendor/<vendor>/<package>/src/<rest>/<Class>.php` where vendor +
    // package come from the first two FQCN segments lowercased. For
    // `Illuminate\Database\Eloquent\SoftDeletes` that resolves to
    // `vendor/illuminate/database/src/Eloquent/SoftDeletes.php`. The
    // real Laravel monorepo lives at `vendor/laravel/framework/...`
    // which only resolves via Composer's installed.json — out of
    // scope for these unit tests, so we use the heuristic-friendly
    // layout instead.
    let trait_path = dir
        .path()
        .join("vendor/illuminate/database/src/Eloquent/SoftDeletes.php");
    fs::create_dir_all(trait_path.parent().unwrap()).unwrap();
    fs::write(
        &trait_path,
        r#"<?php
namespace Illuminate\Database\Eloquent;
trait SoftDeletes {
    public function trashed() { return false; }
}
"#,
    )
    .unwrap();

    let composer = r#"{
        "autoload": { "psr-4": { "App\\": "app/" } }
    }"#;
    fs::write(dir.path().join("composer.json"), composer).unwrap();

    let view = analyze(&model_path, dir.path()).unwrap();
    let map = cols(&view);
    assert_eq!(
        map.get("deleted_at"),
        Some(&("Carbon".to_string(), ColumnSource::Trait)),
        "SoftDeletes should add deleted_at; got cols: {:?}",
        view.column_surface
    );
}

#[test]
fn authenticatable_parent_adds_canonical_auth_columns() {
    // Stand up Foundation\Auth\User so the walker can compose it in via
    // `extends Authenticatable`. The body of Authenticatable doesn't
    // matter; only the FQCN provenance on inherited members does.
    let dir = TempDir::new().unwrap();
    let model_path = dir.path().join("app/Models/User.php");
    fs::create_dir_all(model_path.parent().unwrap()).unwrap();
    fs::write(
        &model_path,
        r#"<?php
namespace App\Models;
use Illuminate\Foundation\Auth\User as Authenticatable;
class User extends Authenticatable {}
"#,
    )
    .unwrap();

    // Heuristic-friendly layout — see SoftDeletes test comment for why
    // we don't use the real laravel/framework monorepo path.
    let auth_path = dir
        .path()
        .join("vendor/illuminate/foundation/src/Auth/User.php");
    fs::create_dir_all(auth_path.parent().unwrap()).unwrap();
    fs::write(
        &auth_path,
        r#"<?php
namespace Illuminate\Foundation\Auth;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public function whoami() { return static::class; }
}
"#,
    )
    .unwrap();

    let composer = r#"{
        "autoload": { "psr-4": { "App\\": "app/" } }
    }"#;
    fs::write(dir.path().join("composer.json"), composer).unwrap();

    let view = analyze(&model_path, dir.path()).unwrap();
    let map = cols(&view);
    for (col, ty) in [
        ("email", "string"),
        ("email_verified_at", "Carbon"),
        ("password", "string"),
        ("remember_token", "string"),
        ("name", "string"),
    ] {
        assert_eq!(
            map.get(col),
            Some(&(ty.to_string(), ColumnSource::ParentClass)),
            "{col} should come from Authenticatable parent; got cols: {:?}",
            view.column_surface
        );
    }
}

#[test]
fn analyze_content_also_populates_column_surface() {
    // Single-file path (no inheritance walk) should still pull in
    // $fillable + casts + conventions. SoftDeletes / Authenticatable
    // can't be detected here (no parent walk), which matches the
    // documented "limited" behaviour of analyze_content.
    let view = analyze_content(
        r#"
namespace App\Models;
class Portfolio {
    protected $fillable = ['name'];
    protected $casts = ['meta' => 'json'];
}
"#,
    )
    .unwrap();
    let map = cols(&view);
    assert_eq!(
        map.get("name"),
        Some(&("mixed".to_string(), ColumnSource::Fillable))
    );
    assert_eq!(
        map.get("meta"),
        Some(&("array".to_string(), ColumnSource::Cast))
    );
    assert!(map.contains_key("id"));
    assert!(map.contains_key("created_at"));
}
