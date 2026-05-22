use super::*;

/// Extract base_path(...) calls from a line (test helper)
fn extract_base_path(line: &str) -> Option<&str> {
    // Match: base_path('some/path') or base_path("some/path")
    if let Some(start) = line.find("base_path(") {
        let after = &line[start + 10..];
        if let Some(quote_start) = after.find(['\'', '"']) {
            let quote_char = after.chars().nth(quote_start)?;
            let after_quote = &after[quote_start + 1..];
            if let Some(quote_end) = after_quote.find(quote_char) {
                return Some(&after_quote[..quote_end]);
            }
        }
    }
    None
}

#[test]
fn test_kebab_to_pascal_case() {
    assert_eq!(kebab_to_pascal_case("user-profile"), "UserProfile");
    assert_eq!(kebab_to_pascal_case("admin-dashboard"), "AdminDashboard");
    assert_eq!(kebab_to_pascal_case("simple"), "Simple");
}

#[test]
fn test_extract_base_path() {
    let line = "base_path('resources/templates'),";
    assert_eq!(extract_base_path(line), Some("resources/templates"));

    let line = "base_path(\"some/other/path\"),";
    assert_eq!(extract_base_path(line), Some("some/other/path"));
}

#[test]
fn test_parse_component_aliases_extracts_string_pairs() {
    let source = r#"<?php
return [
'aliases' => [
    'light-button' => 'components.buttons.light-button',
    'danger-button' => 'components.buttons.danger-button',
],
];
"#;
    let mut aliases = HashMap::new();
    parse_component_aliases(source, &mut aliases);
    assert_eq!(
        aliases.get("light-button").map(String::as_str),
        Some("components.buttons.light-button"),
    );
    assert_eq!(
        aliases.get("danger-button").map(String::as_str),
        Some("components.buttons.danger-button"),
    );
}

#[test]
fn test_parse_component_aliases_skips_class_references() {
    let source = r#"<?php
return [
'aliases' => [
    'success-alert' => App\View\Components\Alerts\SuccessAlert::class,
    'light-button' => 'components.buttons.light-button',
],
];
"#;
    let mut aliases = HashMap::new();
    parse_component_aliases(source, &mut aliases);
    assert!(!aliases.contains_key("success-alert"));
    assert_eq!(
        aliases.get("light-button").map(String::as_str),
        Some("components.buttons.light-button"),
    );
}

#[test]
fn test_parse_component_aliases_honors_comments() {
    let source = r#"<?php
return [
'aliases' => [
    // 'commented-out' => 'components.commented',
    'real-button' => 'components.buttons.real',
],
];
"#;
    let mut aliases = HashMap::new();
    parse_component_aliases(source, &mut aliases);
    assert!(!aliases.contains_key("commented-out"));
    assert_eq!(
        aliases.get("real-button").map(String::as_str),
        Some("components.buttons.real"),
    );
}

#[test]
fn test_extract_provider_blade_aliases_instance_form() {
    let php = r#"<?php
namespace App\Providers;

class AppServiceProvider {
public function boot($blade) {
    $blade->component('components.buttons.light-button', 'light-button');
    $blade->component('components.alerts.danger', 'danger-alert');
}
}
"#;
    let mut aliases = HashMap::new();
    extract_provider_blade_aliases(php, &mut aliases);

    assert_eq!(
        aliases.get("light-button").map(String::as_str),
        Some("components.buttons.light-button"),
    );
    assert_eq!(
        aliases.get("danger-alert").map(String::as_str),
        Some("components.alerts.danger"),
    );
}

#[test]
fn test_extract_provider_blade_aliases_static_form() {
    let php = r#"<?php
namespace App\Providers;

use Illuminate\Support\Facades\Blade;

class AppServiceProvider {
public function boot() {
    Blade::component('components.modal', 'modal');
}
}
"#;
    let mut aliases = HashMap::new();
    extract_provider_blade_aliases(php, &mut aliases);

    assert_eq!(
        aliases.get("modal").map(String::as_str),
        Some("components.modal"),
    );
}

#[test]
fn test_extract_provider_blade_aliases_skips_class_fqn_view() {
    // When the first arg is a PHP class FQN (contains backslashes), it
    // points at a class-based component which the directory convention
    // handles. We skip those to avoid pretending they're view paths.
    let php = r#"<?php
namespace App\Providers;

class AppServiceProvider {
public function boot($blade) {
    $blade->component('App\\View\\Components\\Alert', 'alert-class');
    $blade->component('components.regular', 'regular');
}
}
"#;
    let mut aliases = HashMap::new();
    extract_provider_blade_aliases(php, &mut aliases);

    assert!(!aliases.contains_key("alert-class"));
    assert_eq!(
        aliases.get("regular").map(String::as_str),
        Some("components.regular"),
    );
}

