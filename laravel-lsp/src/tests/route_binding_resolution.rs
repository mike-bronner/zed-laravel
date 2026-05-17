use laravel_lsp::route_binding::{
    discover_route_files, find_route_binding_type, infer_model_class_from_param,
    livewire_component_names_for_blade, parse_livewire_route_bindings, RouteBinding,
};
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

fn write(path: &PathBuf, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

// ============================================================================
// parse_livewire_route_bindings
// ============================================================================

#[test]
fn parses_single_route_with_single_param() {
    let content = r#"Route::livewire('/posts/{post}', 'pages::show-post');"#;
    let bindings = parse_livewire_route_bindings(content);
    assert_eq!(bindings.len(), 1);
    assert_eq!(
        bindings[0],
        RouteBinding {
            component_name: "pages::show-post".to_string(),
            params: vec!["post".to_string()],
        }
    );
}

#[test]
fn parses_multiple_params_in_path() {
    let content = r#"Route::livewire('/posts/{post}/comments/{comment}', 'show-comment');"#;
    let bindings = parse_livewire_route_bindings(content);
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].params, vec!["post", "comment"]);
}

#[test]
fn parses_optional_route_param() {
    let content = r#"Route::livewire('/users/{user?}', 'user.profile');"#;
    let bindings = parse_livewire_route_bindings(content);
    assert_eq!(bindings[0].params, vec!["user"]);
}

#[test]
fn parses_route_with_no_params() {
    let content = r#"Route::livewire('/dashboard', 'dashboard');"#;
    let bindings = parse_livewire_route_bindings(content);
    assert_eq!(bindings.len(), 1);
    assert!(bindings[0].params.is_empty());
}

#[test]
fn parses_with_double_quoted_strings() {
    let content = r#"Route::livewire("/posts/{post}", "pages::post");"#;
    let bindings = parse_livewire_route_bindings(content);
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].component_name, "pages::post");
}

#[test]
fn parses_multiple_routes_in_one_file() {
    let content = r#"
        Route::livewire('/posts/{post}', 'pages::show-post');
        Route::livewire('/users/{user}', 'pages::show-user');
        Route::livewire('/dashboard', 'pages::dashboard');
    "#;
    let bindings = parse_livewire_route_bindings(content);
    assert_eq!(bindings.len(), 3);
}

#[test]
fn ignores_non_livewire_routes() {
    let content = r#"
        Route::get('/api/posts', [PostsController::class, 'index']);
        Route::livewire('/posts/{post}', 'pages::post');
        Route::post('/api/posts', [PostsController::class, 'store']);
    "#;
    let bindings = parse_livewire_route_bindings(content);
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].component_name, "pages::post");
}

// ============================================================================
// infer_model_class_from_param
// ============================================================================

#[test]
fn infers_simple_class_name() {
    assert_eq!(
        infer_model_class_from_param("post"),
        Some("Post".to_string())
    );
}

#[test]
fn infers_pascal_case_from_snake_case() {
    assert_eq!(
        infer_model_class_from_param("user_setting"),
        Some("UserSetting".to_string())
    );
}

#[test]
fn infers_pascal_case_from_kebab_case() {
    assert_eq!(
        infer_model_class_from_param("user-setting"),
        Some("UserSetting".to_string())
    );
}

#[test]
fn infers_returns_none_for_empty_input() {
    assert_eq!(infer_model_class_from_param(""), None);
}

#[test]
fn infers_leaves_pluralization_alone() {
    // Singularizing is a heuristic with bad failure modes. The caller is
    // expected to use singular param names per Laravel convention.
    assert_eq!(
        infer_model_class_from_param("posts"),
        Some("Posts".to_string())
    );
}

// ============================================================================
// livewire_component_names_for_blade
// ============================================================================

#[test]
fn derives_pages_namespace_for_mfc() {
    let dir = TempDir::new().unwrap();
    let blade = dir
        .path()
        .join("resources/views/pages/\u{26A1}contact-us/contact-us.blade.php");
    let names = livewire_component_names_for_blade(&blade, dir.path());
    assert_eq!(names, vec!["pages::contact-us"]);
}

#[test]
fn derives_pages_namespace_for_sfc() {
    let dir = TempDir::new().unwrap();
    let blade = dir
        .path()
        .join("resources/views/pages/\u{26A1}contact-us.blade.php");
    let names = livewire_component_names_for_blade(&blade, dir.path());
    assert_eq!(names, vec!["pages::contact-us"]);
}

#[test]
fn derives_layouts_namespace() {
    let dir = TempDir::new().unwrap();
    let blade = dir
        .path()
        .join("resources/views/layouts/\u{26A1}app.blade.php");
    let names = livewire_component_names_for_blade(&blade, dir.path());
    assert_eq!(names, vec!["layouts::app"]);
}

