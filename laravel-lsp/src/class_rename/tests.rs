use super::{class_at_cursor, is_dependency_path, project_php_files, reference_spans};
use std::path::Path;

/// Replace every span (right-to-left so earlier offsets stay valid) — mimics
/// what the rename WorkspaceEdit does, so we can assert on the rewritten source.
fn apply(content: &str, spans: &[(usize, usize)], new: &str) -> String {
    let mut out = content.to_string();
    let mut spans = spans.to_vec();
    spans.sort_unstable();
    for (s, e) in spans.into_iter().rev() {
        out.replace_range(s..e, new);
    }
    out
}

fn rename(content: &str, fqcn: &str, old: &str, new: &str) -> String {
    apply(content, &reference_spans(content, fqcn, old), new)
}

// ---- declaration + same-file references -----------------------------------

#[test]
fn renames_declaration_and_self_references() {
    let src = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model
{
    public function scopeRecent($q) { return $q; }
}
"#;
    let out = rename(src, "App\\Models\\User", "User", "Customer");
    assert!(out.contains("class Customer extends Model"));
    // The base class `Model` is untouched.
    assert!(out.contains("extends Model"));
}

// ---- use imports + static calls + new + type hints ------------------------

const CONTROLLER: &str = r#"<?php
namespace App\Http\Controllers;

use App\Models\User;
use App\Models\Post;

class UserController extends Controller
{
    public function show(User $user): ?User
    {
        $u = new User();
        $found = User::where('id', 1)->first();
        $key = User::class;
        if ($u instanceof User) {
            return $u;
        }
        return $found;
    }

    public User $current;
}
"#;

#[test]
fn renames_all_reference_kinds_in_consumer_file() {
    let out = rename(CONTROLLER, "App\\Models\\User", "User", "Member");
    // use import
    assert!(
        out.contains("use App\\Models\\Member;"),
        "use import\n{out}"
    );
    // param + return type
    assert!(
        out.contains("show(Member $user): ?Member"),
        "type hints\n{out}"
    );
    // new
    assert!(out.contains("new Member()"), "new\n{out}");
    // static call
    assert!(out.contains("Member::where("), "static call\n{out}");
    // ::class
    assert!(out.contains("Member::class"), "::class\n{out}");
    // instanceof
    assert!(out.contains("instanceof Member"), "instanceof\n{out}");
    // property type
    assert!(
        out.contains("public Member $current"),
        "property type\n{out}"
    );
    // unrelated import untouched
    assert!(
        out.contains("use App\\Models\\Post;"),
        "Post untouched\n{out}"
    );
    // the controller class name itself is untouched
    assert!(out.contains("class UserController extends Controller"));
}

// ---- alias safety ---------------------------------------------------------

#[test]
fn aliased_import_rewrites_only_the_class_segment() {
    let src = r#"<?php
namespace App\Http;
use App\Models\User as Account;
class C {
    public function f(Account $a): Account { return new Account(); }
}
"#;
    let out = rename(src, "App\\Models\\User", "User", "Member");
    // The import's class segment changes; the alias `Account` stays everywhere.
    assert!(out.contains("use App\\Models\\Member as Account;"), "{out}");
    assert!(
        out.contains("f(Account $a): Account"),
        "alias usages unchanged\n{out}"
    );
    assert!(out.contains("new Account()"), "{out}");
    assert!(!out.contains("Member as Member"));
}

// ---- fully-qualified references -------------------------------------------

#[test]
fn renames_fully_qualified_references() {
    let src = r#"<?php
namespace App\Jobs;
class J {
    public function f() {
        return \App\Models\User::query()->get();
    }
}
"#;
    let out = rename(src, "App\\Models\\User", "User", "Member");
    assert!(out.contains("\\App\\Models\\Member::query()"), "{out}");
}

// ---- precision: same-named member must NOT be touched ---------------------