#[test]
fn test_extract_provider_blade_aliases_ignores_loop_with_variables() {
    // The decisioncloud-style pattern (loop with variable args) cannot
    // produce literal captures and is properly handled by the config
    // file source instead. This verifies the extractor doesn't crash
    // or hallucinate aliases when args aren't literals.
    let php = r#"<?php
namespace App\Providers;

class AppServiceProvider {
public function boot($blade) {
    foreach (config('component.aliases', []) as $alias => $component) {
        $blade->component($component, $alias);
    }
}
}
"#;
    let mut aliases = HashMap::new();
    extract_provider_blade_aliases(php, &mut aliases);

    assert!(aliases.is_empty(), "no literal pairs to extract from variable args");
}

#[test]
fn test_scan_vendor_uncached_finds_provider_aliases() {
    use std::fs as std_fs;

    let tmp = std::env::temp_dir().join(format!(
        "laravel-lsp-test-vendor-{}",
        std::process::id(),
    ));
    let _ = std_fs::remove_dir_all(&tmp);

    let provider_dir = tmp.join("vendor/acme/widgets/src");
    std_fs::create_dir_all(&provider_dir).unwrap();

    let provider_php = r#"<?php
namespace Acme\Widgets;

use Illuminate\Support\Facades\Blade;
use Illuminate\Support\ServiceProvider;

class WidgetsServiceProvider extends ServiceProvider {
public function boot() {
    Blade::component('widgets.spinner', 'widget-spinner');
}
}
"#;
    std_fs::write(provider_dir.join("WidgetsServiceProvider.php"), provider_php).unwrap();

    // Non-provider file with no relevant calls — should be skipped.
    std_fs::write(
        provider_dir.join("SomeOtherClass.php"),
        "<?php namespace Acme\\Widgets; class SomeOtherClass {}",
    )
    .unwrap();

    let aliases = scan_vendor_uncached(&tmp);

    assert_eq!(
        aliases.get("widget-spinner").map(String::as_str),
        Some("widgets.spinner"),
    );

    let _ = std_fs::remove_dir_all(&tmp);
}

#[test]
fn test_scan_vendor_uncached_skips_non_serviceprovider_files() {
    use std::fs as std_fs;

    let tmp = std::env::temp_dir().join(format!(
        "laravel-lsp-test-vendor-skip-{}",
        std::process::id(),
    ));
    let _ = std_fs::remove_dir_all(&tmp);

    let pkg_dir = tmp.join("vendor/acme/lib/src");
    std_fs::create_dir_all(&pkg_dir).unwrap();

    // File contains a Blade::component call but isn't named like a
    // service provider — should be skipped by the filename gate.
    let helper_php = r#"<?php
namespace Acme\Lib;

class Helper {
public function setup($blade) {
    $blade->component('lib.thing', 'lib-thing');
}
}
"#;
    std_fs::write(pkg_dir.join("Helper.php"), helper_php).unwrap();

    let aliases = scan_vendor_uncached(&tmp);

    assert!(
        !aliases.contains_key("lib-thing"),
        "non-ServiceProvider files must be ignored",
    );

    let _ = std_fs::remove_dir_all(&tmp);
}

#[test]
fn test_scan_vendor_icons_finds_heroicon_style_set() {
    use std::fs as std_fs;

    let tmp = std::env::temp_dir().join(format!(
        "laravel-lsp-test-icons-{}",
        std::process::id(),
    ));
    let _ = std_fs::remove_dir_all(&tmp);

    // Replicate the heroicons layout: flat SVG dir + blade-*.php config
    // with 'prefix' => 'heroicon'.
    let pkg_dir = tmp.join("vendor/blade-ui-kit/blade-heroicons");
    let svg_dir = pkg_dir.join("resources/svg");
    let config_dir = pkg_dir.join("config");
    std_fs::create_dir_all(&svg_dir).unwrap();
    std_fs::create_dir_all(&config_dir).unwrap();

    std_fs::write(
        config_dir.join("blade-heroicons.php"),
        "<?php\nreturn [\n    'prefix' => 'heroicon',\n];\n",
    )
    .unwrap();

    // Drop a couple of SVG files matching the real heroicons naming.
    std_fs::write(svg_dir.join("o-clock.svg"), "<svg></svg>").unwrap();
    std_fs::write(svg_dir.join("s-bell.svg"), "<svg></svg>").unwrap();

    let icons = scan_vendor_icons_uncached(&tmp);

    assert!(
        icons.contains_key("heroicon-o-clock"),
        "expected heroicon-o-clock entry, got keys: {:?}",
        icons.keys().collect::<Vec<_>>(),
    );
    assert!(icons.contains_key("heroicon-s-bell"));
    assert!(
        icons["heroicon-o-clock"].ends_with("o-clock.svg"),
        "value should point to the svg file",
    );

    let _ = std_fs::remove_dir_all(&tmp);
}

