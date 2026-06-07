//! Tests for controller → Blade view-variable extraction.
//!
//! Variable types here resolve via flow tracking on a typed parameter, so no
//! on-disk model is needed — `view_renders_in_file` returns the view name and
//! the `var → fqcn` map for each render site.

use super::*;
use crate::class_hierarchy_index::ClassHierarchyIndex;
use std::path::Path;

fn renders(controller: &str) -> Vec<ViewRender> {
    view_renders_in_file(
        controller,
        &ClassHierarchyIndex::default(),
        &mut ClassViewCache::new(),
        Path::new("/proj"),
    )
}

const CTRL_HEADER: &str = "<?php
namespace App\\Http\\Controllers;
use App\\Models\\User;
class C {
    public function show(User $user) {
";

#[test]
fn extracts_array_data() {
    let src =
        format!("{CTRL_HEADER}        return view('users.show', ['user' => $user]);\n    }}\n}}\n");
    let r = renders(&src);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].view_name, "users.show");
    assert_eq!(
        r[0].vars.get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn extracts_compact() {
    let src =
        format!("{CTRL_HEADER}        return view('users.show', compact('user'));\n    }}\n}}\n");
    let r = renders(&src);
    assert_eq!(r.len(), 1);
    assert_eq!(
        r[0].vars.get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn extracts_with_key_value() {
    let src = format!(
        "{CTRL_HEADER}        return view('users.show')->with('user', $user);\n    }}\n}}\n"
    );
    let r = renders(&src);
    assert_eq!(r.len(), 1, "got {r:?}");
    assert_eq!(r[0].view_name, "users.show");
    assert_eq!(
        r[0].vars.get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn extracts_with_array() {
    let src = format!(
        "{CTRL_HEADER}        return view('users.show')->with(['user' => $user]);\n    }}\n}}\n"
    );
    let r = renders(&src);
    assert_eq!(r.len(), 1);
    assert_eq!(
        r[0].vars.get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn unresolvable_value_is_omitted() {
    // `$mystery` has no type info → the var simply doesn't appear (vs. a wrong
    // guess). The view render is still recorded.
    let src = "<?php
function show($mystery) {
    return view('x', ['thing' => $mystery]);
}
";
    let r = renders(src);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].view_name, "x");
    assert!(r[0].vars.is_empty(), "got {:?}", r[0].vars);
}

#[test]
fn no_view_calls_yields_empty() {
    let r = renders("<?php\nfunction f() { return 1; }\n");
    assert!(r.is_empty());
}

// ---- ViewVarIndex --------------------------------------------------------

use std::collections::HashMap;
use std::path::PathBuf;

fn render(view: &str, vars: &[(&str, &str)]) -> ViewRender {
    ViewRender {
        view_name: view.to_string(),
        vars: vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<HashMap<_, _>>(),
    }
}

#[test]
fn index_returns_var_type() {
    let mut idx = ViewVarIndex::new();
    idx.insert_file(
        PathBuf::from("/proj/UserController.php"),
        &[render("users.show", &[("user", "App\\Models\\User")])],
    );
    assert_eq!(
        idx.var_types("users.show", "user"),
        vec!["App\\Models\\User"]
    );
    assert!(idx.var_types("users.show", "missing").is_empty());
    assert!(idx.var_types("other.view", "user").is_empty());
}

#[test]
fn index_unions_types_across_files() {
    // Two controllers render the same view with different types for `user`.
    let mut idx = ViewVarIndex::new();
    idx.insert_file(
        PathBuf::from("/proj/UserController.php"),
        &[render("dash", &[("user", "App\\Models\\User")])],
    );
    idx.insert_file(
        PathBuf::from("/proj/AdminController.php"),
        &[render("dash", &[("user", "App\\Models\\Admin")])],
    );
    // Union — both observed types are kept (sorted).
    assert_eq!(
        idx.var_types("dash", "user"),
        vec!["App\\Models\\Admin", "App\\Models\\User"]
    );
}

#[test]
fn index_evicts_on_reinsert() {
    let mut idx = ViewVarIndex::new();
    let path = PathBuf::from("/proj/UserController.php");
    idx.insert_file(path.clone(), &[render("v", &[("a", "App\\A")])]);
    // Re-parse of the same file now renders a different var — old one is gone.
    idx.insert_file(path, &[render("v", &[("b", "App\\B")])]);
    assert!(idx.var_types("v", "a").is_empty());
    assert_eq!(idx.var_types("v", "b"), vec!["App\\B"]);
}

#[test]
fn index_clear_empties() {
    let mut idx = ViewVarIndex::new();
    idx.insert_file(
        PathBuf::from("/proj/C.php"),
        &[render("v", &[("a", "App\\A")])],
    );
    assert!(!idx.is_empty());
    idx.clear();
    assert!(idx.is_empty());
    assert_eq!(idx.view_count(), 0);
}

// ---- view_name_for_path --------------------------------------------------

#[test]
fn view_name_strips_root_and_suffix() {
    let roots = vec![PathBuf::from("/proj/resources/views")];
    assert_eq!(
        view_name_for_path(
            Path::new("/proj/resources/views/users/show.blade.php"),
            &roots
        ),
        Some("users.show".to_string())
    );
    assert_eq!(
        view_name_for_path(Path::new("/proj/resources/views/welcome.blade.php"), &roots),
        Some("welcome".to_string())
    );
}

#[test]
fn view_name_none_outside_roots() {
    let roots = vec![PathBuf::from("/proj/resources/views")];
    assert_eq!(
        view_name_for_path(Path::new("/proj/app/Models/User.php"), &roots),
        None
    );
}

#[test]
fn view_name_longest_root_wins() {
    // A package view root nested under the app's view root should win, yielding
    // the package-relative name rather than the deep app-relative one.
    let roots = vec![
        PathBuf::from("/proj/resources/views"),
        PathBuf::from("/proj/resources/views/vendor/pkg"),
    ];
    assert_eq!(
        view_name_for_path(
            Path::new("/proj/resources/views/vendor/pkg/button.blade.php"),
            &roots
        ),
        Some("button".to_string())
    );
}

// ---- resolve_blade_member_accesses ---------------------------------------

use crate::salsa_impl::{Confidence, MemberAccessReferenceData};
use std::fs;
use std::sync::Arc;
use tempfile::TempDir;

const USER_MODEL: &str = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
use Illuminate\Database\Eloquent\Relations\HasMany;
class User extends Model {
    protected $fillable = ['email'];
    public function posts(): HasMany { return $this->hasMany(Post::class); }
}
"#;

/// A temp project with `app/Models/User.php` indexed, plus a ready resolver.
struct BladeProject {
    _dir: TempDir,
    index: ClassHierarchyIndex,
    root: PathBuf,
}

fn blade_project() -> BladeProject {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let model_path = root.join("app/Models/User.php");
    fs::create_dir_all(model_path.parent().unwrap()).unwrap();
    fs::write(&model_path, USER_MODEL).unwrap();
    fs::write(
        root.join("composer.json"),
        r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#,
    )
    .unwrap();
    let mut index = ClassHierarchyIndex::default();
    index.insert_file(
        &model_path,
        crate::class_hierarchy_index::classes_in_file(&model_path, USER_MODEL),
    );
    BladeProject {
        _dir: dir,
        index,
        root,
    }
}

/// A property-form member-access ref as the capture pass would emit it
/// (byte ranges unused for the Blade path — only receiver text + position).
fn member_ref(
    receiver: &str,
    member: &str,
    line: u32,
    column: u32,
) -> Arc<MemberAccessReferenceData> {
    Arc::new(MemberAccessReferenceData {
        member: member.to_string(),
        receiver: receiver.to_string(),
        receiver_byte_start: 0,
        receiver_byte_end: 0,
        is_nullsafe: false,
        line,
        column,
        end_column: column + member.len() as u32,
        declaring_fqcn: None,
        kind: None,
        confidence: Confidence::Unresolved,
    })
}

#[test]
fn blade_var_resolves_via_view_index() {
    let p = blade_project();
    let mut idx = ViewVarIndex::new();
    idx.insert_file(
        p.root.join("app/Http/Controllers/UserController.php"),
        &[render("users.show", &[("user", "App\\Models\\User")])],
    );

    // `{{ $user->email }}` captured at line 3, col 15.
    let refs = vec![member_ref("$user", "email", 3, 15)];
    let mut cache = ClassViewCache::new();
    let entries = resolve_blade_member_accesses(
        &refs,
        "users.show",
        &idx,
        &[],
        &p.index,
        &mut cache,
        &p.root,
    );

    assert_eq!(entries.len(), 1, "got {entries:?}");
    assert_eq!(entries[0].fqcn, "App\\Models\\User");
    assert_eq!(entries[0].member, "email");
    assert_eq!(entries[0].line, 3);
    assert_eq!(entries[0].column, 15);
}

#[test]
fn blade_unknown_member_is_dropped() {
    let p = blade_project();
    let mut idx = ViewVarIndex::new();
    idx.insert_file(
        p.root.join("C.php"),
        &[render("users.show", &[("user", "App\\Models\\User")])],
    );
    // `nope` is not a column/accessor/relationship/property on User → dropped.
    let refs = vec![member_ref("$user", "nope", 1, 0)];
    let mut cache = ClassViewCache::new();
    let entries = resolve_blade_member_accesses(
        &refs,
        "users.show",
        &idx,
        &[],
        &p.index,
        &mut cache,
        &p.root,
    );
    assert!(entries.is_empty(), "got {entries:?}");
}

#[test]
fn blade_var_with_no_inferred_type_is_dropped() {
    let p = blade_project();
    let idx = ViewVarIndex::new(); // empty — nothing rendered this view
    let refs = vec![member_ref("$user", "email", 1, 0)];
    let mut cache = ClassViewCache::new();
    let entries = resolve_blade_member_accesses(
        &refs,
        "users.show",
        &idx,
        &[],
        &p.index,
        &mut cache,
        &p.root,
    );
    assert!(entries.is_empty(), "got {entries:?}");
}

#[test]
fn blade_relationship_resolves() {
    let p = blade_project();
    let mut idx = ViewVarIndex::new();
    idx.insert_file(
        p.root.join("C.php"),
        &[render("users.show", &[("user", "App\\Models\\User")])],
    );
    // `{{ $user->posts }}` — relationship read as a property.
    let refs = vec![member_ref("$user", "posts", 2, 4)];
    let mut cache = ClassViewCache::new();
    let entries = resolve_blade_member_accesses(
        &refs,
        "users.show",
        &idx,
        &[],
        &p.index,
        &mut cache,
        &p.root,
    );
    assert_eq!(entries.len(), 1, "got {entries:?}");
    assert_eq!(entries[0].fqcn, "App\\Models\\User");
    assert_eq!(entries[0].member, "posts");
}

// ---- Volt: volt_property_types -------------------------------------------

/// Extract Volt prop types. Inline Eloquent values (`User::first()`) resolve via
/// the flow chain classifier, which needs only the `use` aliases — no on-disk
/// model — so a default resolver suffices here.
fn volt_types(src: &str) -> HashMap<String, String> {
    volt_property_types(
        src,
        &ClassHierarchyIndex::default(),
        &mut ClassViewCache::new(),
        Path::new("/proj"),
    )
}

#[test]
fn volt_typed_public_property() {
    let src = r#"<?php
use App\Models\User;
use Livewire\Volt\Component;

new class extends Component {
    public User $user;
    public ?User $maybe;
    public int $count = 0;
};
?>
<div>{{ $this->user->email }}</div>
"#;
    let types = volt_types(src);
    assert_eq!(
        types.get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
    // Nullable still resolves.
    assert_eq!(
        types.get("maybe").map(String::as_str),
        Some("App\\Models\\User")
    );
    // Builtins are not classes — excluded.
    assert!(!types.contains_key("count"), "got {types:?}");
}

#[test]
fn volt_functional_mount_assignment() {
    let src = r#"<?php
use App\Models\User;
use function Livewire\Volt\{state, mount};

state(['user']);

mount(function (User $user) {
    $this->user = $user;
});
?>
<div>{{ $this->user->email }}</div>
"#;
    let types = volt_types(src);
    assert_eq!(
        types.get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn volt_class_mount_assignment() {
    let src = r#"<?php
use App\Models\User;
use Livewire\Volt\Component;

new class extends Component {
    public $user;
    public function mount(User $account) {
        $this->user = $account;
    }
};
?>
"#;
    let types = volt_types(src);
    assert_eq!(
        types.get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn volt_untyped_state_yields_nothing() {
    let src = r#"<?php
use function Livewire\Volt\{state};
state(['count' => 0]);
?>
<div>{{ $count }}</div>
"#;
    assert!(volt_types(src).is_empty());
}

#[test]
fn volt_state_typed_initial_value() {
    let src = r#"<?php
use App\Models\User;
use function Livewire\Volt\{state};
state(['user' => User::find(1)]);
?>
<div>{{ $user->email }}</div>
"#;
    assert_eq!(
        volt_types(src).get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn volt_computed_explicit_return_type() {
    let src = r#"<?php
use App\Models\User;
use function Livewire\Volt\{computed};
$user = computed(fn (): User => User::find(1));
?>
<div>{{ $this->user->email }}</div>
"#;
    assert_eq!(
        volt_types(src).get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn volt_computed_inferred_from_body() {
    let src = r#"<?php
use App\Models\User;
use function Livewire\Volt\{computed};
$user = computed(fn () => User::firstWhere('id', 1));
?>
<div>{{ $this->user->email }}</div>
"#;
    assert_eq!(
        volt_types(src).get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn volt_with_closure_array() {
    let src = r#"<?php
use App\Models\User;
use function Livewire\Volt\{with};
with(fn () => ['account' => User::first()]);
?>
<div>{{ $account->email }}</div>
"#;
    assert_eq!(
        volt_types(src).get("account").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn volt_class_render_view_data() {
    let src = r#"<?php
use App\Models\User;
use Livewire\Volt\Component;
new class extends Component {
    public function render() {
        return view('livewire.users', ['account' => User::first()]);
    }
};
?>
"#;
    assert_eq!(
        volt_types(src).get("account").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn volt_typed_property_wins_over_state() {
    // A typed public prop must not be downgraded by a same-named state entry.
    let src = r#"<?php
use App\Models\User;
use Livewire\Volt\Component;
new class extends Component {
    public User $user;
};
?>
"#;
    assert_eq!(
        volt_types(src).get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
}

// ---- Volt: resolve_volt_member_accesses ----------------------------------

#[test]
fn volt_resolves_this_property_access() {
    let p = blade_project();
    let mut types = HashMap::new();
    types.insert("user".to_string(), "App\\Models\\User".to_string());

    // `{{ $this->user->email }}` — receiver captured as `$this->user`.
    let refs = vec![member_ref("$this->user", "email", 5, 18)];
    let mut cache = ClassViewCache::new();
    let entries = resolve_volt_member_accesses(&refs, &types, &[], &p.index, &mut cache, &p.root);
    assert_eq!(entries.len(), 1, "got {entries:?}");
    assert_eq!(entries[0].fqcn, "App\\Models\\User");
    assert_eq!(entries[0].member, "email");
}

#[test]
fn volt_resolves_bare_public_property_access() {
    let p = blade_project();
    let mut types = HashMap::new();
    types.insert("user".to_string(), "App\\Models\\User".to_string());

    // Public properties are also readable bare in the template: `{{ $user->email }}`.
    let refs = vec![member_ref("$user", "email", 1, 0)];
    let mut cache = ClassViewCache::new();
    let entries = resolve_volt_member_accesses(&refs, &types, &[], &p.index, &mut cache, &p.root);
    assert_eq!(entries.len(), 1, "got {entries:?}");
    assert_eq!(entries[0].fqcn, "App\\Models\\User");
}

#[test]
fn volt_unknown_property_is_dropped() {
    let p = blade_project();
    let types = HashMap::new(); // nothing inferred
    let refs = vec![member_ref("$this->user", "email", 1, 0)];
    let mut cache = ClassViewCache::new();
    let entries = resolve_volt_member_accesses(&refs, &types, &[], &p.index, &mut cache, &p.root);
    assert!(entries.is_empty(), "got {entries:?}");
}

// ---- MFC / @foreach loop-variable typing ---------------------------------

#[test]
fn volt_computed_attribute_collection_body_inference() {
    // `#[Computed] public function users(): Collection { return User::...->get(); }`
    // — declared return type is a bare Collection, so the element type must come
    // from the body's flow chain (→ App\Models\User).
    let src = r#"<?php
use App\Models\User;
use Illuminate\Database\Eloquent\Collection;
use Livewire\Volt\Component;
new class extends Component {
    #[Computed]
    public function users(): Collection {
        return User::with("roles")->orderBy("name")->get();
    }
};
?>
"#;
    assert_eq!(
        volt_types(src).get("users").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn volt_foreach_loop_var_resolves_from_this_computed() {
    let p = blade_project();
    // `users` computed yields User elements.
    let mut types = HashMap::new();
    types.insert("users".to_string(), "App\\Models\\User".to_string());

    // `@foreach($this->users as $user)` on line 5; `{{ $user->email }}` on line 7.
    let loops = vec![crate::salsa_impl::BladeLoopVar {
        item_var: "user".to_string(),
        iterable: "$this->users".to_string(),
        start_line: 5,
        end_line: 20,
    }];
    let refs = vec![member_ref("$user", "email", 7, 40)];
    let mut cache = ClassViewCache::new();
    let entries =
        resolve_volt_member_accesses(&refs, &types, &loops, &p.index, &mut cache, &p.root);
    assert_eq!(entries.len(), 1, "got {entries:?}");
    assert_eq!(entries[0].fqcn, "App\\Models\\User");
    assert_eq!(entries[0].member, "email");
}

#[test]
fn blade_foreach_loop_var_resolves_from_view_var() {
    let p = blade_project();
    let mut idx = ViewVarIndex::new();
    // Controller passed `users` (a User collection → element type User).
    idx.insert_file(
        p.root.join("C.php"),
        &[render("users.index", &[("users", "App\\Models\\User")])],
    );
    // `@foreach($users as $user)` lines 2..10; `{{ $user->email }}` line 4.
    let loops = vec![crate::salsa_impl::BladeLoopVar {
        item_var: "user".to_string(),
        iterable: "$users".to_string(),
        start_line: 2,
        end_line: 10,
    }];
    let refs = vec![member_ref("$user", "email", 4, 12)];
    let mut cache = ClassViewCache::new();
    let entries = resolve_blade_member_accesses(
        &refs,
        "users.index",
        &idx,
        &loops,
        &p.index,
        &mut cache,
        &p.root,
    );
    assert_eq!(entries.len(), 1, "got {entries:?}");
    assert_eq!(entries[0].fqcn, "App\\Models\\User");
}

#[test]
fn loop_var_outside_loop_range_is_dropped() {
    let p = blade_project();
    let mut types = HashMap::new();
    types.insert("users".to_string(), "App\\Models\\User".to_string());
    let loops = vec![crate::salsa_impl::BladeLoopVar {
        item_var: "user".to_string(),
        iterable: "$this->users".to_string(),
        start_line: 5,
        end_line: 20,
    }];
    // Access on line 30 — outside the loop body → no resolution.
    let refs = vec![member_ref("$user", "email", 30, 0)];
    let mut cache = ClassViewCache::new();
    let entries =
        resolve_volt_member_accesses(&refs, &types, &loops, &p.index, &mut cache, &p.root);
    assert!(entries.is_empty(), "got {entries:?}");
}
