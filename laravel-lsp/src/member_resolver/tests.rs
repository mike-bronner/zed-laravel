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

// ─── Orchestration: resolve_and_classify (M3) ────────────────────────────

use crate::class_hierarchy_index::{classes_in_file, ClassHierarchyIndex};
use crate::parser::parse_php;
use crate::query_chain::use_aliases::extract_use_aliases;

/// A temp project: a model file indexed in a `ClassHierarchyIndex`, plus the
/// project root for `analyze`. Caller source is parsed per-test.
struct Project {
    _dir: TempDir,
    index: ClassHierarchyIndex,
    root: PathBuf,
}

/// Build a project with a model at `model_rel` and index it.
fn project(model_rel: &str, model_php: &str) -> Project {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let model_path = root.join(model_rel);
    fs::create_dir_all(model_path.parent().unwrap()).unwrap();
    fs::write(&model_path, model_php).unwrap();
    fs::write(
        root.join("composer.json"),
        r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#,
    )
    .unwrap();
    let mut index = ClassHierarchyIndex::default();
    index.insert_file(&model_path, classes_in_file(&model_path, model_php));
    Project {
        _dir: dir,
        index,
        root,
    }
}

/// Find the receiver (object) node of the first `$x->{member}` access.
fn receiver_of<'t>(
    tree: &'t tree_sitter::Tree,
    bytes: &[u8],
    member: &str,
) -> Option<tree_sitter::Node<'t>> {
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if matches!(
            n.kind(),
            "member_access_expression" | "nullsafe_member_access_expression"
        ) {
            if let Some(name) = n.child_by_field_name("name") {
                if name.utf8_text(bytes).ok() == Some(member) {
                    return n.child_by_field_name("object");
                }
            }
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    None
}

const USER_MODEL: &str = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
use Illuminate\Database\Eloquent\Relations\HasMany;
class User extends Model {
    protected $fillable = ['email'];
    public function posts(): HasMany { return $this->hasMany(Post::class); }
}
"#;

/// Resolve `$x->{member}` in `caller` against project `p`.
fn resolve_in(p: &Project, caller: &str, member: &str) -> Option<ResolvedMemberAccess> {
    let tree = parse_php(caller).expect("parse caller");
    let bytes = caller.as_bytes();
    let aliases = extract_use_aliases(&tree, caller);
    let receiver = receiver_of(&tree, bytes, member)?;
    let mut cache = ClassViewCache::new();
    resolve_and_classify(
        receiver,
        member,
        AccessForm::Property,
        bytes,
        &aliases,
        &p.index,
        &mut cache,
        &p.root,
    )
}

#[test]
fn resolves_typed_param_property_to_column_high() {
    let p = project("app/Models/User.php", USER_MODEL);
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function show(User $user) {
        return $user->email;
    }
}
"#;
    let r = resolve_in(&p, caller, "email").expect("resolves");
    assert_eq!(r.kind, MagicMemberKind::Column);
    assert_eq!(r.confidence, Confidence::High);
    assert_eq!(r.declaring_fqcn, "App\\Models\\User");
}

#[test]
fn resolves_typed_param_relationship() {
    let p = project("app/Models/User.php", USER_MODEL);
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function show(User $user) {
        return $user->posts;
    }
}
"#;
    let r = resolve_in(&p, caller, "posts").expect("resolves");
    assert_eq!(r.kind, MagicMemberKind::Relationship);
}

#[test]
fn resolves_this_to_enclosing_class() {
    // `$this->email` inside a User method resolves to the User model.
    let p = project("app/Models/User.php", USER_MODEL);
    let caller = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    protected $fillable = ['email'];
    public function greeting() {
        return $this->email;
    }
}
"#;
    let r = resolve_in(&p, caller, "email").expect("resolves $this");
    assert_eq!(r.kind, MagicMemberKind::Column);
    assert_eq!(r.confidence, Confidence::High);
    assert_eq!(r.declaring_fqcn, "App\\Models\\User");
}

#[test]
fn multi_hop_receiver_lowers_confidence() {
    let p = project("app/Models/User.php", USER_MODEL);
    // `$q` is seeded one hop from the typed `$user` → MEDIUM.
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function show(User $user) {
        $q = $user->newQuery();
        return $q->email;
    }
}
"#;
    let r = resolve_in(&p, caller, "email").expect("resolves multi-hop");
    assert_eq!(r.kind, MagicMemberKind::Column);
    assert_eq!(r.confidence, Confidence::Medium);
}

