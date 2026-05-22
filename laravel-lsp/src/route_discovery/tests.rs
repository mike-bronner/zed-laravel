use super::*;

#[test]
fn extracts_single_quoted_route_name() {
    let src = r#"<?php
Route::get('/login', [LoginController::class, 'show'])->name('login');
"#;
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP);

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
    let results = extract_named_routes(src, &path, PRIORITY_APP);

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
    let results = extract_named_routes(src, &path, PRIORITY_APP);

    let names: Vec<&str> = results.iter().filter_map(|(n, _)| n.as_deref()).collect();
    assert_eq!(names, vec!["login", "logout", "register"]);
}

#[test]
fn tolerates_whitespace_in_call() {
    let src = "<?php\nRoute::get('/x')->name ( 'spaced' );\n";
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0.as_deref(), Some("spaced"));
}

#[test]
fn skips_variable_route_names() {
    let src = "<?php\nRoute::get('/x')->name($name);\n";
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP);
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
    let results = extract_named_routes(src, &path, PRIORITY_PACKAGE);

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
    let mut results = extract_named_routes(src, &path, PRIORITY_APP);
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
    let results = extract_named_routes(src, &path, PRIORITY_APP);
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
    let results = extract_named_routes(src, &path, PRIORITY_APP);
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
    let results = extract_named_routes(src, &path, PRIORITY_APP);
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
    let results = extract_named_routes(src, &path, PRIORITY_APP);
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
    let results = extract_named_routes(src, &path, PRIORITY_APP);
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
    let results = extract_named_routes(src, &path, PRIORITY_APP);
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
    let results = extract_named_routes(src, &path, PRIORITY_APP);
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
    let results = extract_named_routes(src, &path, PRIORITY_APP);
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
    let results = extract_named_routes(src, &path, PRIORITY_APP);
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
    let results = extract_named_routes(src, &path, PRIORITY_APP);
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
    let results = extract_named_routes(src, &path, PRIORITY_APP);
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
    let results = extract_named_routes(src, &path, PRIORITY_APP);
    let names: Vec<&str> = results.iter().filter_map(|(n, _)| n.as_deref()).collect();
    assert_eq!(names, vec!["admin.x", "api.y"]);
}
