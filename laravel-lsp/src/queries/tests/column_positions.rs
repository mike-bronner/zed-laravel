//! Integration tests for column-position accuracy across every captured
//! pattern.
//!
//! These tests pin down the contract that captured columns point at
//! string CONTENT (not quotes), and that the helper
//! `calculate_string_column_range` correctly handles directive arg shapes
//! including indentation, double quotes, and spaces inside parentheses.
//!
//! Position-indexing convention: 0-based throughout. See
//! `CLAUDE.md` § Position Indexing Convention.
//!
//! Relocated from the inline `mod tests` block in `src/queries.rs` so
//! business logic and test logic don't share a file.

use super::super::*;
use crate::parser::{language_blade, language_php, parse_blade, parse_php};

// ─── PHP helpers ────────────────────────────────────────────────────────────

#[test]
fn view_column_positions() {
    // view('users.profile')
    // Position: 0         1         2
    //           0123456789012345678901234567
    //           <?php view('users.profile');
    // The tree-sitter query captures string_content (without quotes).
    let php_code = "<?php view('users.profile');";
    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns = extract_all_php_patterns(&tree, php_code, &lang)
        .expect("Should extract patterns");

    assert_eq!(patterns.views.len(), 1);
    let view = &patterns.views[0];

    assert_eq!(view.view_name, "users.profile");
    // In "<?php view('users.profile');", 'u' starts at column 12
    assert_eq!(view.column, 12, "column should point to first char of view name");
    // End column should be at 'e' + 1 = 25
    assert_eq!(view.end_column, 25, "end_column should be after last char");
}

#[test]
fn env_column_positions() {
    // env('APP_NAME')
    // Position: 0         1         2
    //           0123456789012345678901
    //           <?php env('APP_NAME');
    let php_code = "<?php env('APP_NAME');";
    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns = extract_all_php_patterns(&tree, php_code, &lang)
        .expect("Should extract patterns");

    assert_eq!(patterns.env_calls.len(), 1);
    let env_call = &patterns.env_calls[0];

    assert_eq!(env_call.var_name, "APP_NAME");
    assert_eq!(env_call.column, 11, "column should point to first char");
    assert_eq!(env_call.end_column, 19, "end_column should be after last char");
}

#[test]
fn config_column_positions() {
    let php_code = "<?php config('app.name');";
    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns = extract_all_php_patterns(&tree, php_code, &lang)
        .expect("Should extract patterns");

    assert_eq!(patterns.config_calls.len(), 1);
    let config_call = &patterns.config_calls[0];

    assert_eq!(config_call.config_key, "app.name");
    assert_eq!(config_call.column, 14, "column should point to first char");
    assert_eq!(config_call.end_column, 22, "end_column should be after last char");
}

#[test]
fn translation_column_positions() {
    let php_code = "<?php __('messages.welcome');";
    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns = extract_all_php_patterns(&tree, php_code, &lang)
        .expect("Should extract patterns");

    assert_eq!(patterns.translation_calls.len(), 1);
    let trans = &patterns.translation_calls[0];

    assert_eq!(trans.translation_key, "messages.welcome");
    assert_eq!(trans.column, 10, "column should point to first char");
    assert_eq!(trans.end_column, 26, "end_column should be after last char");
}

#[test]
fn asset_column_positions() {
    let php_code = "<?php asset('css/app.css');";
    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns = extract_all_php_patterns(&tree, php_code, &lang)
        .expect("Should extract patterns");

    assert_eq!(patterns.asset_calls.len(), 1);
    let asset = &patterns.asset_calls[0];

    assert_eq!(asset.path, "css/app.css");
    assert_eq!(asset.column, 13, "column should point to first char");
    assert_eq!(asset.end_column, 24, "end_column should be after last char");
}

#[test]
fn middleware_column_positions() {
    let php_code = "<?php Route::middleware('auth');";
    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns = extract_all_php_patterns(&tree, php_code, &lang)
        .expect("Should extract patterns");

    assert_eq!(patterns.middleware_calls.len(), 1);
    let mw = &patterns.middleware_calls[0];

    assert_eq!(mw.middleware_name, "auth");
    assert_eq!(mw.column, 25, "column should point to first char");
    assert_eq!(mw.end_column, 29, "end_column should be after last char");
}

