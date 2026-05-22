//! Integration tests for Laravel call-site variants (issue #13).
//!
//! Verifies that the additional helper / facade variants added in PR #16
//! flow through the public pattern-extraction API and land in the same
//! `*Match` collections as their canonical helpers (`route()`, `config()`,
//! `env()`, `app()`), so that goto / completion / diagnostics dispatch
//! without any per-variant special-casing.
//!
//! These tests exercise only the public crate API
//! (`laravel_lsp::queries::extract_all_php_patterns`), matching the
//! convention used by `tests/integration_tests.rs`.

use super::super::*;
use std::collections::HashMap;

use crate::parser::{language_php, parse_php};

#[test]
fn route_variants_signed_route_and_url_facade() {
    // signed_route() and URL::signedRoute() should resolve like route().
    let php_code = r#"<?php
$a = route('home');
$b = signed_route('verify.email');
$c = URL::route('users.show', ['id' => 1]);
$d = URL::signedRoute('subscribe');
"#;
    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns = extract_all_php_patterns(&tree, php_code, &lang)
        .expect("Should extract patterns");

    let names: Vec<&str> = patterns.route_calls.iter().map(|r| r.route_name).collect();

    assert!(
        names.contains(&"home"),
        "route() should be captured; got {names:?}"
    );
    assert!(
        names.contains(&"verify.email"),
        "signed_route() should be captured; got {names:?}"
    );
    assert!(
        names.contains(&"users.show"),
        "URL::route() should be captured; got {names:?}"
    );
    assert!(
        names.contains(&"subscribe"),
        "URL::signedRoute() should be captured; got {names:?}"
    );
    assert_eq!(
        patterns.route_calls.len(),
        4,
        "All four route variants should be captured exactly once"
    );
}

#[test]
fn config_variants_getmany_modern_aliases_and_fluent() {
    // Config::getMany() (array form), modern Config::int/bool/float aliases,
    // and the config()->method('key') fluent instance form should all
    // resolve like config('key').
    let php_code = r#"<?php
$a = config('app.name');
$b = Config::get('database.default');
$c = Config::int('app.timeout');
$d = Config::bool('app.debug');
$e = Config::float('app.weight');
$f = Config::getMany(['mail.host', 'mail.port']);
$g = config()->string('app.locale');
$h = config()->array('app.providers');
"#;
    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns = extract_all_php_patterns(&tree, php_code, &lang)
        .expect("Should extract patterns");

    let keys: Vec<&str> = patterns.config_calls.iter().map(|c| c.config_key).collect();

    // Existing function call still works
    assert!(keys.contains(&"app.name"), "config() should match; got {keys:?}");

    // Existing Config::get still works
    assert!(keys.contains(&"database.default"), "Config::get should match; got {keys:?}");

    // Modern type aliases on the Config facade
    assert!(keys.contains(&"app.timeout"), "Config::int should match; got {keys:?}");
    assert!(keys.contains(&"app.debug"), "Config::bool should match; got {keys:?}");
    assert!(keys.contains(&"app.weight"), "Config::float should match; got {keys:?}");

    // Config::getMany array — both elements captured
    assert!(keys.contains(&"mail.host"), "Config::getMany[0] should match; got {keys:?}");
    assert!(keys.contains(&"mail.port"), "Config::getMany[1] should match; got {keys:?}");

    // config()->method fluent form
    assert!(keys.contains(&"app.locale"), "config()->string should match; got {keys:?}");
    assert!(keys.contains(&"app.providers"), "config()->array should match; got {keys:?}");

    // No accidental duplicates or extras
    assert_eq!(
        patterns.config_calls.len(),
        9,
        "Expected exactly 9 config keys captured, got {}: {keys:?}",
        patterns.config_calls.len()
    );
}

#[test]
fn env_variant_facade_get() {
    // Env::get('KEY') should resolve like env('KEY'), including correct
    // fallback detection on the second argument.
    let php_code = r#"<?php
$a = env('APP_NAME');
$b = Env::get('DB_HOST');
$c = Env::get('APP_KEY', 'fallback');
"#;
    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns = extract_all_php_patterns(&tree, php_code, &lang)
        .expect("Should extract patterns");

    let by_name: HashMap<&str, &EnvMatch> = patterns
        .env_calls
        .iter()
        .map(|e| (e.var_name, e))
        .collect();

    assert_eq!(patterns.env_calls.len(), 3, "Expected 3 env captures");
    assert!(by_name.contains_key("APP_NAME"), "env() should match");
    assert!(by_name.contains_key("DB_HOST"), "Env::get without fallback should match");
    assert!(by_name.contains_key("APP_KEY"), "Env::get with fallback should match");

    assert!(
        !by_name["DB_HOST"].has_fallback,
        "Env::get('DB_HOST') has no fallback argument"
    );
    assert!(
        by_name["APP_KEY"].has_fallback,
        "Env::get('APP_KEY', 'fallback') has a fallback argument"
    );
}

#[test]
fn container_variants_app_bound_and_is_shared() {
    // App::bound() and App::isShared() are container introspection methods
    // on the App facade. They take a string binding name, same as
    // app() / resolve().
    let php_code = r#"<?php
$a = app('cache');
$b = App::bound('auth');
$c = App::isShared('App\Contracts\Mailer');
$d = App::bound("queue");
"#;
    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns = extract_all_php_patterns(&tree, php_code, &lang)
        .expect("Should extract patterns");

    let names: Vec<&str> = patterns
        .binding_calls
        .iter()
        .map(|b| b.binding_name)
        .collect();

    assert!(names.contains(&"cache"), "app() should still match; got {names:?}");
    assert!(names.contains(&"auth"), "App::bound() should match; got {names:?}");
    assert!(
        names.contains(&"App\\Contracts\\Mailer"),
        "App::isShared() should match; got {names:?}"
    );
    assert!(
        names.contains(&"queue"),
        "App::bound() with double-quoted string should match; got {names:?}"
    );
    assert_eq!(
        patterns.binding_calls.len(),
        4,
        "Expected 4 binding captures, got {}",
        patterns.binding_calls.len()
    );
}
