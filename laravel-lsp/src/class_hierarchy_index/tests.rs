//! Tests for the class-hierarchy + member index.

use super::*;
use std::path::PathBuf;

fn extract(path: &str, content: &str) -> Vec<ClassNode> {
    classes_in_file(&PathBuf::from(path), content)
}

#[test]
fn resolves_fqcn_and_edges_via_use_aliases() {
    let content = r#"<?php
namespace App\Models;

use Illuminate\Database\Eloquent\Model;
use App\Contracts\HasAvatar;
use App\Concerns\Sluggable;

class User extends Model implements HasAvatar
{
    use Sluggable;

    public function posts() {}
}
"#;
    let nodes = extract("/proj/app/Models/User.php", content);
    assert_eq!(nodes.len(), 1);
    let n = &nodes[0];
    assert_eq!(n.fqcn, "App\\Models\\User");
    assert_eq!(
        n.extends.as_deref(),
        Some("Illuminate\\Database\\Eloquent\\Model")
    );
    assert_eq!(n.implements, vec!["App\\Contracts\\HasAvatar".to_string()]);
    assert_eq!(n.trait_uses, vec!["App\\Concerns\\Sluggable".to_string()]);
    assert!(n.methods.iter().any(|m| m.name == "posts"));
}

#[test]
fn bare_unaliased_name_qualifies_to_current_namespace() {
    let content = r#"<?php
namespace App\Models;
class Admin extends User {}
"#;
    let nodes = extract("/proj/app/Models/Admin.php", content);
    assert_eq!(nodes[0].extends.as_deref(), Some("App\\Models\\User"));
}

#[test]
fn leading_backslash_treated_as_absolute() {
    let content = r#"<?php
namespace App\Models;
class Counter extends \Livewire\Component {}
"#;
    let nodes = extract("/proj/app/Models/Counter.php", content);
    assert_eq!(nodes[0].extends.as_deref(), Some("Livewire\\Component"));
}

#[test]
fn reverse_edges_populate_and_query() {
    let mut idx = ClassHierarchyIndex::default();

    let iface_path = PathBuf::from("/p/HasAvatar.php");
    let iface = "<?php\nnamespace App\\Contracts;\ninterface HasAvatar {}\n";
    let user_path = PathBuf::from("/p/User.php");
    let user = "<?php\nnamespace App\\Models;\nuse App\\Contracts\\HasAvatar;\nuse Illuminate\\Database\\Eloquent\\Model;\nclass User extends Model implements HasAvatar {}\n";
    let admin_path = PathBuf::from("/p/Admin.php");
    let admin = "<?php\nnamespace App\\Models;\nclass Admin extends User {}\n";

    idx.insert_file(&iface_path, classes_in_file(&iface_path, iface));
    idx.insert_file(&user_path, classes_in_file(&user_path, user));
    idx.insert_file(&admin_path, classes_in_file(&admin_path, admin));

    assert_eq!(
        idx.implementers_of("App\\Contracts\\HasAvatar").to_vec(),
        vec!["App\\Models\\User".to_string()]
    );
    assert_eq!(
        idx.subclasses_of("App\\Models\\User").to_vec(),
        vec!["App\\Models\\Admin".to_string()]
    );
    assert_eq!(
        idx.parent_of("App\\Models\\Admin"),
        Some("App\\Models\\User")
    );
    assert!(idx.get("App\\Models\\User").is_some());
    assert_eq!(idx.class_count(), 3);
}

#[test]
fn trait_users_tracked() {
    let mut idx = ClassHierarchyIndex::default();
    let tr_path = PathBuf::from("/p/Sluggable.php");
    let tr = "<?php\nnamespace App\\Concerns;\ntrait Sluggable {}\n";
    let post_path = PathBuf::from("/p/Post.php");
    let post = "<?php\nnamespace App\\Models;\nuse App\\Concerns\\Sluggable;\nclass Post { use Sluggable; }\n";

    idx.insert_file(&tr_path, classes_in_file(&tr_path, tr));
    idx.insert_file(&post_path, classes_in_file(&post_path, post));

    assert_eq!(
        idx.trait_users_of("App\\Concerns\\Sluggable").to_vec(),
        vec!["App\\Models\\Post".to_string()]
    );
}

