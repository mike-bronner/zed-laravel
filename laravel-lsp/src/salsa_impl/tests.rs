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
