//! Tests for the tree-sitter route outline walker.

use super::*;

fn names(routes: &[RouteOutline]) -> Vec<String> {
    routes
        .iter()
        .map(|r| {
            if r.is_group {
                format!("GROUP {}", r.uri)
            } else {
                format!("{} {}", r.method, r.uri)
            }
        })
        .collect()
}

// ============================================================================
// Empty / non-route inputs
// ============================================================================

#[test]
fn empty_input_returns_empty() {
    assert!(extract_route_outline("").is_empty());
}

#[test]
fn non_route_chain_is_ignored() {
    // `Foo::bar()` should not be claimed as a route.
    let content = "<?php\nFoo::bar('/x', fn () => 1);\n";
    assert!(extract_route_outline(content).is_empty());
}

// ============================================================================
// Simple route definitions
// ============================================================================

#[test]
fn extracts_bare_get_route() {
    let content = "<?php\nRoute::get('/users', fn () => 1);\n";
    let routes = extract_route_outline(content);
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].method, "GET");
    assert_eq!(routes[0].uri, "/users");
    assert!(routes[0].name.is_none());
    assert!(!routes[0].is_group);
}

#[test]
fn extracts_named_route() {
    let content = "<?php\nRoute::get('/users', fn () => 1)->name('users.index');\n";
    let routes = extract_route_outline(content);
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].method, "GET");
    assert_eq!(routes[0].uri, "/users");
    assert_eq!(routes[0].name.as_deref(), Some("users.index"));
}

#[test]
fn extracts_all_standard_verbs() {
    let content = r#"<?php
Route::get('/a', fn () => 1);
Route::post('/b', fn () => 1);
Route::put('/c', fn () => 1);
Route::patch('/d', fn () => 1);
Route::delete('/e', fn () => 1);
Route::options('/f', fn () => 1);
"#;
    let routes = extract_route_outline(content);
    let expected = vec![
        "GET /a",
        "POST /b",
        "PUT /c",
        "PATCH /d",
        "DELETE /e",
        "OPTIONS /f",
    ];
    assert_eq!(names(&routes), expected);
}

#[test]
fn extracts_laravel_route_methods() {
    // The verb-set the regex extractor missed in Mike's project.
    let content = r#"<?php
Route::livewire('/release-notes', ReleaseNotes::class);
Route::resource('/posts', PostController::class);
Route::apiResource('/api/posts', PostController::class);
Route::view('/about', 'pages.about');
Route::redirect('/old', '/new');
Route::permanentRedirect('/legacy', '/current');
"#;
    let routes = extract_route_outline(content);
    let expected = vec![
        "LIVEWIRE /release-notes",
        "RESOURCE /posts",
        "APIRESOURCE /api/posts",
        "VIEW /about",
        "REDIRECT /old",
        "PERMANENTREDIRECT /legacy",
    ];
    assert_eq!(names(&routes), expected);
}

// ============================================================================
// Multi-line route definitions
// ============================================================================

#[test]
fn extracts_multi_line_route_definition() {
    let content = r#"<?php
Route::get(
    '/users/{user}',
    [UserController::class, 'show']
)->name('users.show');
"#;
    let routes = extract_route_outline(content);
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].uri, "/users/{user}");
    assert_eq!(routes[0].name.as_deref(), Some("users.show"));
}

// ============================================================================
// Groups: prefix
// ============================================================================

#[test]
fn group_with_prefix_combines_uri() {
    let content = r#"<?php
Route::prefix('/api')->group(function () {
    Route::get('/users', fn () => 1)->name('users');
    Route::post('/users', fn () => 1)->name('users.store');
});
"#;
    let routes = extract_route_outline(content);
    assert_eq!(routes.len(), 1);
    assert!(routes[0].is_group);
    assert_eq!(routes[0].method, "GROUP");
    assert_eq!(routes[0].uri, "/api");
    assert_eq!(routes[0].children.len(), 2);
    assert_eq!(routes[0].children[0].method, "GET");
    assert_eq!(routes[0].children[0].uri, "/api/users");
    assert_eq!(routes[0].children[0].name.as_deref(), Some("users"));
    assert_eq!(routes[0].children[1].method, "POST");
    assert_eq!(routes[0].children[1].uri, "/api/users");
    assert_eq!(routes[0].children[1].name.as_deref(), Some("users.store"));
}

// ============================================================================
// Groups: name prefix
// ============================================================================

#[test]
fn group_with_name_combines_route_names() {
    let content = r#"<?php
Route::name('api.')->group(function () {
    Route::get('/users', fn () => 1)->name('users');
});
"#;
    let routes = extract_route_outline(content);
    assert_eq!(routes.len(), 1);
    assert!(routes[0].is_group);
    assert_eq!(routes[0].children.len(), 1);
    assert_eq!(routes[0].children[0].name.as_deref(), Some("api.users"));
}

// ============================================================================
// Groups: combined modifiers in any order
// ============================================================================

