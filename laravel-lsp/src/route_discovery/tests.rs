use super::*;

#[test]
fn extracts_single_quoted_route_name() {
    let src = r#"<?php
Route::get('/login', [LoginController::class, 'show'])->name('login');
"#;
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);

    assert_eq!(results.len(), 1);
    let (name, def) = &results[0];
    assert_eq!(name.as_deref(), Some("login"));
    assert_eq!(def.line, 1);
    assert_eq!(def.priority, PRIORITY_APP);
}

#[test]
fn extracts_double_quoted_route_name() {
    let src = r#"<?php
Route::get('/dashboard')->name("dashboard.index");
"#;
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0.as_deref(), Some("dashboard.index"));
}

#[test]
fn extracts_multiple_routes_per_file() {
    let src = r#"<?php
Route::get('/login')->name('login');
Route::post('/logout')->name('logout');
Route::get('/register')->name('register');
"#;
    let path = PathBuf::from("/fake/routes/auth.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);

    let names: Vec<&str> = results.iter().filter_map(|(n, _)| n.as_deref()).collect();
    assert_eq!(names, vec!["login", "logout", "register"]);
}

#[test]
fn tolerates_whitespace_in_call() {
    let src = "<?php\nRoute::get('/x')->name ( 'spaced' );\n";
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0.as_deref(), Some("spaced"));
}

#[test]
fn skips_variable_route_names() {
    let src = "<?php\nRoute::get('/x')->name($name);\n";
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);
    assert!(results.is_empty(), "should skip variable name arguments");
}

#[test]
fn extracts_routes_inside_macro_body() {
    // Models the Laravel UI AuthRouteMethods pattern.
    let src = r#"<?php
class AuthRouteMethods
{
public function auth()
{
    return function () {
        $this->get('login', ...)->name('login');
        $this->post('logout', ...)->name('logout');
    };
}
}
"#;
    let path = PathBuf::from("/fake/vendor/laravel/ui/src/AuthRouteMethods.php");
    let results = extract_named_routes(src, &path, PRIORITY_PACKAGE, &[]);

    let names: Vec<&str> = results.iter().filter_map(|(n, _)| n.as_deref()).collect();
    assert_eq!(names, vec!["login", "logout"]);
}

#[test]
fn route_index_resolves_priority_collision() {
    let mut idx = RouteIndex::new();

    idx.insert(
        "login".into(),
        RouteDefinition {
            file: PathBuf::from("/fake/vendor/laravel/fortify/routes/routes.php"),
            line: 5,
            column: 0,
            end_column: 10,
            priority: PRIORITY_PACKAGE,
            method: None,
            uri: None,
            action: None,
        },
    );
    idx.insert(
        "login".into(),
        RouteDefinition {
            file: PathBuf::from("/fake/routes/auth.php"),
            line: 12,
            column: 0,
            end_column: 10,
            priority: PRIORITY_APP,
            method: None,
            uri: None,
            action: None,
        },
    );

    let def = idx.get("login").expect("should resolve");
    assert!(
        def.file.ends_with("routes/auth.php"),
        "app should win over package"
    );
    assert_eq!(def.priority, PRIORITY_APP);
}

#[test]
fn route_index_keeps_lower_when_higher_does_not_redefine() {
    let mut idx = RouteIndex::new();
    idx.insert(
        "horizon.index".into(),
        RouteDefinition {
            file: PathBuf::from("/fake/vendor/laravel/horizon/routes/web.php"),
            line: 3,
            column: 0,
            end_column: 10,
            priority: PRIORITY_PACKAGE,
            method: None,
            uri: None,
            action: None,
        },
    );
    let def = idx
        .get("horizon.index")
        .expect("package route should index");
    assert_eq!(def.priority, PRIORITY_PACKAGE);
}

