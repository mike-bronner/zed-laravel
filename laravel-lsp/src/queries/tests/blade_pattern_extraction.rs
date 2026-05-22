//! Integration tests for Blade pattern extraction.
//!
//! Covers `extract_all_blade_patterns` across components, directives,
//! attribute-embedded Blade, and translation helpers. Relocated from the
//! inline `mod tests` block in `src/queries.rs` so business logic and
//! test logic don't share a file.

use super::super::*;
use crate::parser::{language_blade, language_php, parse_blade, parse_php};

#[test]
fn extract_all_blade_patterns_components() {
    let blade_code = r#"
    <div>
        <x-button type="primary">Click me</x-button>
        <x-forms.input name="email" />
    </div>
    "#;

    let tree = parse_blade(blade_code).expect("Should parse Blade");
    let lang = language_blade();
    let patterns =
        extract_all_blade_patterns(&tree, blade_code, &lang).expect("Should extract patterns");

    assert!(
        !patterns.components.is_empty(),
        "Should find at least one component"
    );

    let component_names: Vec<&str> = patterns
        .components
        .iter()
        .map(|m| m.component_name)
        .collect();
    assert!(
        component_names
            .iter()
            .any(|&name| name == "button" || name.starts_with("button")),
        "Should find button component"
    );
}

#[test]
fn extract_all_blade_patterns_directives() {
    let blade_code = r#"
@extends('layouts.app')
@section('content')
    @foreach($users as $user)
        <p>{{ $user->name }}</p>
    @endforeach
@endsection
    "#;

    let tree = parse_blade(blade_code).expect("Should parse Blade");
    let lang = language_blade();
    let patterns =
        extract_all_blade_patterns(&tree, blade_code, &lang).expect("Should extract patterns");

    let directive_names: Vec<&str> = patterns
        .directives
        .iter()
        .map(|m| m.directive_name)
        .collect();

    assert!(directive_names.contains(&"extends"), "Should find @extends");
    assert!(directive_names.contains(&"section"), "Should find @section");
    assert!(directive_names.contains(&"foreach"), "Should find @foreach");

    // Should NOT contain closing directives
    assert!(
        !directive_names.contains(&"endforeach"),
        "Should not find @endforeach"
    );
    assert!(
        !directive_names.contains(&"endsection"),
        "Should not find @endsection"
    );
}

#[test]
fn extract_blade_feature_directive() {
    let blade_code = r#"
@feature('new-api')
    <p>New API is enabled!</p>
@else
    <p>Using old API</p>
@endfeature

@feature("beta-mode")
    <x-beta-badge />
@endfeature
    "#;

    let tree = parse_blade(blade_code).expect("Should parse Blade");
    let lang = language_blade();
    let patterns =
        extract_all_blade_patterns(&tree, blade_code, &lang).expect("Should extract patterns");

    // Check that @feature directive is captured
    let feature_directives: Vec<_> = patterns
        .directives
        .iter()
        .filter(|d| d.directive_name == "feature")
        .collect();

    assert_eq!(
        feature_directives.len(),
        2,
        "Should find 2 @feature directives"
    );

    // Verify first @feature directive
    let first = feature_directives[0];
    assert_eq!(first.directive_name, "feature");
    assert!(
        first.arguments.as_ref().unwrap().contains("new-api"),
        "First @feature should have 'new-api' argument"
    );

    // Verify second @feature directive
    let second = feature_directives[1];
    assert_eq!(second.directive_name, "feature");
    assert!(
        second.arguments.as_ref().unwrap().contains("beta-mode"),
        "Second @feature should have 'beta-mode' argument"
    );
}