#[test]
fn derives_default_namespace_for_components() {
    let dir = TempDir::new().unwrap();
    let blade = dir
        .path()
        .join("resources/views/components/posts/\u{26A1}create.blade.php");
    let names = livewire_component_names_for_blade(&blade, dir.path());
    assert_eq!(names, vec!["posts.create"]);
}

#[test]
fn derives_classic_livewire_name() {
    let dir = TempDir::new().unwrap();
    let blade = dir
        .path()
        .join("resources/views/livewire/counter.blade.php");
    let names = livewire_component_names_for_blade(&blade, dir.path());
    assert_eq!(names, vec!["counter"]);
}

#[test]
fn derives_nested_path_segments_as_dots() {
    let dir = TempDir::new().unwrap();
    let blade = dir
        .path()
        .join("resources/views/pages/admin/\u{26A1}users/users.blade.php");
    let names = livewire_component_names_for_blade(&blade, dir.path());
    assert_eq!(names, vec!["pages::admin.users"]);
}

// ============================================================================
// discover_route_files
// ============================================================================

#[test]
fn discovers_top_level_route_files() {
    let dir = TempDir::new().unwrap();
    write(&dir.path().join("routes/web.php"), "<?php // ...");
    write(&dir.path().join("routes/api.php"), "<?php // ...");
    write(&dir.path().join("app/Http/routes.php"), "<?php // ..."); // outside routes/ — ignored

    let files = discover_route_files(dir.path());
    assert_eq!(files.len(), 2);
    assert!(files.iter().any(|p| p.ends_with("routes/web.php")));
    assert!(files.iter().any(|p| p.ends_with("routes/api.php")));
}

#[test]
fn discovers_nested_route_files() {
    let dir = TempDir::new().unwrap();
    write(
        &dir.path().join("routes/admin/dashboard.php"),
        "<?php // ...",
    );
    write(&dir.path().join("routes/auth.php"), "<?php // ...");

    let files = discover_route_files(dir.path());
    assert_eq!(files.len(), 2);
}

#[test]
fn returns_empty_when_routes_dir_missing() {
    let dir = TempDir::new().unwrap();
    let files = discover_route_files(dir.path());
    assert!(files.is_empty());
}

// ============================================================================
// End-to-end: find_route_binding_type
// ============================================================================

#[test]
fn e2e_resolves_route_bound_type_for_pages_mfc() {
    let dir = TempDir::new().unwrap();
    let blade = dir
        .path()
        .join("resources/views/pages/\u{26A1}show-post/show-post.blade.php");
    write(&blade, "<div>{{ $post->title }}</div>");
    write(
        &dir.path().join("routes/web.php"),
        r#"<?php

use Illuminate\Support\Facades\Route;

Route::livewire('/posts/{post}', 'pages::show-post');
"#,
    );

    assert_eq!(
        find_route_binding_type(&blade, dir.path(), "post"),
        Some("Post".to_string())
    );
}

#[test]
fn e2e_returns_none_when_no_route_matches_component() {
    let dir = TempDir::new().unwrap();
    let blade = dir
        .path()
        .join("resources/views/pages/\u{26A1}contact-us/contact-us.blade.php");
    write(&blade, "<div>{{ $post }}</div>");
    write(
        &dir.path().join("routes/web.php"),
        r#"<?php Route::livewire('/posts/{post}', 'pages::show-post');"#,
    );

    assert_eq!(find_route_binding_type(&blade, dir.path(), "post"), None);
}

#[test]
fn e2e_returns_none_when_property_doesnt_match_route_param() {
    let dir = TempDir::new().unwrap();
    let blade = dir
        .path()
        .join("resources/views/pages/\u{26A1}show-post/show-post.blade.php");
    write(&blade, "<div>{{ $unrelated }}</div>");
    write(
        &dir.path().join("routes/web.php"),
        r#"<?php Route::livewire('/posts/{post}', 'pages::show-post');"#,
    );

    assert_eq!(
        find_route_binding_type(&blade, dir.path(), "unrelated"),
        None
    );
}

#[test]
fn e2e_walks_nested_route_files() {
    let dir = TempDir::new().unwrap();
    let blade = dir
        .path()
        .join("resources/views/pages/\u{26A1}admin-users/admin-users.blade.php");
    write(&blade, "<div>{{ $user }}</div>");
    write(
        &dir.path().join("routes/admin/users.php"),
        r#"<?php Route::livewire('/admin/users/{user}', 'pages::admin-users');"#,
    );

    assert_eq!(
        find_route_binding_type(&blade, dir.path(), "user"),
        Some("User".to_string())
    );
}