#[test]
fn file_registers_named_routes_detects_macro_file() {
    let dir = std::env::temp_dir().join("laravel-lsp-route-test");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("AuthRouteMethods.php");
    let src = "<?php\nclass X {\n  public function auth() {\n    return function () {\n      $this->get('login')->name('login');\n    };\n  }\n}\n";
    std::fs::write(&path, src).unwrap();

    assert!(file_registers_named_routes(&path));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn content_registers_named_routes_via_verb_method_call() {
    // Laravel UI's AuthRouteMethods style — uses `$this->get(...)->name(...)`
    // with no `Route::` token at all.
    let src = r#"<?php
$this->get('login')->name('login');
$this->post('logout')->name('logout');
"#;
    assert!(content_registers_named_routes(src));
}

#[test]
fn content_registers_named_routes_via_route_facade() {
    let src = "<?php\nRoute::get('/x')->name('x');\n";
    assert!(content_registers_named_routes(src));
}

#[test]
fn content_registers_named_routes_rejects_no_name_call() {
    // `->name(` is required regardless of other tokens.
    let src = "<?php\nRoute::get('/x', [Controller::class, 'index']);\n";
    assert!(!content_registers_named_routes(src));
}

#[test]
fn content_registers_named_routes_rejects_only_name_calls() {
    // `->name(` alone (e.g., builder DSL with no routing context) is not
    // sufficient. We require some route-shape token.
    let src = "<?php\n$builder->name('foo');\n";
    assert!(!content_registers_named_routes(src));
}

#[test]
fn file_registers_named_routes_rejects_unrelated_php() {
    let dir = std::env::temp_dir().join("laravel-lsp-route-test-2");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("Plain.php");
    std::fs::write(&path, "<?php\nclass Plain { public $name = 'x'; }\n").unwrap();

    assert!(!file_registers_named_routes(&path));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn is_under_routes_dir_recognizes_package_layout() {
    assert!(is_under_routes_dir(Path::new(
        "/project/vendor/laravel/fortify/routes/routes.php"
    )));
    assert!(is_under_routes_dir(Path::new("/project/routes/auth.php")));
    assert!(!is_under_routes_dir(Path::new(
        "/project/vendor/foo/src/Http/Controllers.php"
    )));
}

#[test]
fn priority_for_vendor_path_distinguishes_framework() {
    assert_eq!(
        priority_for_vendor_path(Path::new(
            "/project/vendor/laravel/framework/src/Illuminate/Auth.php"
        )),
        PRIORITY_FRAMEWORK
    );
    assert_eq!(
        priority_for_vendor_path(Path::new(
            "/project/vendor/laravel/fortify/routes/routes.php"
        )),
        PRIORITY_PACKAGE
    );
}

// ============================================================================
// Route metadata extraction (method / URI / action)
// ============================================================================

/// Convenience: run a full extraction over `src` and return the first
/// `RouteDefinition`. Tests are about extraction shape, not file paths.
fn first_def(src: &str) -> RouteDefinition {
    let path = PathBuf::from("/fake/routes/web.php");
    let mut results = extract_named_routes(src, &path, PRIORITY_APP, &[]);
    assert!(
        !results.is_empty(),
        "expected at least one route definition"
    );
    results.remove(0).1
}

#[test]
fn metadata_extracts_array_action() {
    let def = first_def(
        "<?php\nRoute::get('/users', [UserController::class, 'show'])->name('users.show');\n",
    );
    assert_eq!(def.method.as_deref(), Some("get"));
    assert_eq!(def.uri.as_deref(), Some("/users"));
    assert_eq!(def.action.as_deref(), Some("UserController@show"));
}

#[test]
fn metadata_extracts_legacy_string_action() {
    let def =
        first_def("<?php\nRoute::post('/login', 'LoginController@authenticate')->name('login');\n");
    assert_eq!(def.method.as_deref(), Some("post"));
    assert_eq!(def.uri.as_deref(), Some("/login"));
    assert_eq!(def.action.as_deref(), Some("LoginController@authenticate"));
}

#[test]
fn metadata_extracts_invokable_action() {
    let def =
        first_def("<?php\nRoute::get('/dashboard', DashboardController::class)->name('dash');\n");
    assert_eq!(def.method.as_deref(), Some("get"));
    assert_eq!(def.uri.as_deref(), Some("/dashboard"));
    assert_eq!(def.action.as_deref(), Some("DashboardController"));
}

#[test]
fn metadata_extracts_closure_action() {
    let def = first_def(
        "<?php\nRoute::get('/closure', function () { return 'hi'; })->name('closure');\n",
    );
    assert_eq!(def.method.as_deref(), Some("get"));
    assert_eq!(def.uri.as_deref(), Some("/closure"));
    assert_eq!(def.action.as_deref(), Some("Closure"));
}

#[test]
fn metadata_extracts_arrow_function_action() {
    let def = first_def("<?php\nRoute::get('/arrow', fn() => 'hi')->name('arrow');\n");
    assert_eq!(def.action.as_deref(), Some("Closure"));
}

#[test]
fn metadata_handles_namespaced_controller_in_array() {
    let def = first_def(
        "<?php\nRoute::get('/x', [\\App\\Http\\Controllers\\UserController::class, 'index'])->name('x');\n",
    );
    assert_eq!(def.action.as_deref(), Some("UserController@index"));
}

#[test]
fn metadata_handles_view_route_without_action() {
    let def = first_def("<?php\nRoute::view('/static', 'view.name')->name('view');\n");
    assert_eq!(def.method.as_deref(), Some("view"));
    assert_eq!(def.uri.as_deref(), Some("/static"));
    // 'view.name' is the second argument — but in Route::view, it's a view name,
    // not an action. We pass it through as a string for now; consumers can decide.
    assert_eq!(def.action.as_deref(), Some("view.name"));
}

#[test]
fn metadata_handles_redirect_route_without_action() {
    let def = first_def("<?php\nRoute::redirect('/from', '/to')->name('redir');\n");
    assert_eq!(def.method.as_deref(), Some("redirect"));
    assert_eq!(def.uri.as_deref(), Some("/from"));
    assert_eq!(def.action.as_deref(), Some("/to"));
}

#[test]
fn metadata_handles_chained_middleware_before_name() {
    let def = first_def(
        "<?php\nRoute::get('/admin', [AdminController::class, 'index'])->middleware('auth')->name('admin');\n",
    );
    assert_eq!(def.method.as_deref(), Some("get"));
    assert_eq!(def.uri.as_deref(), Some("/admin"));
    assert_eq!(def.action.as_deref(), Some("AdminController@index"));
}

#[test]
fn metadata_handles_multiline_route_declaration() {
    let src = r#"<?php
Route::get(
    '/users/{user}',
    [UserController::class, 'show'],
)->name('users.show');
"#;
    let def = first_def(src);
    assert_eq!(def.method.as_deref(), Some("get"));
    assert_eq!(def.uri.as_deref(), Some("/users/{user}"));
    assert_eq!(def.action.as_deref(), Some("UserController@show"));
}

#[test]
fn metadata_handles_this_router_macro_style() {
    let src = r#"<?php
$this->get('login', [LoginController::class, 'show'])->name('login');
"#;
    let def = first_def(src);
    assert_eq!(def.method.as_deref(), Some("get"));
    assert_eq!(def.uri.as_deref(), Some("login"));
    assert_eq!(def.action.as_deref(), Some("LoginController@show"));
}

#[test]
fn metadata_skips_unrelated_verb_calls_in_other_statements() {
    // Two statements; the first contains `Route::post(...)`, the second contains
    // `->name('x')`. They must not bleed into each other.
    let src = r#"<?php
Route::post('/wrong', [Wrong::class, 'wrong']);
Route::get('/right', [Right::class, 'right'])->name('right');
"#;
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);
    assert_eq!(results.len(), 1);
    let def = &results[0].1;
    assert_eq!(def.method.as_deref(), Some("get"));
    assert_eq!(def.uri.as_deref(), Some("/right"));
    assert_eq!(def.action.as_deref(), Some("Right@right"));
}

#[test]
fn metadata_returns_none_when_verb_call_missing() {
    // A `->name(` callsite without any verb call upstream — e.g. someone building
    // routes through a builder we don't recognise.
    let src = "<?php\n$builder->name('orphan');\n";
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);
    // content_registers_named_routes filters this in the discovery pipeline, but
    // the raw extractor still surfaces the name with empty metadata.
    assert_eq!(results.len(), 1);
    let def = &results[0].1;
    assert!(def.method.is_none());
    assert!(def.uri.is_none());
    assert!(def.action.is_none());
}

