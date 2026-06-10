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
        anonymous_component_paths: HashMap::new(),
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: aliases,
        icon_aliases: HashMap::new(),
        class_component_files: HashMap::new(),
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
        anonymous_component_paths: HashMap::new(),
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: icons,
        class_component_files: HashMap::new(),
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

// ─── Anonymous component path / namespace resolution (issue #44) ────────

fn make_config_with_anonymous_path(prefix: &str, abs_dir: &str) -> LaravelConfigData {
    let mut anon = HashMap::new();
    anon.insert(prefix.to_string(), PathBuf::from(abs_dir));

    LaravelConfigData {
        root: PathBuf::from("/project"),
        view_paths: vec![PathBuf::from("resources/views")],
        component_paths: Vec::new(),
        livewire_path: None,
        has_livewire: false,
        view_namespaces: HashMap::new(),
        component_namespaces: HashMap::new(),
        anonymous_component_paths: anon,
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
        class_component_files: HashMap::new(),
    }
}

fn make_config_with_anonymous_namespace(prefix: &str, dir: &str) -> LaravelConfigData {
    let mut anon = HashMap::new();
    anon.insert(prefix.to_string(), dir.to_string());

    LaravelConfigData {
        root: PathBuf::from("/project"),
        view_paths: vec![PathBuf::from("resources/views")],
        component_paths: Vec::new(),
        livewire_path: None,
        has_livewire: false,
        view_namespaces: HashMap::new(),
        component_namespaces: HashMap::new(),
        anonymous_component_paths: HashMap::new(),
        anonymous_component_namespaces: anon,
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
        class_component_files: HashMap::new(),
    }
}

#[test]
fn anonymous_component_path_resolves_to_registered_directory() {
    // Issue #44: Blade::anonymousComponentPath(resource_path('views/backstage/components'), 'backstage')
    // <x-backstage::layout> must resolve to the registered directory directly —
    // not the package-publish `resources/views/vendor/backstage/...` guess.
    let config = make_config_with_anonymous_path(
        "backstage",
        "/project/resources/views/backstage/components",
    );

    let paths = config.resolve_component_path("backstage::layout");

    assert_eq!(
        paths.first(),
        Some(&PathBuf::from(
            "/project/resources/views/backstage/components/layout.blade.php"
        )),
        "registered anonymousComponentPath must be the first (expected) candidate: {:?}",
        paths,
    );
}

#[test]
fn anonymous_component_path_supports_index_convention() {
    let config = make_config_with_anonymous_path(
        "backstage",
        "/project/resources/views/backstage/components",
    );

    let paths = config.resolve_component_path("backstage::layout");

    assert!(
        paths.iter().any(|p| p
            == &PathBuf::from(
                "/project/resources/views/backstage/components/layout/index.blade.php"
            )),
        "expected the index.blade.php convention candidate, got: {:?}",
        paths,
    );
}

#[test]
fn anonymous_component_path_resolves_dotted_component_name() {
    let config = make_config_with_anonymous_path(
        "backstage",
        "/project/resources/views/backstage/components",
    );

    let paths = config.resolve_component_path("backstage::forms.input");

    assert!(
        paths.iter().any(|p| p
            == &PathBuf::from(
                "/project/resources/views/backstage/components/forms/input.blade.php"
            )),
        "dotted component name must map dots to slashes: {:?}",
        paths,
    );
}

#[test]
fn anonymous_component_namespace_resolves_relative_to_view_paths() {
    // Blade::anonymousComponentNamespace('components.flux', 'flux')
    // <x-flux::button> -> resources/views/components/flux/button.blade.php
    let config = make_config_with_anonymous_namespace("flux", "components/flux");

    let paths = config.resolve_component_path("flux::button");

    assert!(
        paths
            .iter()
            .any(|p| p
                == &PathBuf::from("/project/resources/views/components/flux/button.blade.php")),
        "anonymous namespace must resolve under the view path: {:?}",
        paths,
    );
}

#[test]
fn unregistered_anonymous_prefix_does_not_borrow_registered_directory() {
    let config = make_config_with_anonymous_path(
        "backstage",
        "/project/resources/views/backstage/components",
    );

    // A different prefix must not resolve into the backstage directory.
    let paths = config.resolve_component_path("other::layout");

    assert!(
        paths
            .iter()
            .all(|p| !p.to_string_lossy().contains("backstage/components")),
        "unregistered prefix must not resolve into a registered anon directory: {:?}",
        paths,
    );
}

// ─── PHP path-expression resolution ─────────────────────────────────────

#[test]
fn resolve_php_path_expr_handles_resource_path() {
    let root = PathBuf::from("/project");
    let provider_dir = PathBuf::from("/project/app/Providers");
    assert_eq!(
        resolve_php_path_expr(
            "resource_path('views/backstage/components')",
            &root,
            &provider_dir
        ),
        Some(PathBuf::from(
            "/project/resources/views/backstage/components"
        )),
    );
}

#[test]
fn resolve_php_path_expr_handles_base_and_app_path() {
    let root = PathBuf::from("/project");
    let provider_dir = PathBuf::from("/project/app/Providers");
    assert_eq!(
        resolve_php_path_expr("base_path('resources/views/x')", &root, &provider_dir),
        Some(PathBuf::from("/project/resources/views/x")),
    );
    assert_eq!(
        resolve_php_path_expr("app_path('View/Components')", &root, &provider_dir),
        Some(PathBuf::from("/project/app/View/Components")),
    );
}

#[test]
fn resolve_php_path_expr_handles_dir_constant() {
    let root = PathBuf::from("/project");
    let provider_dir = PathBuf::from("/pkg/src/Providers");
    assert_eq!(
        resolve_php_path_expr(
            "__DIR__ . '/../resources/views/components'",
            &root,
            &provider_dir
        ),
        Some(PathBuf::from("/pkg/src/resources/views/components")),
    );
}

#[test]
fn resolve_php_path_expr_handles_absolute_literal() {
    let root = PathBuf::from("/project");
    let provider_dir = PathBuf::from("/project/app/Providers");
    assert_eq!(
        resolve_php_path_expr("'/abs/components'", &root, &provider_dir),
        Some(PathBuf::from("/abs/components")),
    );
}