#[test]
fn test_scan_vendor_icons_handles_nested_directories() {
    use std::fs as std_fs;

    let tmp = std::env::temp_dir().join(format!(
        "laravel-lsp-test-icons-nested-{}",
        std::process::id(),
    ));
    let _ = std_fs::remove_dir_all(&tmp);

    let pkg_dir = tmp.join("vendor/some-vendor/some-icons");
    let svg_dir = pkg_dir.join("resources/svg/outline");
    let config_dir = pkg_dir.join("config");
    std_fs::create_dir_all(&svg_dir).unwrap();
    std_fs::create_dir_all(&config_dir).unwrap();

    std_fs::write(
        config_dir.join("blade-some-icons.php"),
        "<?php return ['prefix' => 'someicon'];",
    )
    .unwrap();

    std_fs::write(svg_dir.join("user.svg"), "<svg></svg>").unwrap();

    let icons = scan_vendor_icons_uncached(&tmp);

    // Nested file `outline/user.svg` should produce tag `someicon-outline-user`.
    assert!(
        icons.contains_key("someicon-outline-user"),
        "nested dirs should produce dashed tag names, got: {:?}",
        icons.keys().collect::<Vec<_>>(),
    );

    let _ = std_fs::remove_dir_all(&tmp);
}

#[test]
fn test_scan_vendor_icons_skips_packages_without_prefix_config() {
    use std::fs as std_fs;

    let tmp = std::env::temp_dir().join(format!(
        "laravel-lsp-test-icons-noconfig-{}",
        std::process::id(),
    ));
    let _ = std_fs::remove_dir_all(&tmp);

    let pkg_dir = tmp.join("vendor/some-vendor/some-pkg");
    let svg_dir = pkg_dir.join("resources/svg");
    let config_dir = pkg_dir.join("config");
    std_fs::create_dir_all(&svg_dir).unwrap();
    std_fs::create_dir_all(&config_dir).unwrap();

    // Config file exists but no 'prefix' key — should be skipped.
    std_fs::write(
        config_dir.join("blade-something.php"),
        "<?php return ['something' => 'else'];",
    )
    .unwrap();
    std_fs::write(svg_dir.join("icon.svg"), "<svg></svg>").unwrap();

    let icons = scan_vendor_icons_uncached(&tmp);
    assert!(icons.is_empty(), "should not register icons without a declared prefix");

    let _ = std_fs::remove_dir_all(&tmp);
}

#[test]
fn test_scan_prefix_string_handles_both_quote_styles() {
    assert_eq!(scan_prefix_string("'prefix' => 'heroicon'"), Some("heroicon".into()));
    assert_eq!(scan_prefix_string("\"prefix\" => \"heroicon\""), Some("heroicon".into()));
    assert_eq!(scan_prefix_string("'prefix'=>'tight'"), Some("tight".into()));
    assert_eq!(scan_prefix_string("no prefix here"), None);
}

#[test]
fn test_scan_vendor_uncached_returns_empty_when_no_vendor() {
    let tmp = std::env::temp_dir().join(format!(
        "laravel-lsp-test-no-vendor-{}",
        std::process::id(),
    ));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    let aliases = scan_vendor_uncached(&tmp);
    assert!(aliases.is_empty());

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_parse_component_aliases_does_not_cross_into_sibling_keys() {
    // Ensures we walk bracket depth and stop at the closing ] of the aliases array.
    let source = r#"<?php
return [
'aliases' => [
    'light-button' => 'components.buttons.light-button',
],
'other-config' => [
    'unrelated-alias' => 'should.not.be.captured',
],
];
"#;
    let mut aliases = HashMap::new();
    parse_component_aliases(source, &mut aliases);
    assert!(aliases.contains_key("light-button"));
    assert!(!aliases.contains_key("unrelated-alias"));
}