#[test]
fn metadata_does_not_match_longer_verb_lookalike() {
    // `->getUser(...)` should not match the `get` verb. The extractor must
    // enforce a word boundary after the verb.
    let src = "<?php\n$obj->getUser('/x', SomeController::class)->name('user');\n";
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);
    assert_eq!(results.len(), 1);
    let def = &results[0].1;
    assert!(
        def.method.is_none(),
        "verb match must require a word boundary"
    );
}

// ============================================================================
// Route group name compositing
// ============================================================================

#[test]
fn route_group_chain_name_prefixes_child_route() {
    let src = r#"<?php
Route::name('admin.')->group(function () {
    Route::get('/users', [UserController::class, 'index'])->name('users.index');
});
"#;
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);
    let names: Vec<&str> = results.iter().filter_map(|(n, _)| n.as_deref()).collect();
    assert_eq!(names, vec!["admin.users.index"]);
}

#[test]
fn route_group_array_as_prefixes_child_route() {
    let src = r#"<?php
Route::group(['as' => 'api.', 'prefix' => 'api'], function () {
    Route::get('/users', [UserController::class, 'index'])->name('users.index');
});
"#;
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);
    let names: Vec<&str> = results.iter().filter_map(|(n, _)| n.as_deref()).collect();
    assert_eq!(names, vec!["api.users.index"]);
}