#[test]
fn resolve_php_path_expr_rejects_unresolvable_expression() {
    let root = PathBuf::from("/project");
    let provider_dir = PathBuf::from("/project/app/Providers");
    assert_eq!(
        resolve_php_path_expr("$this->componentPath", &root, &provider_dir),
        None,
    );
}

// ─── Service-provider registration extraction ───────────────────────────

#[test]
fn extract_anonymous_component_paths_reads_registration() {
    let src = r#"
        public function boot(): void
        {
            Blade::anonymousComponentPath(resource_path('views/backstage/components'), 'backstage');
        }
    "#;
    let root = PathBuf::from("/project");
    let provider_dir = PathBuf::from("/project/app/Providers");

    let regs = extract_anonymous_component_paths(src, &root, &provider_dir);

    assert_eq!(regs.len(), 1);
    assert_eq!(regs[0].0, "backstage");
    assert_eq!(
        regs[0].1,
        PathBuf::from("/project/resources/views/backstage/components"),
    );
}

#[test]
fn extract_anonymous_component_namespaces_normalizes_dots() {
    let src = "Blade::anonymousComponentNamespace('components.flux', 'flux');";

    let regs = extract_anonymous_component_namespaces(src);

    assert_eq!(regs.len(), 1);
    assert_eq!(regs[0].0, "flux");
    assert_eq!(regs[0].1, "components/flux");
}

// ─── Salsa actor: config reflects provider registrations (issue #44) ────

/// A provider that registers both anonymous-component forms. Parsing is
/// text-based, so the paths need not exist on disk.
fn anon_component_provider_src() -> String {
    r#"<?php
namespace App\Providers;
use Illuminate\Support\Facades\Blade;
class AppServiceProvider {
    public function boot(): void {
        Blade::anonymousComponentPath(resource_path('views/test/components'), 'test');
        Blade::anonymousComponentNamespace('components.flux', 'flux');
    }
}
"#
    .to_string()
}

#[tokio::test]
async fn salsa_config_indexes_anonymous_component_registrations() {
    let handle = SalsaActor::spawn();
    let root = PathBuf::from("/tmp/zed-laravel-issue44");

    handle
        .register_config_files(root.clone(), None, None, None)
        .await
        .unwrap();
    handle
        .register_service_provider_source(
            root.join("app/Providers/AppServiceProvider.php"),
            anon_component_provider_src(),
            2,
            root.clone(),
        )
        .await
        .unwrap();

    let config = handle.get_laravel_config().await.unwrap().unwrap();

    assert_eq!(
        config.anonymous_component_paths.get("test"),
        Some(&root.join("resources/views/test/components")),
        "anonymousComponentPath must be indexed into the Laravel config",
    );
    assert_eq!(
        config.anonymous_component_namespaces.get("flux"),
        Some(&"components/flux".to_string()),
        "anonymousComponentNamespace must be indexed into the Laravel config",
    );
}

#[tokio::test]
async fn salsa_config_refreshes_when_provider_registered_after_first_build() {
    // Regression for the stale-config bug: the config is built (and memoized)
    // before the provider is registered, then the provider arrives via the
    // init-time app rescan. get_laravel_config must rebuild — not serve the
    // cached empty-namespace config — or `<x-test::...>` stays a false
    // "not found" until an unrelated provider edit forces an invalidation.
    let handle = SalsaActor::spawn();
    let root = PathBuf::from("/tmp/zed-laravel-issue44-late");

    handle
        .register_config_files(root.clone(), None, None, None)
        .await
        .unwrap();

    // First build — no providers registered yet. This memoizes config_cache.
    let before = handle.get_laravel_config().await.unwrap().unwrap();
    assert!(
        before.anonymous_component_paths.is_empty(),
        "precondition: no anon paths before the provider is registered",
    );

    // Provider registered late, as during the init-time app rescan.
    handle
        .register_service_provider_source(
            root.join("app/Providers/AppServiceProvider.php"),
            anon_component_provider_src(),
            2,
            root.clone(),
        )
        .await
        .unwrap();

    let after = handle.get_laravel_config().await.unwrap().unwrap();
    assert_eq!(
        after.anonymous_component_paths.get("test"),
        Some(&root.join("resources/views/test/components")),
        "config must refresh after late provider registration (config_cache must be invalidated)",
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

// ─── Shared component resolution (issue #69) ────────────────────────────
//
// `component_candidate_paths` is the single source of truth shared by
// goto-definition and the "component not found" diagnostic. These tests pin
// the class-based `Blade::componentNamespace` (PSR-4) resolution that the
// naive guesses in `resolve_component_path` missed — the Filament / mail
// failure case from the issue — plus the false-negative guarantee.

use crate::composer_autoload::ComposerAutoload;
use tempfile::TempDir;

/// Build a Laravel-shaped tempdir with the given (relative path, body) pairs.
fn project_with_files(files: &[(&str, &str)]) -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    for (relpath, body) in files {
        let full = dir.path().join(relpath);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, body).unwrap();
    }
    let root = dir.path().to_path_buf();
    (dir, root)
}

/// Config whose only interesting field is a set of `Blade::componentNamespace`
/// registrations (`prefix => PHP namespace`), rooted at `root`.
fn config_with_component_namespaces(root: &Path, ns: &[(&str, &str)]) -> LaravelConfigData {
    let mut component_namespaces = HashMap::new();
    for (prefix, php_ns) in ns {
        component_namespaces.insert(prefix.to_string(), php_ns.to_string());
    }
    LaravelConfigData {
        root: root.to_path_buf(),
        view_paths: vec![PathBuf::from("resources/views")],
        component_paths: Vec::new(),
        livewire_path: None,
        has_livewire: false,
        view_namespaces: HashMap::new(),
        component_namespaces,
        anonymous_component_paths: HashMap::new(),
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
        class_component_files: HashMap::new(),
    }
}

