use super::{
    class_at_cursor, is_dependency_path, project_php_files, reference_spans, renamed_file_path,
};
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

// ---- file move target -----------------------------------------------------

#[test]
fn renamed_file_path_swaps_basename_same_dir_for_every_kind() {
    // The declaring file moves within its own directory, basename swapped,
    // `.php` preserved — identical rule for every class kind (PSR-4: one class
    // per file, basename == class basename).
    let cases = [
        (
            "/proj/app/Http/Controllers/UserController.php",
            "AdminController",
            "/proj/app/Http/Controllers/AdminController.php",
        ),
        (
            "/proj/app/Jobs/SendWelcomeEmail.php",
            "SendGreeting",
            "/proj/app/Jobs/SendGreeting.php",
        ),
        (
            "/proj/app/Services/PaymentService.php",
            "BillingService",
            "/proj/app/Services/BillingService.php",
        ),
        (
            "/proj/app/Http/Requests/StorePostRequest.php",
            "CreatePostRequest",
            "/proj/app/Http/Requests/CreatePostRequest.php",
        ),
        // Regression guard: models follow the very same rule.
        (
            "/proj/app/Models/User.php",
            "Customer",
            "/proj/app/Models/Customer.php",
        ),
    ];
    for (decl, new_basename, expected) in cases {
        assert_eq!(
            renamed_file_path(Path::new(decl), new_basename),
            Path::new(expected),
            "rename {decl} → {new_basename}"
        );
    }
}

// ---- controllers ----------------------------------------------------------

#[test]
fn renames_controller_class_declaration_and_route_references() {
    // Declaration site: `class UserController extends Controller`.
    let decl = r#"<?php
namespace App\Http\Controllers;
class UserController extends Controller
{
    public function index() {}
}
"#;
    let out = rename(
        decl,
        "App\\Http\\Controllers\\UserController",
        "UserController",
        "AdminController",
    );
    assert!(
        out.contains("class AdminController extends Controller"),
        "declaration\n{out}"
    );
    // The base `Controller` is untouched.
    assert!(out.contains("extends Controller"), "base class\n{out}");

    // Consumer site: a routes file referencing the controller via `::class`.
    let routes = r#"<?php
use App\Http\Controllers\UserController;
Route::get('/users', [UserController::class, 'index']);
Route::resource('users', UserController::class);
"#;
    let out = rename(
        routes,
        "App\\Http\\Controllers\\UserController",
        "UserController",
        "AdminController",
    );
    assert!(
        out.contains("use App\\Http\\Controllers\\AdminController;"),
        "use import\n{out}"
    );
    assert!(
        out.contains("[AdminController::class, 'index']"),
        "route array action\n{out}"
    );
    assert!(
        out.contains("Route::resource('users', AdminController::class)"),
        "resource action\n{out}"
    );
}

#[test]
fn cursor_on_controller_declaration_resolves_class() {
    let src =
        "<?php\nnamespace App\\Http\\Controllers;\nclass UserController extends Controller {}\n";
    let byte = src.find("class UserController").unwrap() + "class User".len();
    let (fqcn, _span) = class_at_cursor(src, byte).expect("class at cursor");
    assert_eq!(fqcn, "App\\Http\\Controllers\\UserController");
}

// ---- jobs -----------------------------------------------------------------

#[test]
fn renames_job_class_declaration_and_dispatch_references() {
    let decl = r#"<?php
namespace App\Jobs;
class SendWelcomeEmail implements ShouldQueue
{
    public function handle() {}
}
"#;
    let out = rename(
        decl,
        "App\\Jobs\\SendWelcomeEmail",
        "SendWelcomeEmail",
        "SendGreeting",
    );
    assert!(
        out.contains("class SendGreeting implements ShouldQueue"),
        "declaration\n{out}"
    );

    // Consumer: dispatch via static call, `new`, and `dispatch(new …)`.
    let consumer = r#"<?php
namespace App\Http\Controllers;
use App\Jobs\SendWelcomeEmail;
class RegisterController extends Controller {
    public function store() {
        SendWelcomeEmail::dispatch($user);
        dispatch(new SendWelcomeEmail($user));
        $job = new SendWelcomeEmail($user);
        return $job;
    }
}
"#;
    let out = rename(
        consumer,
        "App\\Jobs\\SendWelcomeEmail",
        "SendWelcomeEmail",
        "SendGreeting",
    );
    assert!(
        out.contains("use App\\Jobs\\SendGreeting;"),
        "use import\n{out}"
    );
    assert!(
        out.contains("SendGreeting::dispatch($user)"),
        "static dispatch\n{out}"
    );
    assert_eq!(
        out.matches("new SendGreeting($user)").count(),
        2,
        "both `new` sites\n{out}"
    );
    // The enclosing controller is untouched.
    assert!(out.contains("class RegisterController extends Controller"));
}