#[test]
fn route_group_chain_handles_intervening_middleware_call() {
    // `->middleware(...)` sits between `->name('admin.')` and `->group(...)`.
    // The backward chain walk must step over middleware() and still find name().
    let src = r#"<?php
Route::name('admin.')->middleware(['auth', 'verified'])->group(function () {
    Route::get('/users', [UserController::class, 'index'])->name('users.index');
});
"#;
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);
    let names: Vec<&str> = results.iter().filter_map(|(n, _)| n.as_deref()).collect();
    assert_eq!(names, vec!["admin.users.index"]);
}

#[test]
fn route_group_nested_two_levels() {
    let src = r#"<?php
Route::name('api.')->group(function () {
    Route::name('v1.')->group(function () {
        Route::get('/users', [UserController::class, 'index'])->name('users.index');
    });
});
"#;
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);
    let names: Vec<&str> = results.iter().filter_map(|(n, _)| n.as_deref()).collect();
    assert_eq!(names, vec!["api.v1.users.index"]);
}

#[test]
fn route_group_nested_five_levels_real_world() {
    // Modelled on the case Mike reported during manual testing:
    // `decision-cloud.lead-settings.management-systems.decisioner-settings.edit`
    let src = r#"<?php
Route::name('decision-cloud.')->group(function () {
    Route::name('lead-settings.')->group(function () {
        Route::name('management-systems.')->group(function () {
            Route::name('decisioner-settings.')->group(function () {
                Route::get('/edit', [Controller::class, 'edit'])->name('edit');
            });
        });
    });
});
"#;
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);
    let names: Vec<&str> = results.iter().filter_map(|(n, _)| n.as_deref()).collect();
    assert_eq!(
        names,
        vec!["decision-cloud.lead-settings.management-systems.decisioner-settings.edit"]
    );
}

#[test]
fn route_group_mixed_chain_and_array_styles() {
    let src = r#"<?php
Route::name('api.')->group(function () {
    Route::group(['as' => 'v1.'], function () {
        Route::get('/users', ...)->name('users.index');
    });
});
"#;
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);
    let names: Vec<&str> = results.iter().filter_map(|(n, _)| n.as_deref()).collect();
    assert_eq!(names, vec!["api.v1.users.index"]);
}

#[test]
fn route_group_does_not_prefix_routes_outside_body() {
    let src = r#"<?php
Route::name('admin.')->group(function () {
    Route::get('/users')->name('users.index');
});
Route::get('/login')->name('login');
"#;
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);
    let names: Vec<&str> = results.iter().filter_map(|(n, _)| n.as_deref()).collect();
    assert_eq!(
        names,
        vec!["admin.users.index", "login"],
        "route outside the group must NOT be prefixed"
    );
}

#[test]
fn route_group_without_prefix_does_not_contribute() {
    // A group with no `->name(...)` and no array `'as'` — it's a valid
    // grouping but doesn't affect child route names.
    let src = r#"<?php
Route::middleware('auth')->group(function () {
    Route::get('/dashboard')->name('dashboard');
});
"#;
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);
    let names: Vec<&str> = results.iter().filter_map(|(n, _)| n.as_deref()).collect();
    assert_eq!(names, vec!["dashboard"]);
}

#[test]
fn route_group_sibling_groups_do_not_cross_pollinate() {
    let src = r#"<?php
Route::name('admin.')->group(function () {
    Route::get('/x')->name('x');
});
Route::name('api.')->group(function () {
    Route::get('/y')->name('y');
});
"#;
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);
    let names: Vec<&str> = results.iter().filter_map(|(n, _)| n.as_deref()).collect();
    assert_eq!(names, vec!["admin.x", "api.y"]);
}

// ============================================================================
// External-file route group loads (issue #43)
// ============================================================================
//
// `Route::as('admin.')->group(base_path('routes/web_backstage.php'))` loads an
// external file and applies the group's name prefix to every route inside it.
// These tests exercise `build_route_index` end-to-end over a temp directory,
// because the load resolution + transitive propagation only happen there.

use tempfile::TempDir;

/// Build a `routes/` tree from `(relative_path, contents)` pairs under a fresh
/// temp dir, then run `build_route_index` over the project routes. Returns the
/// temp dir (kept alive) and the resulting index.
fn index_routes(files: &[(&str, &str)]) -> (TempDir, RouteIndex) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let mut route_files = Vec::new();
    for (rel, contents) in files {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, contents).unwrap();
        route_files.push(RouteFile {
            path,
            priority: PRIORITY_APP,
        });
    }
    let index = build_route_index(root, &route_files);
    (tmp, index)
}