/// Mirror of the live resolver's existence check: a component "resolves" when
/// any candidate path exists on disk.
fn resolves(name: &str, config: &LaravelConfigData, autoload: &ComposerAutoload) -> bool {
    component_candidate_paths(name, config, autoload)
        .iter()
        .any(|p| p.exists())
}

#[test]
fn psr4_class_namespace_component_resolves_across_two_namespaces() {
    // Two separate package namespaces registered via componentNamespace, each
    // shipped under a PSR-4 vendor layout that the naive
    // `vendor/<Namespace>/...` guess in resolve_component_path can't find.
    // Both `<x-filament::badge>` and `<x-nightshade::alert-banner>` must
    // resolve through the autoload map (issue #69 — at least two namespaces).
    let installed = r#"{
        "packages": [
            {
                "name": "filament/support",
                "autoload": { "psr-4": { "Filament\\Support\\": "src/" } },
                "install-path": "../filament/support"
            },
            {
                "name": "nightshade/ui",
                "autoload": { "psr-4": { "Nightshade\\Ui\\": "src/" } },
                "install-path": "../nightshade/ui"
            }
        ]
    }"#;
    let (_dir, root) = project_with_files(&[
        ("vendor/composer/installed.json", installed),
        (
            "vendor/filament/support/src/View/Components/Badge.php",
            "<?php namespace Filament\\Support\\View\\Components; class Badge {}",
        ),
        (
            "vendor/nightshade/ui/src/View/Components/AlertBanner.php",
            "<?php namespace Nightshade\\Ui\\View\\Components; class AlertBanner {}",
        ),
    ]);
    let autoload = ComposerAutoload::load(&root);
    let config = config_with_component_namespaces(
        &root,
        &[
            ("filament", "Filament\\Support\\View\\Components"),
            ("nightshade", "Nightshade\\Ui\\View\\Components"),
        ],
    );

    assert!(
        resolves("filament::badge", &config, &autoload),
        "filament::badge must resolve via PSR-4 autoload: {:#?}",
        component_candidate_paths("filament::badge", &config, &autoload),
    );
    // kebab tag → PascalCase class file under the same namespace.
    assert!(
        resolves("nightshade::alert-banner", &config, &autoload),
        "nightshade::alert-banner must resolve via PSR-4 autoload: {:#?}",
        component_candidate_paths("nightshade::alert-banner", &config, &autoload),
    );
}

#[test]
fn psr4_class_namespace_resolves_dotted_subnamespace() {
    // `<x-filament::forms.text-input>` → Forms/TextInput.php under the
    // registered namespace.
    let installed = r#"{
        "packages": [
            {
                "name": "filament/forms",
                "autoload": { "psr-4": { "Filament\\Forms\\": "src/" } },
                "install-path": "../filament/forms"
            }
        ]
    }"#;
    let (_dir, root) = project_with_files(&[
        ("vendor/composer/installed.json", installed),
        (
            "vendor/filament/forms/src/View/Components/Forms/TextInput.php",
            "<?php namespace Filament\\Forms\\View\\Components\\Forms; class TextInput {}",
        ),
    ]);
    let autoload = ComposerAutoload::load(&root);
    let config = config_with_component_namespaces(
        &root,
        &[("filament", "Filament\\Forms\\View\\Components")],
    );

    assert!(
        resolves("filament::forms.text-input", &config, &autoload),
        "dotted namespaced component must map to a sub-namespaced class: {:#?}",
        component_candidate_paths("filament::forms.text-input", &config, &autoload),
    );
}

#[test]
fn missing_namespaced_component_still_reports_not_found() {
    // A registered namespace whose class file does NOT exist must NOT resolve —
    // diagnostics still fire (issue #69, no false negatives).
    let installed = r#"{
        "packages": [
            {
                "name": "filament/support",
                "autoload": { "psr-4": { "Filament\\Support\\": "src/" } },
                "install-path": "../filament/support"
            }
        ]
    }"#;
    let (_dir, root) = project_with_files(&[
        ("vendor/composer/installed.json", installed),
        (
            "vendor/filament/support/src/View/Components/Badge.php",
            "<?php class Badge {}",
        ),
    ]);
    let autoload = ComposerAutoload::load(&root);
    let config = config_with_component_namespaces(
        &root,
        &[("filament", "Filament\\Support\\View\\Components")],
    );

    assert!(
        !resolves("filament::does-not-exist", &config, &autoload),
        "a namespaced component with no backing file must not resolve",
    );
    // An entirely unregistered namespace must not resolve either.
    assert!(
        !resolves("unknown::widget", &config, &autoload),
        "an unregistered namespace must not resolve",
    );
}

// ─── Member-access capture data model (M2) ──────────────────────────────

/// A captured property-form access with the capture-time defaults (the
/// resolution scaffold left unfilled, as M2 leaves it).
fn unresolved_member_access(member: &str, line: u32, col: u32) -> MemberAccessReferenceData {
    MemberAccessReferenceData {
        member: member.into(),
        receiver: "$user".into(),
        receiver_byte_start: 0,
        receiver_byte_end: 5,
        is_nullsafe: false,
        form: AccessForm::Property,
        line,
        column: col,
        end_column: col + member.len() as u32,
        declaring_fqcn: None,
        kind: None,
        confidence: Confidence::Unresolved,
    }
}

#[test]
fn member_access_is_indexed_and_found_at_position() {
    let mut p = ParsedPatternsData::default();
    // `$user->email` — member name spans cols 12..17 on row 1.
    p.member_access_refs
        .push(Arc::new(unresolved_member_access("email", 1, 12)));
    p.build_position_index();

    let found = p
        .find_at_position(1, 14)
        .expect("cursor inside the member name should hit the access");
    match found {
        PatternAtPosition::MemberAccess(m) => assert_eq!(m.member, "email"),
        other => panic!("expected MemberAccess, got {other:?}"),
    }

    // Cursor before the member name (on the receiver) must not match —
    // the access is indexed at the member span, not the whole expression.
    assert!(p.find_at_position(1, 2).is_none());
}

#[test]
fn member_access_capture_defaults_are_unresolved() {
    let m = unresolved_member_access("email", 1, 12);
    assert!(m.declaring_fqcn.is_none());
    assert!(m.kind.is_none());
    assert_eq!(m.confidence, Confidence::Unresolved);
    assert_eq!(Confidence::default(), Confidence::Unresolved);
}