#[test]
fn remove_file_evicts_node_and_reverse_edges() {
    let mut idx = ClassHierarchyIndex::default();
    let user_path = PathBuf::from("/p/User.php");
    let user = "<?php\nnamespace App\\Models;\nuse App\\Contracts\\HasAvatar;\nclass User implements HasAvatar {}\n";

    idx.insert_file(&user_path, classes_in_file(&user_path, user));
    assert_eq!(idx.implementers_of("App\\Contracts\\HasAvatar").len(), 1);

    idx.remove_file(&user_path);
    assert!(idx.get("App\\Models\\User").is_none());
    assert!(idx.implementers_of("App\\Contracts\\HasAvatar").is_empty());
    assert_eq!(idx.indexed_file_count(), 0);
}

#[test]
fn take_dirty_drains_and_is_idempotent() {
    let mut idx = ClassHierarchyIndex::default();
    idx.mark_dirty(&PathBuf::from("/p/A.php"));
    idx.mark_dirty(&PathBuf::from("/p/A.php"));
    assert_eq!(idx.take_dirty().len(), 1);
    assert!(idx.take_dirty().is_empty());
}

#[test]
fn fqcn_file_map_maps_each_class_to_its_file() {
    let mut idx = ClassHierarchyIndex::default();
    let user_path = PathBuf::from("/proj/app/Models/User.php");
    let post_path = PathBuf::from("/proj/app/Models/Post.php");
    idx.insert_file(
        &user_path,
        extract(
            "/proj/app/Models/User.php",
            "<?php\nnamespace App\\Models;\nclass User {}\n",
        ),
    );
    idx.insert_file(
        &post_path,
        extract(
            "/proj/app/Models/Post.php",
            "<?php\nnamespace App\\Models;\nclass Post {}\n",
        ),
    );

    let map = idx.fqcn_file_map();
    assert_eq!(map.get("App\\Models\\User"), Some(&user_path));
    assert_eq!(map.get("App\\Models\\Post"), Some(&post_path));
}

// ── Snapshot-invalidation helpers (perf: cached class→file snapshot) ──────

#[test]
fn fqcns_changed_detects_class_set_changes() {
    let mut idx = ClassHierarchyIndex::default();
    let path = PathBuf::from("/proj/app/Models/User.php");
    let original = r#"<?php
namespace App\Models;
class User {
    public function show() {}
}
"#;
    idx.insert_file(&path, extract("/proj/app/Models/User.php", original));

    // A method-body edit leaves the same class — no mapping change, so the
    // cached snapshot can stay valid (the common edit stays O(1)).
    let body_edit = r#"<?php
namespace App\Models;
class User {
    public function show() { return 42; }
}
"#;
    assert!(!idx.fqcns_changed(&path, &extract("/proj/app/Models/User.php", body_edit)));

    // Adding a class to the file changes the set → invalidates.
    let added_class = r#"<?php
namespace App\Models;
class User {}
class Profile {}
"#;
    assert!(idx.fqcns_changed(&path, &extract("/proj/app/Models/User.php", added_class)));

    // A brand-new (unindexed) path with classes is a change.
    let other = PathBuf::from("/proj/app/Models/Post.php");
    let post = "<?php\nnamespace App\\Models;\nclass Post {}\n";
    assert!(idx.fqcns_changed(&other, &extract("/proj/app/Models/Post.php", post)));
}

#[test]
fn contains_file_tracks_indexed_paths() {
    let mut idx = ClassHierarchyIndex::default();
    let path = PathBuf::from("/proj/app/Models/User.php");
    assert!(!idx.contains_file(&path));
    idx.insert_file(
        &path,
        extract("/proj/app/Models/User.php", "<?php\nclass User {}\n"),
    );
    assert!(idx.contains_file(&path));
    idx.remove_file(&path);
    assert!(!idx.contains_file(&path));
}

// ── Surface signatures (incremental save gate, #80) ─────────────────────

#[test]
fn surface_signature_ignores_body_edits_and_span_shifts() {
    let original = r#"<?php
namespace App\Models;
class User {
    public $name;
    public function show() { return 1; }
}
"#;
    // Same surface: a fatter body and extra blank lines shift every span.
    let body_edit = r#"<?php
namespace App\Models;


class User {
    public $name;
    public function show() {
        $value = compute();
        return $value + 41;
    }
}
"#;
    let a = extract("/proj/app/Models/User.php", original);
    let b = extract("/proj/app/Models/User.php", body_edit);
    assert_eq!(a[0].surface_signature(), b[0].surface_signature());
}