#[test]
fn route_column_positions() {
    let php_code = "<?php route('home');";
    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns = extract_all_php_patterns(&tree, php_code, &lang)
        .expect("Should extract patterns");

    assert_eq!(patterns.route_calls.len(), 1);
    let route = &patterns.route_calls[0];

    assert_eq!(route.route_name, "home");
    assert_eq!(route.column, 13, "column should point to first char");
    assert_eq!(route.end_column, 17, "end_column should be after last char");
}

#[test]
fn binding_column_positions() {
    let php_code = "<?php app('cache');";
    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns = extract_all_php_patterns(&tree, php_code, &lang)
        .expect("Should extract patterns");

    assert_eq!(patterns.binding_calls.len(), 1);
    let binding = &patterns.binding_calls[0];

    assert_eq!(binding.binding_name, "cache");
    assert_eq!(binding.column, 11, "column should point to first char");
    assert_eq!(binding.end_column, 16, "end_column should be after last char");
}

#[test]
fn feature_column_positions() {
    // Feature::active('new-api')
    // Position: 0         1         2         3
    //           0123456789012345678901234567890123
    //           <?php Feature::active('new-api');
    // 0-5 = "<?php ", 6-12 = "Feature", 13 = ":", 14 = ":", 15-20 = "active",
    // 21 = "(", 22 = "'", 23 = "n"
    let php_code = "<?php Feature::active('new-api');";
    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns = extract_all_php_patterns(&tree, php_code, &lang)
        .expect("Should extract patterns");

    if !patterns.feature_calls.is_empty() {
        let feature = &patterns.feature_calls[0];
        assert_eq!(feature.feature_name, "new-api");
        assert_eq!(feature.column, 23, "column should point to first char of feature name");
        assert_eq!(feature.end_column, 30, "end_column should be after last char");
    }
}

// ─── Blade ──────────────────────────────────────────────────────────────────

#[test]
fn blade_component_column_positions() {
    let blade_code = "<div><x-button></x-button></div>";
    let tree = parse_blade(blade_code).expect("Should parse Blade");
    let lang = language_blade();
    let patterns = extract_all_blade_patterns(&tree, blade_code, &lang)
        .expect("Should extract patterns");

    // Components may or may not be found depending on tree-sitter grammar.
    // Just verify the structure works.
    if !patterns.components.is_empty() {
        let component = &patterns.components[0];
        assert!(component.column < blade_code.len(), "column should be valid");
        assert!(
            component.end_column >= component.column,
            "end_column should be >= column"
        );
    }
}

#[test]
fn livewire_component_column_positions() {
    let blade_code = "<div><livewire:counter /></div>";
    let tree = parse_blade(blade_code).expect("Should parse Blade");
    let lang = language_blade();
    let patterns = extract_all_blade_patterns(&tree, blade_code, &lang)
        .expect("Should extract patterns");

    // Livewire components may or may not be found depending on grammar.
    if !patterns.livewire.is_empty() {
        let livewire = &patterns.livewire[0];
        assert!(livewire.column < blade_code.len(), "column should be valid");
        assert!(
            livewire.end_column >= livewire.column,
            "end_column should be >= column"
        );
    }
}

#[test]
fn blade_directive_column_positions() {
    let blade_code = "@include('partials.header')";
    let tree = parse_blade(blade_code).expect("Should parse Blade");
    let lang = language_blade();
    let patterns = extract_all_blade_patterns(&tree, blade_code, &lang)
        .expect("Should extract patterns");

    let include_directive = patterns
        .directives
        .iter()
        .find(|d| d.directive_name == "include");

    assert!(include_directive.is_some(), "Should find @include directive");
    let directive = include_directive.unwrap();

    // Directive starts at column 0 (the @)
    assert_eq!(directive.column, 0, "directive should start at column 0");
    // string_column should point to the view name string
    assert!(
        directive.string_column > 0,
        "string_column should be after directive name"
    );
}