#[test]
fn unresolvable_receiver_yields_none() {
    let p = project("app/Models/User.php", USER_MODEL);
    // `$mystery` has no type info anywhere.
    let caller = r#"<?php
function show($mystery) {
    return $mystery->email;
}
"#;
    assert!(resolve_in(&p, caller, "email").is_none());
}

#[test]
fn receiver_class_absent_from_index_yields_none() {
    // Empty index — even a perfectly typed receiver can't be classified.
    let p = Project {
        _dir: TempDir::new().unwrap(),
        index: ClassHierarchyIndex::default(),
        root: PathBuf::from("/nonexistent"),
    };
    let caller = r#"<?php
use App\Models\User;
function show(User $user) {
    return $user->email;
}
"#;
    assert!(resolve_in(&p, caller, "email").is_none());
}

#[test]
fn classview_cache_reuses_built_view() {
    // Two resolutions against the same FQCN must reuse one ClassView build.
    let p = project("app/Models/User.php", USER_MODEL);
    let mut cache = ClassViewCache::new();
    let node = p.index.get("App\\Models\\User").expect("indexed");
    let v1 = cache.get_or_build("App\\Models\\User", &node.file_path, &p.root);
    let v2 = cache.get_or_build("App\\Models\\User", &node.file_path, &p.root);
    assert!(v1.is_some());
    assert!(std::sync::Arc::ptr_eq(
        v1.as_ref().unwrap(),
        v2.as_ref().unwrap()
    ));
}

// ─── Widening: typed properties ($this->prop) ────────────────────────────

/// Build a project from several PSR-4 files, indexing every one.
fn project_files(files: &[(&str, &str)]) -> Project {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let mut index = ClassHierarchyIndex::default();
    for (rel, src) in files {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, src).unwrap();
        index.insert_file(&path, classes_in_file(&path, src));
    }
    fs::write(
        root.join("composer.json"),
        r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#,
    )
    .unwrap();
    Project {
        _dir: dir,
        index,
        root,
    }
}

const PROFILE_MODEL: &str = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Profile extends Model {
    protected $fillable = ['bio'];
}
"#;

#[test]
fn widens_typed_property_this_prop() {
    let user = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    private Profile $profile;
    public function bio() {
        return $this->profile->bio;
    }
}
"#;
    let p = project_files(&[
        ("app/Models/User.php", user),
        ("app/Models/Profile.php", PROFILE_MODEL),
    ]);
    // Receiver of `bio` is `$this->profile`.
    let r = resolve_in(&p, user, "bio").expect("typed property resolves");
    assert_eq!(r.kind, MagicMemberKind::Column);
    assert_eq!(r.confidence, Confidence::High);
    assert_eq!(r.declaring_fqcn, "App\\Models\\Profile");
}

#[test]
fn widens_nullable_typed_property() {
    let user = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    protected ?Profile $profile = null;
    public function bio() {
        return $this->profile->bio;
    }
}
"#;
    let p = project_files(&[
        ("app/Models/User.php", user),
        ("app/Models/Profile.php", PROFILE_MODEL),
    ]);
    let r = resolve_in(&p, user, "bio").expect("nullable typed property resolves");
    assert_eq!(r.kind, MagicMemberKind::Column);
    assert_eq!(r.declaring_fqcn, "App\\Models\\Profile");
}

#[test]
fn widens_promoted_constructor_property() {
    let user = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public function __construct(private Profile $profile) {}
    public function bio() {
        return $this->profile->bio;
    }
}
"#;
    let p = project_files(&[
        ("app/Models/User.php", user),
        ("app/Models/Profile.php", PROFILE_MODEL),
    ]);
    let r = resolve_in(&p, user, "bio").expect("promoted property resolves");
    assert_eq!(r.kind, MagicMemberKind::Column);
    assert_eq!(r.declaring_fqcn, "App\\Models\\Profile");
}

#[test]
fn untyped_property_does_not_resolve() {
    let user = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    private $profile;
    public function bio() {
        return $this->profile->bio;
    }
}
"#;
    let p = project_files(&[
        ("app/Models/User.php", user),
        ("app/Models/Profile.php", PROFILE_MODEL),
    ]);
    assert!(
        resolve_in(&p, user, "bio").is_none(),
        "an untyped property gives the resolver nothing to go on"
    );
}