#[test]
fn member_access_deserializes_without_resolution_fields() {
    // A disk-cache entry written before the resolution scaffold existed:
    // only the capture fields are present. `#[serde(default)]` must fill the
    // rest rather than failing the whole entry.
    let json = r#"{
        "member": "email",
        "receiver": "$user",
        "receiver_byte_start": 0,
        "receiver_byte_end": 5,
        "is_nullsafe": false,
        "line": 1,
        "column": 12,
        "end_column": 17
    }"#;
    let m: MemberAccessReferenceData =
        serde_json::from_str(json).expect("legacy entry should deserialize");
    assert_eq!(m.member, "email");
    assert!(m.declaring_fqcn.is_none());
    assert!(m.kind.is_none());
    assert_eq!(m.confidence, Confidence::Unresolved);
}

#[test]
fn member_access_resolution_fields_round_trip() {
    // Once M3 fills the scaffold, it must survive (de)serialization.
    let mut m = unresolved_member_access("active", 3, 8);
    m.declaring_fqcn = Some("App\\Models\\User".into());
    m.kind = Some(MagicMemberKind::Scope);
    m.confidence = Confidence::High;

    let json = serde_json::to_string(&m).expect("serialize");
    let back: MemberAccessReferenceData = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(back.declaring_fqcn.as_deref(), Some("App\\Models\\User"));
    assert_eq!(back.kind, Some(MagicMemberKind::Scope));
    assert_eq!(back.confidence, Confidence::High);
}

// ─── Project source-file discovery (magic-member index breadth) ─────────

#[test]
fn collect_source_files_covers_app_and_views_skips_vendor() {
    use std::fs;
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let write = |rel: &str| {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, "<?php\n").unwrap();
        p
    };

    let model = write("app/Models/User.php");
    let provider = write("app/Providers/HorizonServiceProvider.php");
    let volt = write("resources/views/pages/users.php"); // Volt .php page
    let blade = write("resources/views/welcome.blade.php");
    let migration = write("database/migrations/2020_create_users.php");
    let vendor = write("vendor/laravel/framework/User.php"); // excluded
    let node = write("node_modules/pkg/x.php"); // excluded
    write("public/app.js"); // non-php, ignored

    let found = collect_source_files(root);

    // Included: app source (all of it), Volt .php under views, Blade, database.
    for p in [&model, &provider, &volt, &blade, &migration] {
        assert!(found.contains(p), "expected {p:?} in {found:?}");
    }
    // Excluded: vendor + node_modules.
    assert!(!found.contains(&vendor), "vendor must be skipped");
    assert!(!found.contains(&node), "node_modules must be skipped");
}

// ─── Blade @foreach iterable member-access capture ──────────────────────

#[test]
fn blade_loop_iterable_captures_this_member_access() {
    let content =
        "<div>\n    @foreach ($this->entities as $entity)\n        {{ $entity->name }}\n    @endforeach\n</div>\n";
    let accesses = blade_loop_iterable_accesses(content);
    assert_eq!(accesses.len(), 1, "got {accesses:?}");
    let a = &accesses[0];
    assert_eq!(a.member, "entities");
    assert_eq!(a.receiver, "$this");
    assert_eq!(a.line, 1); // 0-based; @foreach is the 2nd line
    let line = content.lines().nth(1).unwrap();
    assert_eq!(a.column, line.find("entities").unwrap() as u32);
    assert_eq!(a.end_column, a.column + "entities".len() as u32);
}

#[test]
fn blade_loop_iterable_bare_var_has_no_member_access() {
    // `@foreach($users as $user)` — a bare collection var, no `->member`.
    let content = "@foreach ($users as $user)\n{{ $user->x }}\n@endforeach\n";
    assert!(blade_loop_iterable_accesses(content).is_empty());
}

// ─── Generic builder-form view-namespace discovery (issue #69) ──────────
//
// Packages registered through a fluent package-builder (e.g. Filament via
// laravel-package-tools) declare views with `->name('x')->hasViews()`, and the
// real `loadViewsFrom` runs in a base class with runtime args — invisible to
// the literal `loadViewsFrom(__DIR__.'lit','lit')` extractor. These tests pin
// the builder-form recognizer that reconstructs the (namespace, directory)
// registration so `<x-x::component>` resolves through the existing
// view-namespace path.

#[test]
fn builder_short_name_strips_leading_laravel_prefix() {
    assert_eq!(builder_short_name("filament"), "filament");
    assert_eq!(builder_short_name("laravel-foo"), "foo");
    assert_eq!(builder_short_name("my-laravel-bar"), "bar");
}

/// Collect the (namespace, view_path) pairs a provider source registers.
fn discovered_view_namespaces(source: &str, provider_path: &str) -> Vec<(String, Option<PathBuf>)> {
    let db = LaravelDatabase::default();
    let file =
        ServiceProviderFile::new(&db, PathBuf::from(provider_path), 0, source.to_string(), 1);
    let parsed = parse_service_provider_source(&db, file, PathBuf::from("/proj"));
    parsed
        .view_namespaces(&db)
        .iter()
        .map(|vn| {
            (
                vn.namespace(&db).namespace(&db).clone(),
                vn.view_path(&db).clone(),
            )
        })
        .collect()
}

#[test]
fn builder_hasviews_registers_namespace_at_package_resources_views() {
    // Provider in `<pkg>/src` → views at `<pkg>/resources/views` (normalized,
    // since the path doesn't exist on disk in this unit test).
    let source = r#"<?php
class WidgetsServiceProvider extends PackageServiceProvider
{
    public function configurePackage(Package $package): void
    {
        $package->name('acme')->hasViews();
    }
}"#;
    let found = discovered_view_namespaces(
        source,
        "/proj/vendor/acme/widgets/src/WidgetsServiceProvider.php",
    );
    assert!(
        found.iter().any(|(ns, p)| ns == "acme"
            && *p == Some(PathBuf::from("/proj/vendor/acme/widgets/resources/views"))),
        "->name('acme')->hasViews() must register 'acme' → package resources/views, got {found:?}"
    );
}

