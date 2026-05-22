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