#[test]
fn external_group_base_path_applies_prefix() {
    let (_tmp, index) = index_routes(&[
        (
            "routes/web.php",
            "<?php\nRoute::as('admin.')->group(base_path('routes/web_backstage.php'));\n",
        ),
        (
            "routes/web_backstage.php",
            "<?php\nRoute::get('/patients', fn () => 'ok')->name('patient.index');\n",
        ),
    ]);

    // Both the prefixed AND the bare name must be present — the loaded file is
    // also scanned directly, so its bare leaf stays in the index.
    assert!(
        index.get("admin.patient.index").is_some(),
        "external group prefix must produce admin.patient.index"
    );
    assert!(
        index.get("patient.index").is_some(),
        "bare leaf name must remain in the index"
    );
}

#[test]
fn external_group_dir_concat_applies_prefix() {
    let (_tmp, index) = index_routes(&[
        (
            "routes/web.php",
            "<?php\nRoute::as('admin.')->group(__DIR__ . '/web_backstage.php');\n",
        ),
        (
            "routes/web_backstage.php",
            "<?php\nRoute::get('/patients', fn () => 'ok')->name('patient.index');\n",
        ),
    ]);

    assert!(
        index.get("admin.patient.index").is_some(),
        "__DIR__ . '/file' load form must resolve and apply prefix"
    );
}

#[test]
fn external_group_nested_closure_prefix_combines() {
    // An enclosing CLOSURE group `admin.` wraps a `->as('v1.')->group($file)`
    // external load. The loaded file's routes get `admin.v1.`.
    let (_tmp, index) = index_routes(&[
        (
            "routes/web.php",
            "<?php\nRoute::as('admin.')->group(function () {\n    Route::as('v1.')->group(base_path('routes/web_backstage.php'));\n});\n",
        ),
        (
            "routes/web_backstage.php",
            "<?php\nRoute::get('/patients', fn () => 'ok')->name('patient.index');\n",
        ),
    ]);

    assert!(
        index.get("admin.v1.patient.index").is_some(),
        "enclosing closure prefix must combine with the load's own prefix"
    );
}

#[test]
fn external_group_transitive_chain_of_loads() {
    // a.php --admin.--> b.php --patient.--> c.php
    let (_tmp, index) = index_routes(&[
        (
            "routes/a.php",
            "<?php\nRoute::as('admin.')->group(base_path('routes/b.php'));\n",
        ),
        (
            "routes/b.php",
            "<?php\nRoute::as('patient.')->group(base_path('routes/c.php'));\n",
        ),
        (
            "routes/c.php",
            "<?php\nRoute::get('/edit', fn () => 'ok')->name('edit');\n",
        ),
    ]);

    assert!(
        index.get("admin.patient.edit").is_some(),
        "transitive load chain must accumulate prefixes"
    );
}

#[test]
fn external_group_array_form_applies_prefix() {
    // Array form: the file path is the SECOND argument; the first is the array.
    let (_tmp, index) = index_routes(&[
        (
            "routes/web.php",
            "<?php\nRoute::group(['as' => 'admin.'], base_path('routes/web_backstage.php'));\n",
        ),
        (
            "routes/web_backstage.php",
            "<?php\nRoute::get('/patients', fn () => 'ok')->name('patient.index');\n",
        ),
    ]);

    assert!(
        index.get("admin.patient.index").is_some(),
        "array-form external group must apply the 'as' prefix"
    );
}

#[test]
fn external_prefixes_for_file_returns_inherited_prefix() {
    // `routes/web.php` loads `routes/web_backstage.php` via an `admin.` group.
    // `external_prefixes_for_file` must return `["", "admin."]` for the loaded
    // file — discovered through `discover_route_files` + the load graph.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let routes = root.join("routes");
    std::fs::create_dir_all(&routes).unwrap();
    std::fs::write(
        routes.join("web.php"),
        "<?php\nRoute::as('admin.')->group(base_path('routes/web_backstage.php'));\n",
    )
    .unwrap();
    let backstage = routes.join("web_backstage.php");
    std::fs::write(
        &backstage,
        "<?php\nRoute::get('/patients', fn () => 'ok')->name('patient.index');\n",
    )
    .unwrap();

    let prefixes = external_prefixes_for_file(root, &backstage);
    assert!(
        prefixes.contains(&String::new()),
        "the empty prefix is always present (file scanned directly)"
    );
    assert!(
        prefixes.contains(&"admin.".to_string()),
        "the loaded file inherits the loader group's `admin.` prefix, got {prefixes:?}"
    );
    assert_eq!(
        prefixes.len(),
        2,
        "exactly `\"\"` and `admin.`: {prefixes:?}"
    );
}