#[test]
fn builder_hasviews_explicit_namespace_overrides_package_name() {
    let source = r#"<?php
class P extends PackageServiceProvider
{
    public function configurePackage(Package $package): void
    {
        $package->name('acme')->hasViews('custom');
    }
}"#;
    let found = discovered_view_namespaces(source, "/proj/vendor/acme/pkg/src/P.php");
    assert!(
        found.iter().any(|(ns, _)| ns == "custom"),
        "explicit ->hasViews('custom') must win over the package name, got {found:?}"
    );
    assert!(
        !found.iter().any(|(ns, _)| ns == "acme"),
        "the package name must not also register when an explicit namespace is given, got {found:?}"
    );
}

#[test]
fn builder_hasviews_strips_laravel_prefix_for_namespace() {
    let source = r#"<?php
class P extends PackageServiceProvider
{
    public function configurePackage(Package $package): void
    {
        $package->name('laravel-widgets')->hasViews();
    }
}"#;
    let found = discovered_view_namespaces(source, "/proj/vendor/acme/widgets/src/P.php");
    assert!(
        found.iter().any(|(ns, _)| ns == "widgets"),
        "->name('laravel-widgets') must register namespace 'widgets', got {found:?}"
    );
}

#[test]
fn builder_name_without_hasviews_registers_no_view_namespace() {
    // A package that declares a name + commands but no views must not have a
    // view namespace synthesized from its `->name()` call.
    let source = r#"<?php
class P extends PackageServiceProvider
{
    public function configurePackage(Package $package): void
    {
        $package->name('acme')->hasCommands([SomeCommand::class]);
    }
}"#;
    let found = discovered_view_namespaces(source, "/proj/vendor/acme/pkg/src/P.php");
    assert!(
        found.is_empty(),
        "no ->hasViews() means no builder-form view namespace, got {found:?}"
    );
}

#[test]
fn builder_discovered_namespace_resolves_anonymous_view_component() {
    // End-to-end: a real builder provider + a real package view file. Discovery
    // must register the namespace so `resolve_component_path` finds the view —
    // the exact `<x-filament::input.wrapper>` failure from issue #69.
    let provider = r#"<?php
class SupportServiceProvider extends PackageServiceProvider
{
    public function configurePackage(Package $package): void
    {
        $package->name('acme')->hasViews();
    }
}"#;
    let (_dir, root) = project_with_files(&[
        (
            "vendor/acme/support/src/SupportServiceProvider.php",
            provider,
        ),
        (
            "vendor/acme/support/resources/views/components/input/wrapper.blade.php",
            "<div>{{ $slot }}</div>",
        ),
    ]);

    // Discover the namespace from the provider on disk.
    let db = LaravelDatabase::default();
    let provider_path = root.join("vendor/acme/support/src/SupportServiceProvider.php");
    let text = std::fs::read_to_string(&provider_path).unwrap();
    let file = ServiceProviderFile::new(&db, provider_path, 0, text, 1);
    let parsed = parse_service_provider_source(&db, file, root.clone());

    let mut view_namespaces = HashMap::new();
    for vn in parsed.view_namespaces(&db) {
        if let Some(p) = vn.view_path(&db).clone() {
            view_namespaces.insert(vn.namespace(&db).namespace(&db).clone(), p);
        }
    }
    assert!(
        view_namespaces.contains_key("acme"),
        "discovery must register the 'acme' view namespace, got {view_namespaces:?}"
    );

    // Build a config carrying that namespace and resolve the dotted component.
    let config = LaravelConfigData {
        root: root.clone(),
        view_paths: vec![PathBuf::from("resources/views")],
        component_paths: Vec::new(),
        livewire_path: None,
        has_livewire: false,
        view_namespaces,
        component_namespaces: HashMap::new(),
        anonymous_component_paths: HashMap::new(),
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
        class_component_files: HashMap::new(),
    };

    let candidates = config.resolve_component_path("acme::input.wrapper");
    assert!(
        candidates.iter().any(|p| p.exists()),
        "acme::input.wrapper must resolve to the real package view via the \
         discovered namespace: {candidates:#?}"
    );
}

// ─── Imperative View::addNamespace() view-namespace discovery (issue #72) ─
//
// A namespace registered at runtime via `View::addNamespace('ns', <path>)`
// (or the `app('view')->`/`$factory->` receivers, or `prependNamespace`) is
// invisible to the literal `loadViewsFrom(__DIR__.'…', 'ns')` extractor, so
// `view('ns::name')` falsely reported "View file not found" and go-to-definition
// failed. These tests pin the imperative extractor that resolves the directory
// argument through the shared path-expression resolver (`app_path()`,
// `base_path()`, `resource_path()`, `__DIR__.'…'`, literals).

#[test]
fn extract_add_namespace_reads_app_path_registration() {
    let src = r#"
        public function boot(): void
        {
            View::addNamespace('ai-prompts', app_path('Ai/Prompts'));
        }
    "#;
    let root = PathBuf::from("/project");
    let provider_dir = PathBuf::from("/project/app/Providers");

    let regs = extract_add_namespace_view_registrations(src, &root, &provider_dir);

    assert_eq!(regs.len(), 1);
    assert_eq!(regs[0].0, "ai-prompts");
    assert_eq!(regs[0].1, PathBuf::from("/project/app/Ai/Prompts"));
}

#[test]
fn extract_add_namespace_handles_all_path_helper_forms() {
    let src = r#"
        View::addNamespace('with-app', app_path('Views/A'));
        View::addNamespace('with-base', base_path('packages/b/views'));
        View::addNamespace('with-resource', resource_path('views/c'));
        View::addNamespace('with-dir', __DIR__ . '/../views');
    "#;
    let root = PathBuf::from("/project");
    let provider_dir = PathBuf::from("/project/app/Providers");

    let regs = extract_add_namespace_view_registrations(src, &root, &provider_dir);
    let by_ns: std::collections::HashMap<_, _> = regs
        .iter()
        .map(|(ns, dir, _)| (ns.as_str(), dir.clone()))
        .collect();

    assert_eq!(by_ns["with-app"], PathBuf::from("/project/app/Views/A"));
    assert_eq!(
        by_ns["with-base"],
        PathBuf::from("/project/packages/b/views")
    );
    assert_eq!(
        by_ns["with-resource"],
        PathBuf::from("/project/resources/views/c")
    );
    // __DIR__ resolves against the provider directory.
    assert_eq!(by_ns["with-dir"], PathBuf::from("/project/app/views"));
}

