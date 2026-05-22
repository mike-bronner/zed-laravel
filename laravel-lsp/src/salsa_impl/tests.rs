//! Tests for the Salsa-backed incremental computation actor.
//!
//! Originally lived as two inline `#[cfg(test)] mod *_tests {}` blocks
//! inside `salsa_impl.rs`. Flattened into a single submodule here
//! to keep the business-logic file clean while preserving access to
//! the parent module via `use super::*`.

use super::*;

// ─── Vite directive parsing ────────────────────────────────────────────

#[test]
fn test_vite_singular_syntax() {
    // @vite('resources/css/app.css') - args from tree-sitter
    let args = "('resources/css/app.css')";
    let results = parse_vite_directive_assets(args, 0, 0, 5);

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, "resources/css/app.css");
}

#[test]
fn test_vite_array_syntax() {
    // @vite(['resources/css/app.css', 'resources/js/app.js'])
    let args = "(['resources/css/app.css', 'resources/js/app.js'])";
    let results = parse_vite_directive_assets(args, 0, 0, 5);

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].0, "resources/css/app.css");
    assert_eq!(results[1].0, "resources/js/app.js");
}

#[test]
fn test_vite_double_quotes() {
    let args = r#"("resources/css/app.css")"#;
    let results = parse_vite_directive_assets(args, 0, 0, 5);

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, "resources/css/app.css");
}

#[test]
fn test_extract_translation_from_echo() {
    // Test the regex extraction
    let content = r#"__("Welcome to our app")"#;
    let result = super::extract_translation_from_echo(content);
    assert!(result.is_some(), "Should extract translation from __()");
    let (key, start, end) = result.unwrap();
    assert_eq!(key, "Welcome to our app");
    println!("Extracted: key='{}', start={}, end={}", key, start, end);

    // Test with single quotes
    let content2 = "__('messages.welcome')";
    let result2 = super::extract_translation_from_echo(content2);
    assert!(
        result2.is_some(),
        "Should extract translation from __() with single quotes"
    );
    let (key2, _, _) = result2.unwrap();
    assert_eq!(key2, "messages.welcome");

    // Test trans()
    let content3 = "trans('auth.failed')";
    let result3 = super::extract_translation_from_echo(content3);
    assert!(result3.is_some(), "Should extract translation from trans()");
    let (key3, _, _) = result3.unwrap();
    assert_eq!(key3, "auth.failed");
}

#[test]
fn test_vite_column_positions() {
    // For @vite('resources/css/app.css'):
    // Position: 0123456789...
    //           @vite('resources/css/app.css')
    // @ at 0, v at 1, ... e at 4, ( at 5, ' at 6, r at 7
    // Path "resources/css/app.css" is 21 chars
    // LSP needs +1 offset, so start col is 8
    let args = "('resources/css/app.css')";
    let path = "resources/css/app.css";
    let results = parse_vite_directive_assets(args, 0, 0, 5);

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, path);
    // Column should point to 'r' (first char of path), adjusted for LSP
    assert_eq!(results[0].2, 8, "start column should be 8");
    // End column should be 8 + 21 = 29
    assert_eq!(
        results[0].3,
        8 + path.len() as u32,
        "end column should be start + path.len()"
    );
}

// ─── Component alias parsing ────────────────────────────────────────────

fn make_config_with_alias(alias: &str, view: &str) -> LaravelConfigData {
    let mut aliases = HashMap::new();
    aliases.insert(alias.to_string(), view.to_string());

    LaravelConfigData {
        root: PathBuf::from("/project"),
        view_paths: vec![PathBuf::from("resources/views")],
        component_paths: Vec::new(),
        livewire_path: None,
        has_livewire: false,
        view_namespaces: HashMap::new(),
        component_namespaces: HashMap::new(),
        component_aliases: aliases,
        icon_aliases: HashMap::new(),
    }
}

fn make_config_with_icon(tag: &str, svg_path: &str) -> LaravelConfigData {
    let mut icons = HashMap::new();
    icons.insert(tag.to_string(), svg_path.to_string());

    LaravelConfigData {
        root: PathBuf::from("/project"),
        view_paths: vec![PathBuf::from("resources/views")],
        component_paths: Vec::new(),
        livewire_path: None,
        has_livewire: false,
        view_namespaces: HashMap::new(),
        component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: icons,
    }
}

#[test]
fn icon_tag_resolves_to_svg_path() {
    let config = make_config_with_icon(
        "heroicon-o-clock",
        "/abs/vendor/blade-ui-kit/blade-heroicons/resources/svg/o-clock.svg",
    );
    let paths = config.resolve_component_path("heroicon-o-clock");
    assert_eq!(paths.len(), 1);
    assert_eq!(
        paths[0],
        PathBuf::from("/abs/vendor/blade-ui-kit/blade-heroicons/resources/svg/o-clock.svg"),
    );
}

#[test]
fn unregistered_icon_tag_falls_through() {
    let config = make_config_with_icon("heroicon-o-clock", "/abs/path/o-clock.svg");
    let paths = config.resolve_component_path("heroicon-o-bell");
    // Falls through to directory convention — no svg path returned.
    assert!(
        paths
            .iter()
            .all(|p| !p.to_string_lossy().ends_with("o-bell.svg")),
        "unregistered icon should not return a phantom svg path: {:?}",
        paths,
    );
}

#[test]
fn aliased_component_resolves_to_aliased_view_path() {
    let config = make_config_with_alias("light-button", "components.buttons.light-button");

    let paths = config.resolve_component_path("light-button");

    assert!(
        paths
            .iter()
            .any(|p| p.ends_with("components/buttons/light-button.blade.php")),
        "expected aliased path, got: {:?}",
        paths,
    );
}

#[test]
fn unaliased_component_falls_back_to_directory_convention() {
    let config = make_config_with_alias("light-button", "components.buttons.light-button");

    // 'unaliased-component' is not registered; should fall through.
    let paths = config.resolve_component_path("unaliased-component");

    assert!(!paths.is_empty(), "expected fallback paths");
    assert!(
        paths
            .iter()
            .all(|p| !p.to_string_lossy().contains("buttons/light-button")),
        "alias must not bleed into unrelated lookups: {:?}",
        paths,
    );
}

#[test]
fn namespaced_component_bypasses_alias_map() {
    // Package components (`pkg::comp`) must not be intercepted by the alias map,
    // since namespace separators carry their own resolution rules.
    let config = make_config_with_alias("courier::alert", "components.never.this");

    let paths = config.resolve_component_path("courier::alert");

    assert!(
        paths
            .iter()
            .all(|p| !p.to_string_lossy().contains("components/never/this")),
        "namespaced lookup must bypass alias map: {:?}",
        paths,
    );
}