#[test]
fn does_not_touch_method_or_property_named_like_class() {
    let src = r#"<?php
namespace App\Http;
use App\Models\User;
class C {
    public function f($obj) {
        $obj->User();       // method call named User
        $x = $obj->User;    // property named User
        return User::find(1); // real class ref
    }
}
"#;
    let spans = reference_spans(src, "App\\Models\\User", "User");
    // Exactly two: the `use` import and the `User::find` static call.
    assert_eq!(spans.len(), 2, "only real class refs: {spans:?}");
    let out = apply(src, &spans, "Member");
    assert!(out.contains("$obj->User()"), "method untouched\n{out}");
    assert!(
        out.contains("$x = $obj->User;"),
        "property untouched\n{out}"
    );
    assert!(out.contains("Member::find(1)"), "{out}");
}

// ---- same-namespace bare references (no import needed) --------------------

#[test]
fn renames_same_namespace_bare_reference() {
    let src = r#"<?php
namespace App\Models;
class Post extends Model {
    public function author() { return $this->belongsTo(User::class); }
    public function owner(): User { return new User(); }
}
"#;
    let out = rename(src, "App\\Models\\User", "User", "Member");
    assert!(out.contains("belongsTo(Member::class)"), "{out}");
    assert!(out.contains("owner(): Member"), "{out}");
    assert!(out.contains("new Member()"), "{out}");
    // Post itself untouched.
    assert!(out.contains("class Post extends Model"));
}

// ---- docblocks ------------------------------------------------------------

#[test]
fn renames_docblock_type_references() {
    let src = r#"<?php
namespace App\Http;
use App\Models\User;
class C {
    /**
     * @param User $user
     * @return User|null
     */
    public function f($user) { return $user; }
}
"#;
    let out = rename(src, "App\\Models\\User", "User", "Member");
    assert!(out.contains("@param Member $user"), "docblock param\n{out}");
    assert!(
        out.contains("@return Member|null"),
        "docblock return\n{out}"
    );
}

// ---- class_at_cursor (prepare_rename) -------------------------------------

#[test]
fn cursor_on_static_call_resolves_class() {
    let src = "<?php\nnamespace App\\Http;\nuse App\\Models\\User;\n$x = User::where('id', 1);\n";
    let byte = src.find("User::").unwrap() + 1; // inside `User`
    let (fqcn, span) = class_at_cursor(src, byte).expect("class at cursor");
    assert_eq!(fqcn, "App\\Models\\User");
    assert_eq!(&src[span.0..span.1], "User");
}

#[test]
fn cursor_on_declaration_resolves_class() {
    let src = "<?php\nnamespace App\\Models;\nclass User extends Model {}\n";
    let byte = src.find("class User").unwrap() + "class U".len();
    let (fqcn, _span) = class_at_cursor(src, byte).expect("class at cursor");
    assert_eq!(fqcn, "App\\Models\\User");
}

#[test]
fn cursor_on_alias_returns_none() {
    // Cursor on the alias token `Account` — not a real class name, so no rename.
    let src = "<?php\nuse App\\Models\\User as Account;\n$x = Account::find(1);\n";
    let byte = src.find("Account::").unwrap() + 1;
    assert!(class_at_cursor(src, byte).is_none());
}

#[test]
fn cursor_off_any_class_returns_none() {
    let src = "<?php\n$x = 1 + 2;\n";
    assert!(class_at_cursor(src, 8).is_none());
}

// ---- project file enumeration ---------------------------------------------

#[test]
fn project_php_files_skips_vendor_and_finds_app() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("app/Models")).unwrap();
    std::fs::create_dir_all(root.join("routes")).unwrap();
    std::fs::create_dir_all(root.join("vendor/laravel/framework/src")).unwrap();
    std::fs::write(root.join("app/Models/User.php"), "<?php").unwrap();
    std::fs::write(root.join("routes/web.php"), "<?php").unwrap();
    std::fs::write(root.join("vendor/laravel/framework/src/Model.php"), "<?php").unwrap();

    let files = project_php_files(root);
    assert!(files.iter().any(|p| p.ends_with("app/Models/User.php")));
    assert!(
        !files.iter().any(|p| p.to_string_lossy().contains("vendor")),
        "vendor must be pruned: {files:?}"
    );
}

#[test]
fn is_dependency_path_flags_vendor() {
    assert!(is_dependency_path(Path::new(
        "/proj/vendor/laravel/framework/src/Model.php"
    )));
    assert!(!is_dependency_path(Path::new("/proj/app/Models/User.php")));
}
