//! Integration test for single-pass extraction architecture.
//!
//! Verifies that `extract_all_php_patterns` returns every pattern type
//! from one tree-sitter pass. The test is named for the historical
//! intent ("single pass is faster than per-pattern passes") but
//! functionally it's a smoke test for the multi-pattern extractor.
//!
//! Relocated from the inline `mod tests` block in `src/queries.rs` so
//! business logic and test logic don't share a file.

use laravel_lsp::parser::{language_php, parse_php};
use laravel_lsp::queries::extract_all_php_patterns;

#[test]
fn single_pass_extracts_all_pattern_types() {
    let php_code = r#"<?php
    return view('home');
    $name = env('APP_NAME');
    $key = config('app.key');
    Route::middleware('auth')->get('/');
    $msg = __('messages.welcome');
    $css = asset('css/app.css');
    $service = app('cache');
    $url = route('home');
    "#;

    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();

    // Should extract all patterns in one call
    let patterns = extract_all_php_patterns(&tree, php_code, &lang)
        .expect("Should extract patterns");

    // Verify we found patterns of different types
    assert!(!patterns.views.is_empty(), "Should find views");
    assert!(!patterns.env_calls.is_empty(), "Should find env calls");
    assert!(!patterns.config_calls.is_empty(), "Should find config calls");
    assert!(!patterns.middleware_calls.is_empty(), "Should find middleware");
    assert!(!patterns.translation_calls.is_empty(), "Should find translations");
    assert!(!patterns.asset_calls.is_empty(), "Should find assets");
    assert!(!patterns.binding_calls.is_empty(), "Should find bindings");
    assert!(!patterns.route_calls.is_empty(), "Should find routes");
}
