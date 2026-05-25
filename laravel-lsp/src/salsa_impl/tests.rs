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

// ─── collect_matches_for_symbol (find-references engine) ───────────────

use std::path::PathBuf;

fn dummy_path() -> PathBuf {
    PathBuf::from("/fixture/app.php")
}

#[test]
fn parse_file_patterns_extracts_views_from_blade_echo() {
    // Sanity check on the Salsa-cached side: `{{ view('partials.header') }}`
    // in a Blade file must show up in ParsedPatterns.views. Regression test
    // for the bug where tree-sitter-php couldn't see through Blade wrappers.
    let db = LaravelDatabase::default();
    let path = PathBuf::from("/fixture/layout.blade.php");
    let source = "<div>{{ view('partials.header') }}</div>\n";
    let file = SourceFile::new(&db, path, 0, source.to_string());
    let patterns = parse_file_patterns(&db, file);
    let names: Vec<String> = patterns
        .views(&db)
        .iter()
        .map(|v| v.name(&db).name(&db).clone())
        .collect();
    assert!(
        names.iter().any(|n| n == "partials.header"),
        "expected 'partials.header' view extracted from Blade echo, got {:?}",
        names
    );
}

#[test]
fn parse_file_patterns_extracts_translations_from_blade_echo() {
    let db = LaravelDatabase::default();
    let path = PathBuf::from("/fixture/page.blade.php");
    let source = "<p>{{ __('auth.failed') }}</p>\n";
    let file = SourceFile::new(&db, path, 0, source.to_string());
    let patterns = parse_file_patterns(&db, file);
    let keys: Vec<String> = patterns
        .translation_refs(&db)
        .iter()
        .map(|t| t.key(&db).key(&db).clone())
        .collect();
    assert!(
        keys.iter().any(|k| k == "auth.failed"),
        "expected 'auth.failed' translation, got {:?}",
        keys
    );
}

#[test]
fn parse_file_patterns_extracts_views_from_blade_php_block() {
    // `@php ... @endphp` content gets the same re-parse treatment.
    let db = LaravelDatabase::default();
    let path = PathBuf::from("/fixture/php-block.blade.php");
    let source = r#"@php
    $partial = view('partials.alert');
@endphp
"#;
    let file = SourceFile::new(&db, path, 0, source.to_string());
    let patterns = parse_file_patterns(&db, file);
    let names: Vec<String> = patterns
        .views(&db)
        .iter()
        .map(|v| v.name(&db).name(&db).clone())
        .collect();
    assert!(
        names.iter().any(|n| n == "partials.alert"),
        "expected 'partials.alert' view extracted from @php block, got {:?}",
        names
    );
}

#[test]
fn collect_view_matches_only_named_classifications() {
    let mut p = ParsedPatternsData::default();
    p.views.push(Arc::new(ViewReferenceData {
        name: "users.profile".into(),
        line: 1,
        column: 5,
        end_column: 24,
        is_route_view: false,
    }));
    p.views.push(Arc::new(ViewReferenceData {
        name: "other.view".into(),
        line: 2,
        column: 5,
        end_column: 20,
        is_route_view: false,
    }));
    p.build_position_index();

    let mut out = Vec::new();
    collect_matches_for_symbol(
        &dummy_path(),
        &p,
        &SymbolRefData::View("users.profile".into()),
        &mut out,
    );

    assert_eq!(out.len(), 1, "only matching view name should appear");
    assert_eq!(out[0].line, 1);
    assert_eq!(out[0].column, 5);
}

#[test]
fn collect_view_also_picks_up_include_directives() {
    let mut p = ParsedPatternsData::default();
    p.directives.push(Arc::new(DirectiveReferenceData {
        name: "include".into(),
        arguments: Some("('users.profile')".into()),
        line: 0,
        column: 0,
        end_column: 30,
        string_column: 10,
        string_end_column: 23,
    }));
    p.directives.push(Arc::new(DirectiveReferenceData {
        name: "include".into(),
        arguments: Some("('not.this.one')".into()),
        line: 1,
        column: 0,
        end_column: 30,
        string_column: 10,
        string_end_column: 22,
    }));
    p.build_position_index();

    let mut out = Vec::new();
    collect_matches_for_symbol(
        &dummy_path(),
        &p,
        &SymbolRefData::View("users.profile".into()),
        &mut out,
    );

    assert_eq!(out.len(), 1);
    assert_eq!(out[0].line, 0);
}

#[test]
fn collect_route_matches() {
    let mut p = ParsedPatternsData::default();
    p.route_refs.push(Arc::new(RouteReferenceData {
        name: "home".into(),
        line: 0,
        column: 6,
        end_column: 10,
    }));
    p.route_refs.push(Arc::new(RouteReferenceData {
        name: "home".into(),
        line: 4,
        column: 12,
        end_column: 16,
    }));
    p.route_refs.push(Arc::new(RouteReferenceData {
        name: "admin.users".into(),
        line: 5,
        column: 12,
        end_column: 23,
    }));
    p.build_position_index();

    let mut out = Vec::new();
    collect_matches_for_symbol(
        &dummy_path(),
        &p,
        &SymbolRefData::Route("home".into()),
        &mut out,
    );

    assert_eq!(out.len(), 2);
    assert!(out.iter().all(|l| l.line == 0 || l.line == 4));
}

#[test]
fn collect_config_matches_by_key() {
    let mut p = ParsedPatternsData::default();
    p.config_refs.push(Arc::new(ConfigReferenceData {
        key: "app.name".into(),
        line: 0,
        column: 8,
        end_column: 16,
    }));
    p.config_refs.push(Arc::new(ConfigReferenceData {
        key: "different.key".into(),
        line: 1,
        column: 8,
        end_column: 21,
    }));
    p.build_position_index();

    let mut out = Vec::new();
    collect_matches_for_symbol(
        &dummy_path(),
        &p,
        &SymbolRefData::Config("app.name".into()),
        &mut out,
    );

    assert_eq!(out.len(), 1);
    assert_eq!(out[0].line, 0);
}

#[test]
fn collect_returns_empty_for_no_matches() {
    // Negative guarantee: same-shape strings present in other pattern kinds
    // must NOT bleed across kinds.
    let mut p = ParsedPatternsData::default();
    p.views.push(Arc::new(ViewReferenceData {
        name: "home".into(),
        line: 0,
        column: 5,
        end_column: 9,
        is_route_view: false,
    }));
    p.build_position_index();

    // Asking for route "home" must NOT match the view "home".
    let mut out = Vec::new();
    collect_matches_for_symbol(
        &dummy_path(),
        &p,
        &SymbolRefData::Route("home".into()),
        &mut out,
    );

    assert!(
        out.is_empty(),
        "a view name must not satisfy a route reference query"
    );
}