#[test]
fn blade_patterns_inside_html_attributes() {
    // Test that Blade patterns inside HTML tag attributes are recognized.
    // This includes:
    // 1. Echo statements in attribute values: value="{{ config('app.name') }}"
    // 2. Directives inside attribute values: class="@if($x) blue @endif"
    // 3. Directives surrounding attributes: <div @if($show) class="visible" @endif>
    // 4. Blade attribute directives: @disabled($x), @checked($x), @selected($x)
    let blade_code = r#"
<input type="text" value="{{ config('app.name') }}" placeholder="{{ __('messages.placeholder') }}">
<div class="container @if($active) bg-blue @endif" data-env="{{ env('APP_ENV') }}">
    Content
</div>
<div class="@feature('beta-mode') beta @endfeature">Beta</div>
<div @if($showClass) class="conditional" @endif data-static="always">
    Conditional attribute
</div>
<button @disabled($isDisabled) @readonly($isReadonly) type="submit">
    Submit
</button>
<input @checked($isChecked) type="checkbox">
    "#;

    let tree = parse_blade(blade_code).expect("Should parse Blade");
    let lang = language_blade();
    let patterns =
        extract_all_blade_patterns(&tree, blade_code, &lang).expect("Should extract patterns");

    // Check that echo statements in attributes are captured
    let has_config_echo = patterns
        .echo_php
        .iter()
        .any(|e| e.php_content.contains("config('app.name')"));
    assert!(
        has_config_echo,
        "Should find config() in attribute echo: {:?}",
        patterns.echo_php
    );

    // Check that directives in attribute values are captured.
    // We should find at least 2 @if directives:
    //   1. class="container @if($active) bg-blue @endif"  (inside attribute value)
    //   2. <div @if($showClass) class="conditional" @endif>  (wrapping attributes)
    let if_count = patterns
        .directives
        .iter()
        .filter(|d| d.directive_name == "if")
        .count();
    assert!(
        if_count >= 2,
        "Should find at least 2 @if directives, found: {if_count}"
    );

    // Check that @feature in attributes is captured
    let has_feature_directive = patterns
        .directives
        .iter()
        .any(|d| d.directive_name == "feature");
    assert!(
        has_feature_directive,
        "Should find @feature directive in attribute"
    );

    // Check that Blade attribute directives are captured (@disabled, @checked, @readonly)
    let has_disabled = patterns
        .directives
        .iter()
        .any(|d| d.directive_name == "disabled");
    assert!(
        has_disabled,
        "Should find @disabled directive: {:?}",
        patterns
            .directives
            .iter()
            .map(|d| &d.directive_name)
            .collect::<Vec<_>>()
    );

    let has_checked = patterns
        .directives
        .iter()
        .any(|d| d.directive_name == "checked");
    assert!(has_checked, "Should find @checked directive");

    let has_readonly = patterns
        .directives
        .iter()
        .any(|d| d.directive_name == "readonly");
    assert!(has_readonly, "Should find @readonly directive");
}

#[test]
fn blade_translation_patterns() {
    // Test that we can extract translations from Blade echo syntax. This
    // exercises BOTH the Blade extractor (for `@lang` directives and echo
    // PHP capture) and the PHP extractor (for the `__()` inside `{{ }}`).
    let blade_code = r#"{{ __("Welcome to our app") }}
@lang("welcome")"#;

    // Parse as Blade
    let blade_tree = parse_blade(blade_code).expect("Should parse Blade");
    let blade_lang = language_blade();
    let blade_patterns = extract_all_blade_patterns(&blade_tree, blade_code, &blade_lang)
        .expect("Should extract Blade patterns");

    // Parse as PHP to see if __() is captured
    let php_tree = parse_php(blade_code).expect("Should parse as PHP");
    let php_lang = language_php();
    let _php_patterns = extract_all_php_patterns(&php_tree, blade_code, &php_lang)
        .expect("Should extract PHP patterns");

    // At minimum, @lang should be captured as a directive
    let has_lang_directive = blade_patterns
        .directives
        .iter()
        .any(|d| d.directive_name == "lang");
    assert!(
        has_lang_directive,
        "@lang should be captured as a directive"
    );

    // And we should have captured the {{ __() }} echo content
    let has_echo_php = !blade_patterns.echo_php.is_empty();
    assert!(has_echo_php, "Should capture PHP content inside {{ }}");
    assert!(
        blade_patterns.echo_php[0].php_content.contains("__"),
        "Echo should contain __() call"
    );
}