#[test]
fn surface_signature_ignores_member_order() {
    let ab = r#"<?php
class User {
    public function alpha() {}
    public function beta() {}
}
"#;
    let ba = r#"<?php
class User {
    public function beta() {}
    public function alpha() {}
}
"#;
    let a = extract("/proj/User.php", ab);
    let b = extract("/proj/User.php", ba);
    assert_eq!(a[0].surface_signature(), b[0].surface_signature());
}

#[test]
fn surface_signature_changes_on_surface_edits() {
    let base = r#"<?php
namespace App\Models;
class User extends Base {
    public $name;
    public function show() {}
}
"#;
    let base_sig = extract("/proj/User.php", base)[0].surface_signature();

    // Added method.
    let added = base.replace(
        "public function show() {}",
        "public function show() {}\n    public function hide() {}",
    );
    assert_ne!(
        base_sig,
        extract("/proj/User.php", &added)[0].surface_signature()
    );

    // Changed parent.
    let reparented = base.replace("extends Base", "extends Other");
    assert_ne!(
        base_sig,
        extract("/proj/User.php", &reparented)[0].surface_signature()
    );

    // Static-ness flip on a member.
    let statified = base.replace("public function show()", "public static function show()");
    assert_ne!(
        base_sig,
        extract("/proj/User.php", &statified)[0].surface_signature()
    );

    // Added trait use.
    let traited = base.replace("public $name;", "use Sluggable;\n    public $name;");
    assert_ne!(
        base_sig,
        extract("/proj/User.php", &traited)[0].surface_signature()
    );
}

#[test]
fn surface_diff_reports_changed_added_and_removed() {
    let mut idx = ClassHierarchyIndex::default();
    let path = PathBuf::from("/proj/app/Models/User.php");
    let original = r#"<?php
namespace App\Models;
class User { public function show() {} }
class Profile { public $bio; }
"#;
    idx.insert_file(&path, extract("/proj/app/Models/User.php", original));
    let old = idx.file_surfaces(&path);
    assert_eq!(old.len(), 2);

    // Body-only edit → empty diff.
    let body_edit = r#"<?php
namespace App\Models;
class User { public function show() { return 7; } }
class Profile { public $bio; }
"#;
    let nodes = extract("/proj/app/Models/User.php", body_edit);
    assert!(surface_diff(&old, &nodes).is_empty());

    // User gains a method (changed), Profile is dropped (removed),
    // Avatar appears (added) → all three FQCNs reported.
    let reshaped = r#"<?php
namespace App\Models;
class User { public function show() {} public function hide() {} }
class Avatar {}
"#;
    let nodes = extract("/proj/app/Models/User.php", reshaped);
    let mut diff = surface_diff(&old, &nodes);
    diff.sort();
    assert_eq!(
        diff,
        vec![
            "App\\Models\\Avatar".to_string(),
            "App\\Models\\Profile".to_string(),
            "App\\Models\\User".to_string(),
        ]
    );
}

#[test]
fn expand_with_descendants_walks_all_edge_kinds_transitively() {
    let mut idx = ClassHierarchyIndex::default();
    let src = r#"<?php
namespace App;
interface Contract {}
trait Helper {}
class Base {}
class Child extends Base {}
class GrandChild extends Child {}
class Impl implements Contract {}
class Uses { use Helper; }
"#;
    idx.insert_file(
        &PathBuf::from("/proj/app/All.php"),
        extract("/proj/app/All.php", src),
    );

    let radius = idx.expand_with_descendants(&["App\\Base".to_string()]);
    assert!(radius.contains("App\\Base"));
    assert!(radius.contains("App\\Child"));
    assert!(radius.contains("App\\GrandChild"));
    assert!(!radius.contains("App\\Impl"));

    let radius = idx.expand_with_descendants(&["App\\Contract".to_string()]);
    assert!(radius.contains("App\\Impl"));

    let radius = idx.expand_with_descendants(&["App\\Helper".to_string()]);
    assert!(radius.contains("App\\Uses"));
}