#[test]
fn extract_add_namespace_supports_factory_receivers_and_prepend() {
    let src = r#"
        app('view')->addNamespace('via-app', resource_path('views/a'));
        $factory->prependNamespace('via-factory', base_path('b'));
    "#;
    let root = PathBuf::from("/project");
    let provider_dir = PathBuf::from("/project/app/Providers");

    let regs = extract_add_namespace_view_registrations(src, &root, &provider_dir);
    let names: Vec<&str> = regs.iter().map(|(ns, _, _)| ns.as_str()).collect();

    assert!(
        names.contains(&"via-app"),
        "app('view')-> form must register, got {names:?}"
    );
    assert!(
        names.contains(&"via-factory"),
        "$factory->prependNamespace form must register, got {names:?}"
    );
}

#[test]
fn extract_add_namespace_skips_unresolvable_path_argument() {
    // A variable directory can't be resolved statically — must be skipped, not
    // registered with a bogus path.
    let src = "View::addNamespace('dynamic', $this->promptPath);";
    let root = PathBuf::from("/project");
    let provider_dir = PathBuf::from("/project/app/Providers");

    let regs = extract_add_namespace_view_registrations(src, &root, &provider_dir);

    assert!(
        regs.is_empty(),
        "unresolvable path must be skipped, got {regs:?}"
    );
}

#[test]
fn add_namespace_registers_view_namespace_through_provider_parse() {
    let source = r#"<?php
class AppServiceProvider extends ServiceProvider
{
    public function boot(): void
    {
        View::addNamespace('ai-prompts', app_path('Ai/Prompts'));
    }
}"#;
    let found = discovered_view_namespaces(source, "/proj/app/Providers/AppServiceProvider.php");
    assert!(
        found
            .iter()
            .any(|(ns, p)| ns == "ai-prompts" && *p == Some(PathBuf::from("/proj/app/Ai/Prompts"))),
        "View::addNamespace must register 'ai-prompts' → app/Ai/Prompts, got {found:?}"
    );
}

#[test]
fn add_namespace_resolves_view_path_for_two_namespaces_end_to_end() {
    // Two distinct namespaces registered via View::addNamespace, each backed by
    // a real blade file. The exact issue #72 failure: view('ns::name') must
    // resolve to the registered directory instead of falling back to the
    // resources/views/vendor convention.
    let provider = r#"<?php
class AppServiceProvider extends ServiceProvider
{
    public function boot(): void
    {
        View::addNamespace('ai-prompts', app_path('Ai/Prompts'));
        View::addNamespace('reports', resource_path('report-views'));
    }
}"#;
    let (_dir, root) = project_with_files(&[
        ("app/Providers/AppServiceProvider.php", provider),
        (
            "app/Ai/Prompts/candidate-proposer.blade.php",
            "{{ $topic }}",
        ),
        ("resources/report-views/monthly.blade.php", "report"),
    ]);

    let db = LaravelDatabase::default();
    let provider_path = root.join("app/Providers/AppServiceProvider.php");
    let text = std::fs::read_to_string(&provider_path).unwrap();
    let file = ServiceProviderFile::new(&db, provider_path, 0, text, 1);
    let parsed = parse_service_provider_source(&db, file, root.clone());

    let mut view_namespaces = HashMap::new();
    for vn in parsed.view_namespaces(&db) {
        if let Some(p) = vn.view_path(&db).clone() {
            view_namespaces.insert(vn.namespace(&db).namespace(&db).clone(), p);
        }
    }
    assert!(
        view_namespaces.contains_key("ai-prompts") && view_namespaces.contains_key("reports"),
        "both namespaces must be discovered, got {view_namespaces:?}"
    );

    let config = LaravelConfigData {
        root: root.clone(),
        view_paths: vec![PathBuf::from("resources/views")],
        component_paths: Vec::new(),
        livewire_path: None,
        has_livewire: false,
        view_namespaces,
        component_namespaces: HashMap::new(),
        anonymous_component_paths: HashMap::new(),
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
        class_component_files: HashMap::new(),
    };

    // Both namespaced views resolve to their registered, real files.
    let prompts = config.resolve_view_path("ai-prompts::candidate-proposer");
    assert!(
        prompts.iter().any(|p| p.exists()),
        "ai-prompts::candidate-proposer must resolve to the registered file: {prompts:#?}"
    );
    let reports = config.resolve_view_path("reports::monthly");
    assert!(
        reports.iter().any(|p| p.exists()),
        "reports::monthly must resolve to the registered file: {reports:#?}"
    );

    // An invalid view under a registered namespace still has no real candidate —
    // the diagnostic must keep firing (no false negative).
    let missing = config.resolve_view_path("ai-prompts::does-not-exist");
    assert!(
        !missing.is_empty() && !missing.iter().any(|p| p.exists()),
        "an invalid namespaced view must still produce only non-existent candidates: {missing:#?}"
    );
}

// ─── Class-backed component registrations (dynamic-component, issue #69) ─
//
// Laravel core registers `<x-dynamic-component>` with an ordinary class
// alias — `$blade->component('dynamic-component', DynamicComponent::class)`
// inside ViewServiceProvider — using the *instance* receiver and a *short*
// class name resolved by a `use` import. These tests pin the broadened
// registration parsing (both receivers, both argument orders, use-statement
// expansion) and the shared-resolver consumption of the resulting map.

