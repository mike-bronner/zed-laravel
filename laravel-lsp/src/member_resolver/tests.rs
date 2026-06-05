//! Tests for the magic-member classifier (`classify_member`).
//!
//! Each test builds a real model on disk (via `tempfile`), runs the existing
//! `chain::analyze` to get an inheritance-resolved `ClassView`, then asserts
//! the classification of a member access against it.

use super::*;
use crate::laravel_introspector::chain::analyze;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

/// Write a single model into a PSR-4 temp project and analyze it.
/// Returns the temp dir (keep alive) + the resolved `ClassView`.
fn model(model_php: &str) -> (TempDir, ClassView) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("app/Models/User.php");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, model_php).unwrap();
    fs::write(
        dir.path().join("composer.json"),
        r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#,
    )
    .unwrap();
    let view = analyze(&path, dir.path()).expect("should analyze model");
    (dir, view)
}

/// Write a model plus an extra PSR-4 file (e.g. a trait) and analyze the model.
fn model_with_extra(model_php: &str, extra_rel: &str, extra_php: &str) -> (TempDir, ClassView) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("app/Models/User.php");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, model_php).unwrap();
    let extra_path: PathBuf = dir.path().join(extra_rel);
    fs::create_dir_all(extra_path.parent().unwrap()).unwrap();
    fs::write(&extra_path, extra_php).unwrap();
    fs::write(
        dir.path().join("composer.json"),
        r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#,
    )
    .unwrap();
    let view = analyze(&path, dir.path()).expect("should analyze model");
    (dir, view)
}

#[test]
fn classifies_scope_as_call() {
    let (_d, view) = model(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Builder;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public function scopeActive(Builder $query): Builder { return $query; }
}
"#,
    );
    let c = classify_member(&view, "active", AccessForm::StaticCall).expect("scope");
    assert_eq!(c.kind, MagicMemberKind::Scope);
    assert_eq!(c.declaring_fqcn, "App\\Models\\User");

    // A scope is not reachable via property read.
    assert!(classify_member(&view, "active", AccessForm::Property).is_none());
}

#[test]
fn classifies_old_style_accessor_as_property() {
    let (_d, view) = model(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public function getFullNameAttribute(): string { return 'x'; }
}
"#,
    );
    let c = classify_member(&view, "full_name", AccessForm::Property).expect("accessor");
    assert_eq!(c.kind, MagicMemberKind::Accessor);
    assert_eq!(c.declaring_fqcn, "App\\Models\\User");
}

#[test]
fn classifies_relationship_both_forms() {
    let (_d, view) = model(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
use Illuminate\Database\Eloquent\Relations\HasMany;
class User extends Model {
    public function posts(): HasMany { return $this->hasMany(Post::class); }
}
"#,
    );
    let as_prop = classify_member(&view, "posts", AccessForm::Property).expect("rel prop");
    assert_eq!(as_prop.kind, MagicMemberKind::Relationship);

    let as_call = classify_member(&view, "posts", AccessForm::InstanceCall).expect("rel call");
    assert_eq!(as_call.kind, MagicMemberKind::Relationship);
    assert_eq!(as_call.declaring_fqcn, "App\\Models\\User");
}

#[test]
fn classifies_column_as_property() {
    let (_d, view) = model(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    protected $fillable = ['email'];
}
"#,
    );
    let c = classify_member(&view, "email", AccessForm::Property).expect("column");
    assert_eq!(c.kind, MagicMemberKind::Column);
    assert_eq!(c.declaring_fqcn, "App\\Models\\User");
}

#[test]
fn classifies_dynamic_finder_as_call() {
    let (_d, view) = model(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    protected $fillable = ['email_address'];
}
"#,
    );
    let c = classify_member(&view, "whereEmailAddress", AccessForm::StaticCall)
        .expect("dynamic finder");
    assert_eq!(c.kind, MagicMemberKind::DynamicFinder);
    assert_eq!(c.declaring_fqcn, "App\\Models\\User");
}

#[test]
fn dynamic_finder_requires_known_column() {
    let (_d, view) = model(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    protected $fillable = ['email'];
}
"#,
    );
    // `whereNonexistent` is not backed by a column → not a dynamic finder.
    assert!(classify_member(&view, "whereNonexistent", AccessForm::StaticCall).is_none());
    // `whereabouts` (lowercase remainder) must not be mistaken for a finder.
    assert!(classify_member(&view, "whereabouts", AccessForm::StaticCall).is_none());
}

#[test]
fn accessor_shadows_column_of_same_name() {
    let (_d, view) = model(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    protected $fillable = ['name'];
    public function getNameAttribute(): string { return 'x'; }
}
"#,
    );
    let c = classify_member(&view, "name", AccessForm::Property).expect("accessor wins");
    assert_eq!(
        c.kind,
        MagicMemberKind::Accessor,
        "an accessor must win over a raw column of the same name"
    );
}

#[test]
fn classifies_plain_method_and_property() {
    let (_d, view) = model(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public $nickname;
    public function doThing(): void {}
}
"#,
    );
    let method = classify_member(&view, "doThing", AccessForm::InstanceCall).expect("method");
    assert_eq!(method.kind, MagicMemberKind::PlainMember);

    let prop = classify_member(&view, "nickname", AccessForm::Property).expect("property");
    assert_eq!(prop.kind, MagicMemberKind::PlainMember);
}

#[test]
fn unknown_member_classifies_to_none() {
    let (_d, view) = model(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {}
"#,
    );
    assert!(classify_member(&view, "totallyUnknown", AccessForm::Property).is_none());
    assert!(classify_member(&view, "totallyUnknown", AccessForm::InstanceCall).is_none());
}

#[test]
fn trait_shared_scope_attributes_to_the_trait() {
    // The plan keys magic members by their *declaring* FQCN so a trait-shared
    // scope keys once. Here the scope is declared on a trait the model uses;
    // the declaring class must be the trait, not the model.
    let (_d, view) = model_with_extra(
        r#"<?php
namespace App\Models;
use App\Concerns\Activatable;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    use Activatable;
}
"#,
        "app/Concerns/Activatable.php",
        r#"<?php
namespace App\Concerns;
use Illuminate\Database\Eloquent\Builder;
trait Activatable {
    public function scopeActive(Builder $query): Builder { return $query; }
}
"#,
    );
    let c = classify_member(&view, "active", AccessForm::StaticCall).expect("trait scope");
    assert_eq!(c.kind, MagicMemberKind::Scope);
    assert_eq!(
        c.declaring_fqcn, "App\\Concerns\\Activatable",
        "a trait-shared scope must attribute to the trait, not the using model"
    );
}

#[test]
fn inherited_column_resolves_through_parent_model() {
    // A child model extending a base model inherits the base's columns.
    let (_d, view) = model_with_extra(
        r#"<?php
namespace App\Models;
class User extends BaseModel {
    protected $fillable = ['email'];
}
"#,
        "app/Models/BaseModel.php",
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class BaseModel extends Model {
    protected $fillable = ['uuid'];
}
"#,
    );
    // Own column.
    let own = classify_member(&view, "email", AccessForm::Property).expect("own column");
    assert_eq!(own.kind, MagicMemberKind::Column);
    // Inherited column from the parent.
    let inherited = classify_member(&view, "uuid", AccessForm::Property).expect("inherited column");
    assert_eq!(inherited.kind, MagicMemberKind::Column);
}
