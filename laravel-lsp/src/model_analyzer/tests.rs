use super::*;

#[test]
fn test_extract_class_name() {
    let content = r#"
        class User extends Model
        {
        }
    "#;
    let metadata = ModelMetadata::from_content(content);
    assert_eq!(metadata.class_name, "User");
}

#[test]
fn test_extract_table_name() {
    let content = r#"
        class User extends Model
        {
            protected $table = 'app_users';
        }
    "#;
    let metadata = ModelMetadata::from_content(content);
    assert_eq!(metadata.table_name, Some("app_users".to_string()));
}

#[test]
fn test_extract_casts_property() {
    let content = r#"
        class User extends Model
        {
            protected $casts = [
                'email_verified_at' => 'datetime',
                'is_admin' => 'boolean',
                'settings' => 'array',
            ];
        }
    "#;
    let metadata = ModelMetadata::from_content(content);
    assert_eq!(
        metadata.casts.get("email_verified_at"),
        Some(&"datetime".to_string())
    );
    assert_eq!(metadata.casts.get("is_admin"), Some(&"boolean".to_string()));
    assert_eq!(metadata.casts.get("settings"), Some(&"array".to_string()));
}

#[test]
fn test_extract_casts_method() {
    let content = r#"
        class User extends Model
        {
            protected function casts(): array
            {
                return [
                    'email_verified_at' => 'datetime',
                    'password' => 'hashed',
                ];
            }
        }
    "#;
    let metadata = ModelMetadata::from_content(content);
    assert_eq!(
        metadata.casts.get("email_verified_at"),
        Some(&"datetime".to_string())
    );
    assert_eq!(metadata.casts.get("password"), Some(&"hashed".to_string()));
}

#[test]
fn test_extract_old_style_accessor() {
    let content = r#"
        class User extends Model
        {
            public function getFullNameAttribute(): string
            {
                return $this->first_name . ' ' . $this->last_name;
            }
        }
    "#;
    let metadata = ModelMetadata::from_content(content);
    assert_eq!(metadata.accessors.len(), 1);
    assert_eq!(metadata.accessors[0].property_name, "full_name");
    assert_eq!(
        metadata.accessors[0].return_type,
        Some("string".to_string())
    );
}

#[test]
fn test_extract_new_style_accessor() {
    let content = r#"
        class User extends Model
        {
            protected function firstName(): Attribute
            {
                return Attribute::make(
                    get: fn (string $value) => ucfirst($value),
                );
            }
        }
    "#;
    let metadata = ModelMetadata::from_content(content);
    assert_eq!(metadata.accessors.len(), 1);
    assert_eq!(metadata.accessors[0].property_name, "first_name");
    assert!(metadata.accessors[0].is_attribute_style);
}

#[test]
fn test_extract_relationships() {
    let content = r#"
        class User extends Model
        {
            public function posts(): HasMany
            {
                return $this->hasMany(Post::class);
            }

            public function profile(): HasOne
            {
                return $this->hasOne(Profile::class);
            }

            public function roles(): BelongsToMany
            {
                return $this->belongsToMany(Role::class);
            }
        }
    "#;
    let metadata = ModelMetadata::from_content(content);
    assert_eq!(metadata.relationships.len(), 3);

    let posts = metadata
        .relationships
        .iter()
        .find(|r| r.method_name == "posts")
        .unwrap();
    assert_eq!(posts.relationship_type, "hasMany");
    assert_eq!(posts.related_model, Some("Post".to_string()));

    let profile = metadata
        .relationships
        .iter()
        .find(|r| r.method_name == "profile")
        .unwrap();
    assert_eq!(profile.relationship_type, "hasOne");
    assert_eq!(profile.related_model, Some("Profile".to_string()));

    let roles = metadata
        .relationships
        .iter()
        .find(|r| r.method_name == "roles")
        .unwrap();
    assert_eq!(roles.relationship_type, "belongsToMany");
    assert_eq!(roles.related_model, Some("Role".to_string()));
}