// ---- services -------------------------------------------------------------

#[test]
fn renames_service_class_declaration_and_injection_references() {
    let decl = r#"<?php
namespace App\Services;
class PaymentService
{
    public function charge() {}
}
"#;
    let out = rename(
        decl,
        "App\\Services\\PaymentService",
        "PaymentService",
        "BillingService",
    );
    assert!(out.contains("class BillingService"), "declaration\n{out}");

    // Consumer: constructor-injected + method type-hint + return type + `new`.
    let consumer = r#"<?php
namespace App\Http\Controllers;
use App\Services\PaymentService;
class CheckoutController extends Controller {
    public function __construct(private PaymentService $payments) {}
    public function build(PaymentService $service): PaymentService {
        return new PaymentService();
    }
}
"#;
    let out = rename(
        consumer,
        "App\\Services\\PaymentService",
        "PaymentService",
        "BillingService",
    );
    assert!(
        out.contains("use App\\Services\\BillingService;"),
        "use import\n{out}"
    );
    assert!(
        out.contains("__construct(private BillingService $payments)"),
        "constructor injection\n{out}"
    );
    assert!(
        out.contains("build(BillingService $service): BillingService"),
        "param + return type\n{out}"
    );
    assert!(out.contains("new BillingService()"), "new\n{out}");
    assert!(out.contains("class CheckoutController extends Controller"));
}

// ---- form requests --------------------------------------------------------

#[test]
fn renames_form_request_class_declaration_and_type_hint_references() {
    let decl = r#"<?php
namespace App\Http\Requests;
class StorePostRequest extends FormRequest
{
    public function rules(): array { return []; }
}
"#;
    let out = rename(
        decl,
        "App\\Http\\Requests\\StorePostRequest",
        "StorePostRequest",
        "CreatePostRequest",
    );
    assert!(
        out.contains("class CreatePostRequest extends FormRequest"),
        "declaration\n{out}"
    );

    // Consumer: a controller type-hinting the request as an action argument,
    // plus a docblock reference.
    let consumer = r#"<?php
namespace App\Http\Controllers;
use App\Http\Requests\StorePostRequest;
class PostController extends Controller {
    public function store(StorePostRequest $request) {
        return $request->validated();
    }
    /**
     * @param StorePostRequest $request
     */
    public function update(StorePostRequest $request) {}
}
"#;
    let out = rename(
        consumer,
        "App\\Http\\Requests\\StorePostRequest",
        "StorePostRequest",
        "CreatePostRequest",
    );
    assert!(
        out.contains("use App\\Http\\Requests\\CreatePostRequest;"),
        "use import\n{out}"
    );
    assert!(
        out.contains("store(CreatePostRequest $request)"),
        "action type hint\n{out}"
    );
    assert!(
        out.contains("update(CreatePostRequest $request)"),
        "second type hint\n{out}"
    );
    assert!(
        out.contains("@param CreatePostRequest $request"),
        "docblock\n{out}"
    );
    assert!(out.contains("class PostController extends Controller"));
}

#[test]
fn cursor_on_job_static_dispatch_resolves_class() {
    let src = "<?php\nnamespace App\\Http\\Controllers;\nuse App\\Jobs\\SendWelcomeEmail;\nSendWelcomeEmail::dispatch($u);\n";
    let byte = src.find("SendWelcomeEmail::dispatch").unwrap() + 1; // inside the class name
    let (fqcn, span) = class_at_cursor(src, byte).expect("class at cursor");
    assert_eq!(fqcn, "App\\Jobs\\SendWelcomeEmail");
    assert_eq!(&src[span.0..span.1], "SendWelcomeEmail");
}