#[test]
fn external_prefixes_for_file_loader_has_only_empty() {
    // The LOADER file (web.php) inherits no external prefix itself — only the
    // always-present empty prefix.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let routes = root.join("routes");
    std::fs::create_dir_all(&routes).unwrap();
    let web = routes.join("web.php");
    std::fs::write(
        &web,
        "<?php\nRoute::as('admin.')->group(base_path('routes/web_backstage.php'));\n",
    )
    .unwrap();
    std::fs::write(
        routes.join("web_backstage.php"),
        "<?php\nRoute::get('/patients', fn () => 'ok')->name('patient.index');\n",
    )
    .unwrap();

    let prefixes = external_prefixes_for_file(root, &web);
    assert_eq!(
        prefixes,
        vec![String::new()],
        "loader has no inherited prefix"
    );
}

#[test]
fn external_prefixes_for_file_transitive_chain_accumulates() {
    // a.php --admin.--> b.php --patient.--> c.php. The deepest file inherits the
    // accumulated `admin.patient.` prefix.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let routes = root.join("routes");
    std::fs::create_dir_all(&routes).unwrap();
    std::fs::write(
        routes.join("a.php"),
        "<?php\nRoute::as('admin.')->group(base_path('routes/b.php'));\n",
    )
    .unwrap();
    std::fs::write(
        routes.join("b.php"),
        "<?php\nRoute::as('patient.')->group(base_path('routes/c.php'));\n",
    )
    .unwrap();
    let c = routes.join("c.php");
    std::fs::write(
        &c,
        "<?php\nRoute::get('/edit', fn () => 'ok')->name('edit');\n",
    )
    .unwrap();

    let prefixes = external_prefixes_for_file(root, &c);
    assert!(prefixes.contains(&String::new()));
    assert!(
        prefixes.contains(&"admin.patient.".to_string()),
        "transitive chain must accumulate: {prefixes:?}"
    );
}

#[test]
fn external_group_cycle_is_guarded() {
    // a.php loads b.php; b.php loads a.php. Must terminate and still index
    // both files' bare names without blowing up.
    let (_tmp, index) = index_routes(&[
        (
            "routes/a.php",
            "<?php\nRoute::as('x.')->group(base_path('routes/b.php'));\nRoute::get('/a', fn () => 'ok')->name('a');\n",
        ),
        (
            "routes/b.php",
            "<?php\nRoute::as('y.')->group(base_path('routes/a.php'));\nRoute::get('/b', fn () => 'ok')->name('b');\n",
        ),
    ]);

    assert!(index.get("a").is_some(), "bare name a must index");
    assert!(index.get("b").is_some(), "bare name b must index");
    // One hop each direction is fine.
    assert!(index.get("x.b").is_some(), "a loads b with prefix x.");
    assert!(index.get("y.a").is_some(), "b loads a with prefix y.");
}

/// Like [`index_routes`] but lets callers place files at ARBITRARY relative
/// paths (e.g. `app/Custom/admin.php`) while still seeding the working set only
/// with the `routes/` entry points. Files NOT under `routes/` are written to
/// disk but NOT added to `route_files`, so they can only enter the index via a
/// transitive `->group(<path>)` load — exactly the issue #43 scenario.
fn index_with_entrypoints(files: &[(&str, &str)], entrypoints: &[&str]) -> (TempDir, RouteIndex) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let mut route_files = Vec::new();
    for (rel, contents) in files {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, contents).unwrap();
        if entrypoints.contains(rel) {
            route_files.push(RouteFile {
                path,
                priority: PRIORITY_APP,
            });
        }
    }
    let index = build_route_index(root, &route_files);
    (tmp, index)
}

#[test]
fn external_group_indexes_file_outside_routes_dir() {
    // The loaded file lives OUTSIDE routes/ (app/Custom/admin.php) and is NOT a
    // discovered entry point — it can only enter the index transitively.
    let (_tmp, index) = index_with_entrypoints(
        &[
            (
                "routes/web.php",
                "<?php\nRoute::as('admin.')->group(base_path('app/Custom/admin.php'));\n",
            ),
            (
                "app/Custom/admin.php",
                "<?php\nRoute::get('/dashboard', fn () => 'ok')->name('dash');\nRoute::resource('widgets', WidgetController::class);\n",
            ),
        ],
        &["routes/web.php"],
    );

    // The ->name('dash') leaf indexes under the load's admin. prefix...
    assert!(
        index.get("admin.dash").is_some(),
        "external file outside routes/ must index its ->name under the load prefix"
    );
    // ...and its bare leaf survives (file scanned directly too).
    assert!(
        index.get("dash").is_some(),
        "bare leaf name from the external file must remain"
    );
    // Resource routes in the external file also compose with the load prefix.
    for action in [
        "index", "create", "store", "show", "edit", "update", "destroy",
    ] {
        assert!(
            index.get(&format!("admin.widgets.{action}")).is_some(),
            "expected admin.widgets.{action} from external file"
        );
    }
}