#[test]
fn test_pascal_to_snake() {
    assert_eq!(ModelMetadata::pascal_to_snake("FirstName"), "first_name");
    assert_eq!(
        ModelMetadata::pascal_to_snake("EmailVerifiedAt"),
        "email_verified_at"
    );
    assert_eq!(ModelMetadata::pascal_to_snake("ID"), "i_d");
    assert_eq!(ModelMetadata::pascal_to_snake("Name"), "name");
}

#[test]
fn test_map_cast_to_php_type() {
    assert_eq!(map_cast_to_php_type("datetime"), "Carbon");
    assert_eq!(map_cast_to_php_type("boolean"), "bool");
    assert_eq!(map_cast_to_php_type("array"), "array");
    assert_eq!(map_cast_to_php_type("integer"), "int");
    assert_eq!(map_cast_to_php_type("float"), "float");
    assert_eq!(map_cast_to_php_type("CustomCast"), "CustomCast");
}

#[test]
fn test_relationship_to_php_type() {
    assert_eq!(
        relationship_to_php_type("hasOne", Some("Profile")),
        "?Profile"
    );
    assert_eq!(relationship_to_php_type("belongsTo", Some("User")), "?User");
    assert_eq!(
        relationship_to_php_type("hasMany", Some("Post")),
        "Collection<Post>"
    );
    assert_eq!(
        relationship_to_php_type("belongsToMany", Some("Role")),
        "Collection<Role>"
    );
}

// ---- Inheritance walking (Phase 5.6) -------------------------------------

use tempfile::TempDir;

/// Helper: spin up a tempdir Laravel-shaped project with multiple model
/// files at `app/Models/`. Returns the project root.
fn project_with_models(files: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let models_dir = dir.path().join("app").join("Models");
    std::fs::create_dir_all(&models_dir).expect("create models dir");
    for (class_name, body) in files {
        let path = models_dir.join(format!("{class_name}.php"));
        std::fs::write(&path, body).expect("write model");
    }
    dir
}

#[test]
fn inheritance_child_without_table_picks_up_parent_table() {
    // Mike's exact case: OAuthAccessToken extends Token; Token declares
    // `protected $table = 'oauth_access_tokens'`. The child should
    // resolve to that table, not snake_pluralize('OAuthAccessToken').
    let dir = project_with_models(&[
        (
            "OAuthAccessToken",
            r#"<?php
namespace App\Models;
class OAuthAccessToken extends Token {}
"#,
        ),
        (
            "Token",
            r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Token extends Model {
    protected $table = 'oauth_access_tokens';
}
"#,
        ),
    ]);
    let child_path = dir.path().join("app/Models/OAuthAccessToken.php");
    let metadata = ModelMetadata::from_file_with_inheritance(&child_path, dir.path())
        .expect("metadata resolves");
    assert_eq!(metadata.class_name, "OAuthAccessToken");
    assert_eq!(metadata.table_name.as_deref(), Some("oauth_access_tokens"));
}

#[test]
fn inheritance_child_table_overrides_parent_table() {
    // PHP semantics: when child declares its own $table, that wins.
    let dir = project_with_models(&[
        (
            "Special",
            r#"<?php
namespace App\Models;
class Special extends Base {
    protected $table = 'special_table';
}
"#,
        ),
        (
            "Base",
            r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Base extends Model {
    protected $table = 'base_table';
}
"#,
        ),
    ]);
    let child_path = dir.path().join("app/Models/Special.php");
    let metadata = ModelMetadata::from_file_with_inheritance(&child_path, dir.path()).unwrap();
    assert_eq!(metadata.table_name.as_deref(), Some("special_table"));
}

