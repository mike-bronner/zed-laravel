//! Tests for the baseline `extract_all_php_patterns` flow across the
//! canonical Laravel helpers (`view()`, `env()`, `config()`,
//! `Route::middleware()`, etc.).

use super::super::*;
use crate::parser::{language_php, parse_php};

#[test]
fn test_extract_all_php_patterns_views() {
    let php_code = r#"<?php
    return view('users.profile');
    Route::view('/home', 'welcome');
    echo view("admin.dashboard");
    "#;

    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns = extract_all_php_patterns(&tree, php_code, &lang)
        .expect("Should extract patterns");

    assert_eq!(patterns.views.len(), 3, "Should find 3 view calls");

    let view_names: Vec<&str> = patterns.views.iter().map(|m| m.view_name).collect();
    assert!(view_names.contains(&"users.profile"));
    assert!(view_names.contains(&"welcome"));
    assert!(view_names.contains(&"admin.dashboard"));

    let welcome = patterns.views.iter().find(|v| v.view_name == "welcome").unwrap();
    assert!(welcome.is_route_view, "Route::view() should set is_route_view=true");

    let users = patterns.views.iter().find(|v| v.view_name == "users.profile").unwrap();
    assert!(!users.is_route_view, "view() should set is_route_view=false");
}

#[test]
fn test_extract_all_php_patterns_env() {
    let php_code = r#"<?php
    $name = env('APP_NAME', 'Laravel');
    $debug = env("APP_DEBUG");
    "#;

    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns = extract_all_php_patterns(&tree, php_code, &lang)
        .expect("Should extract patterns");

    assert_eq!(patterns.env_calls.len(), 2, "Should find 2 env calls");
    assert_eq!(patterns.env_calls[0].var_name, "APP_NAME");
    assert_eq!(patterns.env_calls[1].var_name, "APP_DEBUG");
}

#[test]
fn test_extract_all_php_patterns_middleware() {
    let php_code = r#"<?php
    Route::middleware('auth')->group(function () {});
    Route::middleware(['auth', 'verified'])->get('/dashboard');
    "#;

    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns = extract_all_php_patterns(&tree, php_code, &lang)
        .expect("Should extract patterns");

    let middleware_names: Vec<&str> = patterns
        .middleware_calls
        .iter()
        .map(|m| m.middleware_name)
        .collect();

    assert!(middleware_names.contains(&"auth"), "Should find 'auth' middleware");
    assert!(
        middleware_names.contains(&"verified"),
        "Should find 'verified' middleware"
    );
}