#[test]
fn union_typed_property_is_ambiguous() {
    let user = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    private Profile|Account $profile;
    public function bio() {
        return $this->profile->bio;
    }
}
"#;
    let p = project_files(&[
        ("app/Models/User.php", user),
        ("app/Models/Profile.php", PROFILE_MODEL),
    ]);
    assert!(
        resolve_in(&p, user, "bio").is_none(),
        "a union-typed property is ambiguous and must not resolve"
    );
}

// ─── Widening: foreach iterator vars ─────────────────────────────────────

#[test]
fn widens_foreach_over_collection_variable() {
    let p = project("app/Models/User.php", USER_MODEL);
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function index() {
        $users = User::all();
        foreach ($users as $user) {
            echo $user->email;
        }
    }
}
"#;
    let r = resolve_in(&p, caller, "email").expect("foreach element resolves");
    assert_eq!(r.kind, MagicMemberKind::Column);
    assert_eq!(
        r.confidence,
        Confidence::Medium,
        "an inferred foreach element type is MEDIUM"
    );
    assert_eq!(r.declaring_fqcn, "App\\Models\\User");
}

#[test]
fn widens_foreach_with_key_value_pair() {
    let p = project("app/Models/User.php", USER_MODEL);
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function index() {
        $users = User::all();
        foreach ($users as $i => $user) {
            echo $user->email;
        }
    }
}
"#;
    let r = resolve_in(&p, caller, "email").expect("foreach pair element resolves");
    assert_eq!(r.kind, MagicMemberKind::Column);
    assert_eq!(r.confidence, Confidence::Medium);
}

#[test]
fn foreach_over_unresolvable_collection_yields_none() {
    let p = project("app/Models/User.php", USER_MODEL);
    let caller = r#"<?php
function index($users) {
    foreach ($users as $user) {
        echo $user->email;
    }
}
"#;
    assert!(
        resolve_in(&p, caller, "email").is_none(),
        "an untyped collection gives the foreach widening nothing"
    );
}

#[test]
fn foreach_docblock_var_is_high_via_flow() {
    // A `@var` on the loop body is found by flow directly (HIGH), before the
    // foreach fallback runs.
    let p = project("app/Models/User.php", USER_MODEL);
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function index($rows) {
        foreach ($rows as $user) {
            /** @var User $user */
            echo $user->email;
        }
    }
}
"#;
    let r = resolve_in(&p, caller, "email").expect("docblock resolves");
    assert_eq!(r.kind, MagicMemberKind::Column);
    assert_eq!(r.confidence, Confidence::High);
}

// ─── Widening: method return-type chains ($obj->m()->...) ────────────────

#[test]
fn widens_static_return_type_chain() {
    let user = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    protected $fillable = ['email'];
    public function activated(): static { return $this; }
}
"#;
    let p = project("app/Models/User.php", user);
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function show(User $user) {
        return $user->activated()->email;
    }
}
"#;
    let r = resolve_in(&p, caller, "email").expect("static return chain resolves");
    assert_eq!(r.kind, MagicMemberKind::Column);
    assert_eq!(
        r.confidence,
        Confidence::Medium,
        "return-type inference is indirect"
    );
    assert_eq!(r.declaring_fqcn, "App\\Models\\User");
}

#[test]
fn widens_self_return_type_chain() {
    let user = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    protected $fillable = ['email'];
    public function refreshed(): self { return $this; }
}
"#;
    let p = project("app/Models/User.php", user);
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function show(User $user) {
        return $user->refreshed()->email;
    }
}
"#;
    let r = resolve_in(&p, caller, "email").expect("self return chain resolves");
    assert_eq!(r.kind, MagicMemberKind::Column);
    assert_eq!(r.declaring_fqcn, "App\\Models\\User");
}

#[test]
fn explicit_class_return_type_is_not_resolved() {
    // Out of scope: an arbitrary class return type is written in the
    // declaring file's namespace, which the caller context can't re-qualify.
    let user = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public function makeProfile(): Profile { return new Profile(); }
}
"#;
    let p = project_files(&[
        ("app/Models/User.php", user),
        ("app/Models/Profile.php", PROFILE_MODEL),
    ]);
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function show(User $user) {
        return $user->makeProfile()->bio;
    }
}
"#;
    assert!(resolve_in(&p, caller, "bio").is_none());
}