#[test]
fn source_files_includes_referenced_external_file() {
    let (tmp, index) = index_with_entrypoints(
        &[
            (
                "routes/web.php",
                "<?php\nRoute::as('admin.')->group(base_path('app/Custom/admin.php'));\n",
            ),
            (
                "app/Custom/admin.php",
                "<?php\nRoute::get('/dashboard', fn () => 'ok')->name('dash');\n",
            ),
        ],
        &["routes/web.php"],
    );

    let root = tmp.path();
    let web = normalize_path(&root.join("routes/web.php"));
    let admin = normalize_path(&root.join("app/Custom/admin.php"));
    assert!(
        index.source_files.contains(&web),
        "source_files must contain the discovered routes/web.php"
    );
    assert!(
        index.source_files.contains(&admin),
        "source_files must contain the transitively-referenced app/Custom/admin.php"
    );
}

#[test]
fn external_group_transitive_chain_outside_routes_dir() {
    // a (entry, in routes/) --admin.--> b (outside) --patient.--> c (outside)
    let (tmp, index) = index_with_entrypoints(
        &[
            (
                "routes/a.php",
                "<?php\nRoute::as('admin.')->group(base_path('app/Custom/b.php'));\n",
            ),
            (
                "app/Custom/b.php",
                "<?php\nRoute::as('patient.')->group(base_path('app/Custom/c.php'));\n",
            ),
            (
                "app/Custom/c.php",
                "<?php\nRoute::get('/edit', fn () => 'ok')->name('edit');\n",
            ),
        ],
        &["routes/a.php"],
    );

    assert!(
        index.get("admin.patient.edit").is_some(),
        "transitive chain through files outside routes/ must accumulate prefixes"
    );

    let root = tmp.path();
    for rel in ["routes/a.php", "app/Custom/b.php", "app/Custom/c.php"] {
        let key = normalize_path(&root.join(rel));
        assert!(
            index.source_files.contains(&key),
            "source_files must contain {rel}"
        );
    }
}

#[test]
fn external_group_cycle_outside_routes_dir_terminates() {
    // a (entry) <-> b (outside): each loads the other. Must terminate and index
    // a finite set of names.
    let (_tmp, index) = index_with_entrypoints(
        &[
            (
                "routes/a.php",
                "<?php\nRoute::as('x.')->group(base_path('app/Custom/b.php'));\nRoute::get('/a', fn () => 'ok')->name('a');\n",
            ),
            (
                "app/Custom/b.php",
                "<?php\nRoute::as('y.')->group(base_path('routes/a.php'));\nRoute::get('/b', fn () => 'ok')->name('b');\n",
            ),
        ],
        &["routes/a.php"],
    );

    // Bare names from both files index.
    assert!(index.get("a").is_some(), "bare name a must index");
    assert!(index.get("b").is_some(), "bare name b must index");
    // One hop each direction.
    assert!(index.get("x.b").is_some(), "a loads b with prefix x.");
    assert!(index.get("y.a").is_some(), "b loads a with prefix y.");
    // Finite: the index can't have exploded into an unbounded name set.
    assert!(
        index.routes.len() < 50,
        "cycle must produce a finite, small name set, got {}",
        index.routes.len()
    );
}

// ============================================================================
// Resource route derivation (Route::resource / Route::apiResource)
// ============================================================================

/// Collect just the route names from a direct extraction over `src`.
fn names_of(src: &str) -> Vec<String> {
    let path = PathBuf::from("/fake/routes/web.php");
    extract_named_routes(src, &path, PRIORITY_APP, &[])
        .into_iter()
        .filter_map(|(n, _)| n)
        .collect()
}

#[test]
fn resource_inside_name_group_applies_prefix_and_strips_slash() {
    // `/leads` (leading slash) inside `Route::name('api.')->group(fn)` with an
    // `->only(['store', 'update'])` filter. Must yield `api.leads.store` and
    // `api.leads.update` — and NEVER a slash-prefixed `/leads.*` name.
    let src = r#"<?php
Route::name('api.')->group(function () {
    Route::resource('/leads', LeadController::class)->only(['store', 'update']);
});
"#;
    let names = names_of(src);
    assert!(
        names.contains(&"api.leads.store".to_string()),
        "expected api.leads.store, got {names:?}"
    );
    assert!(
        names.contains(&"api.leads.update".to_string()),
        "expected api.leads.update, got {names:?}"
    );
    // only() must drop the rest.
    assert!(!names.iter().any(|n| n.ends_with(".index")));
    assert!(!names.iter().any(|n| n.ends_with(".show")));
    // No name may carry a literal slash from the stripped resource URI.
    assert!(
        !names.iter().any(|n| n.contains('/')),
        "no name may contain a slash, got {names:?}"
    );
}