#[test]
fn expand_class_via_use_statements_resolves_short_names() {
    let source = r#"<?php
namespace Illuminate\View;

use Illuminate\View\DynamicComponent;
use Foo\Bar as Baz;
use function array_map;

class P {}
"#;
    assert_eq!(
        expand_class_via_use_statements("DynamicComponent", source),
        "Illuminate\\View\\DynamicComponent"
    );
    // Aliased import resolves through the alias.
    assert_eq!(expand_class_via_use_statements("Baz", source), "Foo\\Bar");
    // Already-qualified names pass through untouched.
    assert_eq!(
        expand_class_via_use_statements("App\\View\\Alert", source),
        "App\\View\\Alert"
    );
    // No matching import → unchanged (resolution fails downstream, as before).
    assert_eq!(
        expand_class_via_use_statements("Unknown", source),
        "Unknown"
    );
}

/// Parse a provider source against a root and return (tag, class, file) per
/// class-backed component registration.
fn parsed_blade_components(
    source: &str,
    provider_path: PathBuf,
    root: PathBuf,
) -> Vec<(String, String, Option<PathBuf>)> {
    let db = LaravelDatabase::default();
    let file = ServiceProviderFile::new(&db, provider_path, 0, source.to_string(), 0);
    let parsed = parse_service_provider_source(&db, file, root);
    parsed
        .blade_components(&db)
        .iter()
        .map(|bc| {
            (
                bc.tag_name(&db).name(&db).clone(),
                bc.class_name(&db).clone(),
                bc.file_path(&db).clone(),
            )
        })
        .collect()
}

#[test]
fn instance_form_component_registration_is_discovered_and_resolved() {
    // The exact framework shape: instance receiver inside a tap() closure,
    // short class name brought in by a use import.
    let provider = r#"<?php
namespace Illuminate\View;

use Illuminate\View\DynamicComponent;

class ViewServiceProvider
{
    public function registerBladeCompiler()
    {
        $this->app->singleton('blade.compiler', function ($app) {
            return tap(new BladeCompiler(), function ($blade) {
                $blade->component('dynamic-component', DynamicComponent::class);
            });
        });
    }
}
"#;
    let (_dir, root) = project_with_files(&[(
        "vendor/laravel/framework/src/Illuminate/View/DynamicComponent.php",
        "<?php namespace Illuminate\\View; class DynamicComponent {}",
    )]);

    let found = parsed_blade_components(
        provider,
        root.join("vendor/laravel/framework/src/Illuminate/View/ViewServiceProvider.php"),
        root.clone(),
    );

    assert!(
        found.iter().any(|(tag, class, file)| {
            tag == "dynamic-component"
                && class == "Illuminate\\View\\DynamicComponent"
                && file
                    .as_ref()
                    .is_some_and(|f| f.ends_with("Illuminate/View/DynamicComponent.php"))
        }),
        "instance-form registration must be discovered with the use-expanded \
         FQN and a resolved file, got {found:?}"
    );
}

#[test]
fn class_first_argument_order_is_discovered() {
    // Canonical order: Blade::component(AlertComponent::class, 'alert').
    let provider = r#"<?php
use App\View\Components\AlertComponent;

class AppServiceProvider
{
    public function boot()
    {
        Blade::component(AlertComponent::class, 'alert');
    }
}
"#;
    let (_dir, root) = project_with_files(&[(
        "app/View/Components/AlertComponent.php",
        "<?php namespace App\\View\\Components; class AlertComponent {}",
    )]);

    let found = parsed_blade_components(
        provider,
        root.join("app/Providers/AppServiceProvider.php"),
        root.clone(),
    );

    assert!(
        found.iter().any(|(tag, class, file)| {
            tag == "alert" && class == "App\\View\\Components\\AlertComponent" && file.is_some()
        }),
        "class-first argument order must be discovered, got {found:?}"
    );
}

#[test]
fn class_component_registration_resolves_via_candidate_paths() {
    // End of the chain: a tag present in `class_component_files` must surface
    // from `component_candidate_paths`, so the shared diagnostic/goto resolver
    // stops flagging `<x-dynamic-component>`.
    let (_dir, root) = project_with_files(&[(
        "vendor/laravel/framework/src/Illuminate/View/DynamicComponent.php",
        "<?php namespace Illuminate\\View; class DynamicComponent {}",
    )]);
    let class_file = root.join("vendor/laravel/framework/src/Illuminate/View/DynamicComponent.php");

    let mut class_component_files = HashMap::new();
    class_component_files.insert("dynamic-component".to_string(), class_file.clone());

    let config = LaravelConfigData {
        root: root.clone(),
        view_paths: vec![PathBuf::from("resources/views")],
        component_paths: Vec::new(),
        livewire_path: None,
        has_livewire: false,
        view_namespaces: HashMap::new(),
        component_namespaces: HashMap::new(),
        anonymous_component_paths: HashMap::new(),
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
        class_component_files,
    };
    let autoload = ComposerAutoload::load(&root);

    let candidates = component_candidate_paths("dynamic-component", &config, &autoload);
    assert!(
        candidates.iter().any(|p| *p == class_file && p.exists()),
        "a class-registered tag must resolve to its class file via the shared \
         resolver: {candidates:#?}"
    );

    // An unregistered tag must not pick up the class file.
    let other = component_candidate_paths("some-other-component", &config, &autoload);
    assert!(
        !other.contains(&class_file),
        "class registrations must not bleed into unrelated lookups"
    );
}

// ─── Call-form magic members at the cursor (#77) ───────────────────────────
//
// End-to-end through the actor: register a model + a caller, then resolve the
// cursor on a CALL-form usage (`User::active()`, `$user->active()`). These
// exercise the `member_ref.form` plumbing — before #77 the resolvers
// hardcoded `AccessForm::Property`, so a scope call could never classify.

const SCOPE_MODEL_SRC: &str = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Builder;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public function scopeActive(Builder $query): Builder { return $query; }
}
"#;

const SCOPE_CALLER_SRC: &str = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class UserController {
    public function index() {
        return User::active()->get();
    }
}
"#;

/// `(line, column)` of the first `needle` occurrence, 0-based.
fn position_of(src: &str, needle: &str) -> (u32, u32) {
    for (row, line) in src.lines().enumerate() {
        if let Some(col) = line.find(needle) {
            return (row as u32, col as u32);
        }
    }
    panic!("{needle} not found in fixture");
}