#[test]
fn untyped_return_does_not_resolve() {
    let user = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    protected $fillable = ['email'];
    public function thing() { return $this; }
}
"#;
    let p = project("app/Models/User.php", user);
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function show(User $user) {
        return $user->thing()->email;
    }
}
"#;
    assert!(resolve_in(&p, caller, "email").is_none());
}

// ─── Population helper: resolve_member_access_entries (M4) ────────────────

use crate::parser::language_php;
use crate::queries::extract_all_php_patterns;
use crate::salsa_impl::MemberAccessReferenceData;
use crate::symbol_index::MagicMemberEntry;
use std::sync::Arc;

/// Capture a caller's property-form member accesses as `MemberAccessReferenceData`
/// (mirrors what `handle_get_patterns` stores), so we can feed the real capture
/// shape into `resolve_member_access_entries`.
fn member_refs_of(source: &str) -> Vec<Arc<MemberAccessReferenceData>> {
    let tree = parse_php(source).expect("parse");
    let lang = language_php();
    extract_all_php_patterns(&tree, source, &lang)
        .expect("extract")
        .member_accesses
        .iter()
        .map(|m| {
            Arc::new(MemberAccessReferenceData {
                member: m.member.to_string(),
                receiver: m.receiver.to_string(),
                receiver_byte_start: m.receiver_byte_start,
                receiver_byte_end: m.receiver_byte_end,
                is_nullsafe: m.is_nullsafe,
                line: m.row as u32,
                column: m.column as u32,
                end_column: m.end_column as u32,
                declaring_fqcn: None,
                kind: None,
                confidence: Confidence::Unresolved,
            })
        })
        .collect()
}

fn has_entry(entries: &[MagicMemberEntry], fqcn: &str, member: &str) -> bool {
    entries.iter().any(|e| e.fqcn == fqcn && e.member == member)
}

#[test]
fn population_resolves_typed_param_member_accesses() {
    let p = project("app/Models/User.php", USER_MODEL);
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function show(User $user) {
        $a = $user->email;
        $b = $user->posts;
        return [$a, $b];
    }
}
"#;
    let refs = member_refs_of(caller);
    let entries =
        resolve_member_access_entries(caller, &refs, &p.index, &mut ClassViewCache::new(), &p.root);
    assert!(
        has_entry(&entries, "App\\Models\\User", "email"),
        "{entries:?}"
    );
    assert!(
        has_entry(&entries, "App\\Models\\User", "posts"),
        "{entries:?}"
    );
    // Position is carried from the capture (member-name span), not the receiver.
    let email = entries.iter().find(|e| e.member == "email").unwrap();
    assert!(email.end_column > email.column);
}

#[test]
fn population_drops_unresolvable_receivers() {
    let p = project("app/Models/User.php", USER_MODEL);
    let caller = r#"<?php
function show($mystery) {
    return $mystery->email;
}
"#;
    let entries = resolve_member_access_entries(
        caller,
        &member_refs_of(caller),
        &p.index,
        &mut ClassViewCache::new(),
        &p.root,
    );
    assert!(
        entries.is_empty(),
        "unresolvable receiver must not produce an index entry, got {entries:?}"
    );
}

#[test]
fn population_drops_unknown_members_on_resolved_receiver() {
    let p = project("app/Models/User.php", USER_MODEL);
    // `$user` resolves to User, but `notAColumn` isn't a known member.
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function show(User $user) {
        return $user->notAColumn;
    }
}
"#;
    let entries = resolve_member_access_entries(
        caller,
        &member_refs_of(caller),
        &p.index,
        &mut ClassViewCache::new(),
        &p.root,
    );
    assert!(entries.is_empty(), "{entries:?}");
}

#[test]
fn population_empty_refs_is_empty() {
    let p = project("app/Models/User.php", USER_MODEL);
    let entries =
        resolve_member_access_entries("", &[], &p.index, &mut ClassViewCache::new(), &p.root);
    assert!(entries.is_empty());
}