#[test]
fn api_resource_yields_five_actions_slash_free() {
    let src = r#"<?php
Route::apiResource('photos', PhotoController::class);
"#;
    let mut names = names_of(src);
    names.sort();
    assert_eq!(
        names,
        vec![
            "photos.destroy",
            "photos.index",
            "photos.show",
            "photos.store",
            "photos.update",
        ],
        "apiResource must register exactly the 5 api actions"
    );
    // create/edit are form routes — excluded from apiResource.
    assert!(!names.iter().any(|n| n == "photos.create"));
    assert!(!names.iter().any(|n| n == "photos.edit"));
}

#[test]
fn resource_except_filters_actions() {
    let src = r#"<?php
Route::resource('posts', PostController::class)->except(['create', 'edit', 'destroy']);
"#;
    let mut names = names_of(src);
    names.sort();
    assert_eq!(
        names,
        vec!["posts.index", "posts.show", "posts.store", "posts.update",]
    );
}

#[test]
fn resource_strips_trailing_slash() {
    let src = r#"<?php
Route::resource('photos/', PhotoController::class)->only(['index']);
"#;
    let names = names_of(src);
    assert_eq!(names, vec!["photos.index"]);
}

#[test]
fn resource_skips_non_string_first_arg() {
    // A variable resource URI can't be resolved statically — skip it entirely.
    let src = r#"<?php
Route::resource($name, PhotoController::class);
"#;
    let names = names_of(src);
    assert!(
        names.is_empty(),
        "non-literal resource URI must be skipped, got {names:?}"
    );
}

#[test]
fn resource_in_external_file_composes_with_load_prefix() {
    // A `Route::resource('patient', …)` in a file loaded via
    // `Route::as('admin.')->group(base_path('routes/b.php'))` must yield
    // `admin.patient.*` (resource derivation + external-file prefix compose).
    let (_tmp, index) = index_routes(&[
        (
            "routes/web.php",
            "<?php\nRoute::as('admin.')->group(base_path('routes/b.php'));\n",
        ),
        (
            "routes/b.php",
            "<?php\nRoute::resource('patient', PatientController::class);\n",
        ),
    ]);

    for action in [
        "index", "create", "store", "show", "edit", "update", "destroy",
    ] {
        assert!(
            index.get(&format!("admin.patient.{action}")).is_some(),
            "expected admin.patient.{action} in index"
        );
        // The bare leaf survives too (file scanned directly).
        assert!(
            index.get(&format!("patient.{action}")).is_some(),
            "expected bare patient.{action} in index"
        );
    }
}

#[test]
fn no_indexed_resource_name_ever_starts_with_slash() {
    // Belt-and-suspenders: across a realistic mix, assert the slash bug is gone.
    let (_tmp, index) = index_routes(&[(
        "routes/web.php",
        "<?php\nRoute::name('api.')->group(function () {\n    Route::resource('/leads', LeadController::class);\n    Route::apiResource('/photos/', PhotoController::class);\n});\nRoute::resource('/bare', BareController::class);\n",
    )]);

    for name in index.routes.keys() {
        assert!(
            !name.starts_with('/'),
            "indexed route name must never start with a slash: {name}"
        );
        assert!(
            !name.contains('/'),
            "indexed route name must never contain a slash: {name}"
        );
    }
    // Spot-check a couple of expected names exist.
    assert!(index.get("api.leads.index").is_some());
    assert!(index.get("api.photos.store").is_some());
    assert!(index.get("bare.show").is_some());
}

#[test]
fn closure_group_behavior_unchanged_regression() {
    // A plain in-file closure group must behave exactly as before — no external
    // load, no extra prefixes leaking in.
    let (_tmp, index) = index_routes(&[(
        "routes/web.php",
        "<?php\nRoute::name('admin.')->group(function () {\n    Route::get('/users', [UserController::class, 'index'])->name('users.index');\n});\nRoute::get('/login')->name('login');\n",
    )]);

    assert!(index.get("admin.users.index").is_some());
    assert!(index.get("login").is_some());
    assert!(
        index.get("users.index").is_none(),
        "in-file closure group should not also emit the bare leaf"
    );
}