/// Spawn an actor over a tempdir project holding the scope model + caller.
async fn scope_project() -> (TempDir, SalsaHandle, PathBuf) {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let model_path = root.join("app/Models/User.php");
    std::fs::create_dir_all(model_path.parent().unwrap()).unwrap();
    std::fs::write(&model_path, SCOPE_MODEL_SRC).unwrap();
    let caller_path = root.join("app/Http/Controllers/UserController.php");
    std::fs::create_dir_all(caller_path.parent().unwrap()).unwrap();
    std::fs::write(&caller_path, SCOPE_CALLER_SRC).unwrap();

    let handle = SalsaActor::spawn();
    handle
        .register_config_files(root.clone(), None, None, None)
        .await
        .unwrap();
    handle
        .update_file(model_path.clone(), 1, SCOPE_MODEL_SRC.to_string())
        .await
        .unwrap();
    handle
        .update_file(caller_path.clone(), 1, SCOPE_CALLER_SRC.to_string())
        .await
        .unwrap();
    // Patterns parse lazily; forcing the model's parse is what feeds the
    // class-hierarchy index (the on-demand population path).
    handle.get_patterns(model_path).await.unwrap();
    (dir, handle, caller_path)
}

#[tokio::test]
async fn resolve_magic_member_at_classifies_static_scope_call() {
    let (_dir, handle, caller_path) = scope_project().await;
    let (line, col) = position_of(SCOPE_CALLER_SRC, "active");

    let data = handle
        .resolve_magic_member_at(caller_path, line, col)
        .await
        .unwrap()
        .expect("scope call should resolve");
    assert_eq!(data.kind, MagicMemberKind::Scope);
    assert_eq!(data.declaring_fqcn, "App\\Models\\User");
    assert_eq!(data.member, "active");
    // Method-backed: both decl lines present so hover can slice the source.
    assert!(data.decl_file.is_some());
    assert!(data.decl_line.is_some());
    assert!(data.decl_end_line.is_some());
}

#[tokio::test]
async fn resolve_magic_member_rename_at_maps_scope_call_to_declaration() {
    let (_dir, handle, caller_path) = scope_project().await;
    let (line, col) = position_of(SCOPE_CALLER_SRC, "active");

    let data = handle
        .resolve_magic_member_rename_at(caller_path, line, col)
        .await
        .unwrap()
        .expect("scope call should be renameable");
    assert_eq!(data.method_name, "scopeActive");
    assert_eq!(data.member, "active");
    assert_eq!(data.kind, MagicMemberKind::Scope);
    assert!(data.decl_file.ends_with("app/Models/User.php"));
}

#[tokio::test]
async fn unclassified_call_does_not_become_tentative_column() {
    // `$user->somethingUnknown()` — receiver resolves to the model, but the
    // member classifies as nothing. The tentative-column fallback is a
    // property-read concept and must NOT fire for calls.
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let model_path = root.join("app/Models/User.php");
    std::fs::create_dir_all(model_path.parent().unwrap()).unwrap();
    std::fs::write(&model_path, SCOPE_MODEL_SRC).unwrap();
    let caller_src = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function x(User $user) { return $user->somethingUnknown(); }
}
"#;
    let caller_path = root.join("app/Http/Controllers/C.php");
    std::fs::create_dir_all(caller_path.parent().unwrap()).unwrap();
    std::fs::write(&caller_path, caller_src).unwrap();

    let handle = SalsaActor::spawn();
    handle
        .register_config_files(root.clone(), None, None, None)
        .await
        .unwrap();
    handle
        .update_file(model_path.clone(), 1, SCOPE_MODEL_SRC.to_string())
        .await
        .unwrap();
    handle
        .update_file(caller_path.clone(), 1, caller_src.to_string())
        .await
        .unwrap();
    // Force the model's parse so the receiver RESOLVES — otherwise this test
    // passes vacuously without exercising the call-form tentative gate.
    handle.get_patterns(model_path).await.unwrap();

    let (line, col) = position_of(caller_src, "somethingUnknown");
    let data = handle
        .resolve_magic_member_at(caller_path, line, col)
        .await
        .unwrap();
    assert!(
        data.is_none(),
        "an unclassified CALL must not resolve as a tentative column; got {data:?}"
    );
}

#[tokio::test]
async fn dynamic_finder_is_not_renameable() {
    // `whereEmail` has no declared method to rewrite — finder rename must be
    // refused BY KIND, not by accident of the candidate-method lookup
    // missing (PR #76 review finding).
    let model_src = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    protected $casts = ['email' => 'string'];
}
"#;
    let caller_src = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function find() { return User::whereEmail('a@b.test')->first(); }
}
"#;
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let model_path = root.join("app/Models/User.php");
    std::fs::create_dir_all(model_path.parent().unwrap()).unwrap();
    std::fs::write(&model_path, model_src).unwrap();
    let caller_path = root.join("app/Http/Controllers/C.php");
    std::fs::create_dir_all(caller_path.parent().unwrap()).unwrap();
    std::fs::write(&caller_path, caller_src).unwrap();

    let handle = SalsaActor::spawn();
    handle
        .register_config_files(root.clone(), None, None, None)
        .await
        .unwrap();
    handle
        .update_file(model_path.clone(), 1, model_src.to_string())
        .await
        .unwrap();
    handle
        .update_file(caller_path.clone(), 1, caller_src.to_string())
        .await
        .unwrap();
    handle.get_patterns(model_path).await.unwrap();

    let (line, col) = position_of(caller_src, "whereEmail");
    // Precondition: the finder itself resolves (hover/goto see it)...
    let hover = handle
        .resolve_magic_member_at(caller_path.clone(), line, col)
        .await
        .unwrap()
        .expect("finder should classify for hover/goto");
    assert_eq!(hover.kind, MagicMemberKind::DynamicFinder);
    // ...but rename refuses it by kind.
    let rename = handle
        .resolve_magic_member_rename_at(caller_path, line, col)
        .await
        .unwrap();
    assert!(
        rename.is_none(),
        "a dynamic finder must not be renameable; got {rename:?}"
    );
}
