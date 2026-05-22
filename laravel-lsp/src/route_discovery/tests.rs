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

    let names: Vec<&str> = results
        .iter()
        .filter_map(|(n, _)| n.as_deref())
        .collect();
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

    let names: Vec<&str> = results
        .iter()
        .filter_map(|(n, _)| n.as_deref())
        .collect();
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
        },
    );

    let def = idx.get("login").expect("should resolve");
    assert!(def.file.ends_with("routes/auth.php"), "app should win over package");
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
        },
    );
    let def = idx.get("horizon.index").expect("package route should index");
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
        priority_for_vendor_path(Path::new("/project/vendor/laravel/fortify/routes/routes.php")),
        PRIORITY_PACKAGE
    );
}