#[test]
fn end_to_end_warming_flow_resolves_this_email() {
    // Mirror the real warming → magic-build flow exactly:
    // parse_owned_with_hierarchy → build index → fqcn_file_map snapshot →
    // resolve_member_access_entries against the snapshot. This is what the
    // warming pass does; the other tests use the index directly + re-extract.
    use crate::class_hierarchy_index::ClassHierarchyIndex;
    use crate::pattern_indexer::parse_owned_with_hierarchy;

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("app/Models/User.php");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let src = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    protected $fillable = ['email'];
    public function gravatar(): string { return md5($this->email); }
}
"#;
    fs::write(&path, src).unwrap();
    fs::write(
        dir.path().join("composer.json"),
        r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#,
    )
    .unwrap();

    let (data, nodes) = parse_owned_with_hierarchy(&path, src);
    assert!(
        !data.member_access_refs.is_empty(),
        "warming parse must capture $this->email"
    );

    let mut index = ClassHierarchyIndex::default();
    index.insert_file(&path, nodes);
    let snapshot = index.fqcn_file_map();
    assert!(
        snapshot.contains_key("App\\Models\\User"),
        "snapshot must map the model fqcn; keys: {:?}",
        snapshot.keys().collect::<Vec<_>>()
    );

    let entries = resolve_member_access_entries(
        src,
        &data.member_access_refs,
        &snapshot,
        &mut ClassViewCache::new(),
        dir.path(),
    );
    assert!(
        entries
            .iter()
            .any(|e| e.member == "email" && e.fqcn == "App\\Models\\User"),
        "end-to-end warming flow should resolve $this->email; got {entries:?}"
    );
}

#[test]
fn realistic_user_resolves_through_full_warm_restart_cycle() {
    // Mirror the production warm-restart path end to end: parse → save
    // (patterns + hierarchy nodes) → load → rebuild hierarchy from the
    // restored nodes → fqcn_file_map snapshot → resolve the restored
    // patterns' member accesses. Uses a realistic User (extends an aliased
    // vendor base, uses a trait) to match the real model shape.
    use crate::class_hierarchy_index::ClassHierarchyIndex;
    use crate::pattern_indexer::parse_owned_with_hierarchy;
    use dashmap::DashMap;

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("app/Models/User.php");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let src = r#"<?php
namespace App\Models;
use Illuminate\Foundation\Auth\User as Authenticatable;
use Illuminate\Notifications\Notifiable;
class User extends Authenticatable {
    use Notifiable;
    protected $fillable = ['name', 'email', 'password'];
    public function getGravatarAttribute(): string {
        return md5(strtolower($this->email));
    }
}
"#;
    fs::write(&path, src).unwrap();
    fs::write(
        dir.path().join("composer.json"),
        r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#,
    )
    .unwrap();

    let (data, nodes) = parse_owned_with_hierarchy(&path, src);

    // Save (patterns + hierarchy) then restore — the warm-restart cycle.
    let cache = Arc::new(DashMap::new());
    cache.insert(path.clone(), (0, data));
    let mut hierarchy_by_file = std::collections::HashMap::new();
    hierarchy_by_file.insert(path.clone(), nodes);
    crate::pattern_disk_cache::save_from(&cache, &hierarchy_by_file, dir.path()).unwrap();

    let restored_cache = Arc::new(DashMap::new());
    let lr = crate::pattern_disk_cache::load_into(&restored_cache, dir.path());

    let mut index = ClassHierarchyIndex::default();
    for (p, ns) in lr.hierarchy {
        index.insert_file(&p, ns);
    }
    let snapshot = index.fqcn_file_map();
    assert!(
        snapshot.contains_key("App\\Models\\User"),
        "restored snapshot must contain the app model; keys: {:?}",
        snapshot.keys().collect::<Vec<_>>()
    );

    let restored = restored_cache.get(&path).unwrap();
    let refs = restored.value().1.member_access_refs.clone();
    assert!(
        !refs.is_empty(),
        "restored patterns must carry member accesses"
    );

    let entries = resolve_member_access_entries(
        src,
        &refs,
        &snapshot,
        &mut ClassViewCache::new(),
        dir.path(),
    );
    assert!(
        entries
            .iter()
            .any(|e| e.member == "email" && e.fqcn == "App\\Models\\User"),
        "warm-restart cycle should resolve $this->email; got {entries:?}"
    );
}