#[test]
fn group_with_middleware_prefix_name_combined() {
    // The pattern from Mike's actual project.
    let content = r#"<?php
Route::middleware('security')->prefix('/security')->name('security.')->group(function () {
    Route::name('questions.')->group(function () {
        Route::get('/questions', fn () => 1)->name('create');
        Route::post('/questions', fn () => 1)->name('store');
    });
});
"#;
    let routes = extract_route_outline(content);
    assert_eq!(routes.len(), 1);

    let outer = &routes[0];
    assert!(outer.is_group);
    assert_eq!(outer.uri, "/security");
    assert_eq!(outer.name.as_deref(), Some("security."));
    assert_eq!(outer.children.len(), 1);

    let inner = &outer.children[0];
    assert!(inner.is_group);
    assert_eq!(inner.uri, "/security"); // inner has no prefix; inherits outer's
    assert_eq!(inner.name.as_deref(), Some("security.questions."));
    assert_eq!(inner.children.len(), 2);

    assert_eq!(inner.children[0].method, "GET");
    assert_eq!(inner.children[0].uri, "/security/questions");
    assert_eq!(
        inner.children[0].name.as_deref(),
        Some("security.questions.create")
    );
    assert_eq!(inner.children[1].method, "POST");
    assert_eq!(inner.children[1].uri, "/security/questions");
    assert_eq!(
        inner.children[1].name.as_deref(),
        Some("security.questions.store")
    );
}

// ============================================================================
// Groups: modifiers in different orders
// ============================================================================

#[test]
fn modifier_order_does_not_matter() {
    // `name('x.')->prefix('/x')->group(...)` should behave the same as
    // `prefix('/x')->name('x.')->group(...)`.
    let content = r#"<?php
Route::name('x.')->prefix('/x')->group(function () {
    Route::get('/show', fn () => 1)->name('show');
});
"#;
    let routes = extract_route_outline(content);
    assert_eq!(routes[0].children[0].uri, "/x/show");
    assert_eq!(routes[0].children[0].name.as_deref(), Some("x.show"));
}

// ============================================================================
// Groups: `as()` alias for `name()`
// ============================================================================

#[test]
fn as_alias_works_like_name() {
    let content = r#"<?php
Route::as('api.')->group(function () {
    Route::get('/users', fn () => 1)->name('users');
});
"#;
    let routes = extract_route_outline(content);
    assert_eq!(routes[0].children[0].name.as_deref(), Some("api.users"));
}

// ============================================================================
// Edge: empty groups
// ============================================================================

#[test]
fn empty_group_is_omitted() {
    let content = r#"<?php
Route::prefix('/api')->group(function () {
});
"#;
    let routes = extract_route_outline(content);
    assert!(
        routes.is_empty(),
        "empty groups shouldn't clutter the outline"
    );
}

// ============================================================================
// Edge: routes outside groups mixed with groups
// ============================================================================

#[test]
fn mixed_top_level_and_grouped_routes() {
    let content = r#"<?php
Route::get('/', fn () => 1)->name('home');
Route::prefix('/api')->group(function () {
    Route::get('/users', fn () => 1)->name('api.users');
});
Route::get('/about', fn () => 1)->name('about');
"#;
    let routes = extract_route_outline(content);
    assert_eq!(routes.len(), 3);
    assert_eq!(routes[0].uri, "/");
    assert!(routes[1].is_group);
    assert_eq!(routes[1].children[0].uri, "/api/users");
    assert_eq!(routes[2].uri, "/about");
}

// ============================================================================
// Position tracking
// ============================================================================

#[test]
fn positions_are_zero_based() {
    let content = "<?php\nRoute::get('/x', fn () => 1);\n";
    let routes = extract_route_outline(content);
    assert_eq!(routes[0].start_line, 1, "second line is index 1");
    assert_eq!(routes[0].start_column, 0, "line starts at column 0");
}

// ============================================================================
// External-file group prefixes (issue #43)
// ============================================================================

#[test]
fn external_prefix_prepends_to_route_names() {
    // A file loaded via `Route::as('admin.')->group(base_path('that.php'))`
    // should display its routes' names with the inherited `admin.` prefix.
    let content = "<?php\nRoute::get('/users', fn () => 1)->name('users.index');\n";
    let routes = extract_route_outline_with_external(content, &["admin.".to_string()]);
    assert_eq!(routes.len(), 1);
    assert_eq!(
        routes[0].uri, "/users",
        "URIs are unaffected by name prefix"
    );
    assert_eq!(routes[0].name.as_deref(), Some("admin.users.index"));
}

#[test]
fn external_prefix_combines_with_in_file_group() {
    // External `admin.` + in-file `api.` group → `admin.api.users`.
    let content = r#"<?php
Route::name('api.')->group(function () {
    Route::get('/users', fn () => 1)->name('users');
});
"#;
    let routes = extract_route_outline_with_external(content, &["admin.".to_string()]);
    assert_eq!(routes.len(), 1);
    assert!(routes[0].is_group);
    assert_eq!(routes[0].name.as_deref(), Some("admin.api."));
    assert_eq!(
        routes[0].children[0].name.as_deref(),
        Some("admin.api.users")
    );
}

#[test]
fn external_prefix_uses_first_non_empty() {
    // The slice always contains "" (file scanned directly); the first non-empty
    // entry is the display prefix.
    let content = "<?php\nRoute::get('/x', fn () => 1)->name('x');\n";
    let routes =
        extract_route_outline_with_external(content, &["".to_string(), "admin.".to_string()]);
    assert_eq!(routes[0].name.as_deref(), Some("admin.x"));
}

#[test]
fn external_prefix_empty_matches_plain_extraction() {
    // No external prefix → byte-identical to `extract_route_outline`.
    let content = r#"<?php
Route::name('api.')->group(function () {
    Route::get('/users', fn () => 1)->name('users');
});
"#;
    assert_eq!(
        extract_route_outline_with_external(content, &[]),
        extract_route_outline(content),
    );
    assert_eq!(
        extract_route_outline_with_external(content, &["".to_string()]),
        extract_route_outline(content),
    );
}