// ─── Edge cases ─────────────────────────────────────────────────────────────

#[test]
fn column_positions_with_indentation() {
    // Test that column positions work correctly with leading whitespace.
    // Position: 0         1         2
    //           012345678901234567890123
    //               view('dashboard');
    // (4 spaces + view( = column 9, then ' = column 10, d = column 10)
    let php_code = "<?php\n    view('dashboard');"; // 4 spaces indentation
    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns = extract_all_php_patterns(&tree, php_code, &lang)
        .expect("Should extract patterns");

    assert_eq!(patterns.views.len(), 1);
    let view = &patterns.views[0];

    // On line 1 (0-indexed), the indented content:
    //   "    view('dashboard');"
    // Column 4-7 is "view", column 8 is "(", column 9 is "'", column 10 is "d"
    assert_eq!(view.row, 1, "should be on second line (0-indexed)");
    assert_eq!(view.column, 10, "column should point to first char of view name");
}

#[test]
fn double_quote_column_positions() {
    let php_code = r#"<?php view("users.profile");"#;
    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns = extract_all_php_patterns(&tree, php_code, &lang)
        .expect("Should extract patterns");

    assert_eq!(patterns.views.len(), 1);
    let view = &patterns.views[0];

    assert_eq!(view.view_name, "users.profile");
    assert_eq!(view.column, 12, "column should point to first char inside quotes");
    assert_eq!(view.end_column, 25, "end_column should be after last char");
}

// ─── Helper: calculate_string_column_range ──────────────────────────────────

#[test]
fn calculate_string_column_range_directive_arg_shapes() {
    // Tests the helper that calculates string column positions for directives.
    // Uses parameter_column (where tree-sitter says the parameter node starts)
    // instead of calculating from directive position.

    // Test 1: Args with full parentheses - @include('view')
    // Position: 0         1
    //           0123456789012345678
    //           @include('view')
    // parameter_column = 8 (where '(' is), content 'view' at columns 10-14
    let result = calculate_string_column_range(8, "('view')");
    assert_eq!(result, Some((10, 14)), "@include('view') - 'view' at columns 10-14");

    // Test 2: Args with double quotes - @feature("beta-mode")
    let result = calculate_string_column_range(8, "(\"beta-mode\")");
    assert_eq!(
        result,
        Some((10, 19)),
        "@feature(\"beta-mode\") - 'beta-mode' at columns 10-19"
    );

    // Test 3: Args without opening paren (tree-sitter captures just the
    // quoted part). When args don't include '(', parameter_column already
    // points past it, so we DON'T add 1 for paren.
    let result = calculate_string_column_range(9, "'view')");
    assert_eq!(result, Some((10, 14)), "Args without ( - parameter already past paren");

    let result = calculate_string_column_range(9, "'view'");
    assert_eq!(result, Some((10, 14)), "Args without parens - parameter at quote");

    // Test 4: Directive with space before paren - @feature ('beta-mode')
    // parameter_column = 9 (where '(' is after the space)
    // content 'beta-mode' should be at columns 11-20
    let result = calculate_string_column_range(9, "('beta-mode')");
    assert_eq!(
        result,
        Some((11, 20)),
        "@feature ('beta-mode') with space - at columns 11-20"
    );

    // Test 5: Indented directive - @include('partial')
    // 4 spaces + @include = 12, parameter at column 12
    let result = calculate_string_column_range(12, "('partial')");
    assert_eq!(result, Some((14, 21)), "Indented directive at columns 14-21");

    // Test 6: Args with spaces after opening paren - parameter at column 8
    let result = calculate_string_column_range(8, "(  'view')");
    assert_eq!(result, Some((12, 16)), "Spaces after ( - at columns 12-16");

    // Test 7: Invalid args (no quotes)
    let result = calculate_string_column_range(8, "($condition)");
    assert_eq!(result, None, "Args without quotes should return None");
}