#[test]
fn inheritance_merges_casts_and_relationships() {
    // The parent declares a relationship and a cast; the child adds its
    // own. Both surface in the resolved metadata.
    let dir = project_with_models(&[
        (
            "Child",
            r#"<?php
namespace App\Models;
class Child extends Parent_ {
    protected $casts = ['child_field' => 'array'];
    public function profile() {
        return $this->hasOne(Profile::class);
    }
}
"#,
        ),
        (
            "Parent_",
            r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Parent_ extends Model {
    protected $table = 'parent_table';
    protected $casts = ['parent_field' => 'boolean'];
    public function inherited_rel() {
        return $this->hasMany(Other::class);
    }
}
"#,
        ),
    ]);
    let child_path = dir.path().join("app/Models/Child.php");
    let metadata = ModelMetadata::from_file_with_inheritance(&child_path, dir.path()).unwrap();
    // Table comes from parent (child has none).
    assert_eq!(metadata.table_name.as_deref(), Some("parent_table"));
    // Both casts present.
    assert_eq!(
        metadata.casts.get("child_field").map(|s| s.as_str()),
        Some("array")
    );
    assert_eq!(
        metadata.casts.get("parent_field").map(|s| s.as_str()),
        Some("boolean")
    );
    // Both relationships present (child + inherited from parent).
    let names: Vec<&str> = metadata
        .relationships
        .iter()
        .map(|r| r.method_name.as_str())
        .collect();
    assert!(
        names.contains(&"profile"),
        "child relation missing; got {names:?}"
    );
    assert!(
        names.contains(&"inherited_rel"),
        "parent relation not inherited; got {names:?}"
    );
}

#[test]
fn inheritance_stops_at_eloquent_model_base() {
    // A class that extends `Model` directly should not try to walk
    // further (there's no Model.php in the project — that's vendor).
    let dir = project_with_models(&[(
        "Plain",
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Plain extends Model {}
"#,
    )]);
    let path = dir.path().join("app/Models/Plain.php");
    // Should resolve without erroring even though there's no parent file.
    let metadata = ModelMetadata::from_file_with_inheritance(&path, dir.path()).unwrap();
    assert_eq!(metadata.class_name, "Plain");
    assert!(metadata.table_name.is_none());
}

#[test]
fn inheritance_handles_missing_parent_gracefully() {
    // Parent class file doesn't exist (could be a vendor class we don't
    // search). The child's own metadata still resolves — we just don't
    // inherit anything.
    let dir = project_with_models(&[(
        "Orphan",
        r#"<?php
namespace App\Models;
class Orphan extends SomeVendorClass {
    protected $table = 'orphans';
}
"#,
    )]);
    let path = dir.path().join("app/Models/Orphan.php");
    let metadata = ModelMetadata::from_file_with_inheritance(&path, dir.path()).unwrap();
    assert_eq!(metadata.table_name.as_deref(), Some("orphans"));
}

#[test]
fn inheritance_survives_cycles() {
    // A extends B, B extends A — invalid PHP, but the walker shouldn't
    // recurse forever. Visited-set short-circuits the second visit.
    let dir = project_with_models(&[
        (
            "A",
            r#"<?php
namespace App\Models;
class A extends B {}
"#,
        ),
        (
            "B",
            r#"<?php
namespace App\Models;
class B extends A {}
"#,
        ),
    ]);
    let path = dir.path().join("app/Models/A.php");
    // Either returns Some with whatever it could collect, or None — but
    // critically it MUST NOT hang or stack-overflow.
    let _ = ModelMetadata::from_file_with_inheritance(&path, dir.path());
}

#[test]
fn inheritance_walks_grandparent() {
    // Multi-level chain: Child → Parent → Grandparent (which has the
    // $table). All three levels walked.
    let dir = project_with_models(&[
        (
            "Grandchild",
            r#"<?php
namespace App\Models;
class Grandchild extends Middle {}
"#,
        ),
        (
            "Middle",
            r#"<?php
namespace App\Models;
class Middle extends Grandparent {}
"#,
        ),
        (
            "Grandparent",
            r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Grandparent extends Model {
    protected $table = 'grandparent_table';
}
"#,
        ),
    ]);
    let path = dir.path().join("app/Models/Grandchild.php");
    let metadata = ModelMetadata::from_file_with_inheritance(&path, dir.path()).unwrap();
    assert_eq!(metadata.table_name.as_deref(), Some("grandparent_table"));
}
