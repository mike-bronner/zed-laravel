//! Interpolated double-quoted strings: fragment skipping + constant
//! propagation for config keys.
//!
//! A string like `"{$config}.export_connection"` parses as an
//! `encapsed_string` whose literal fragment (`.export_connection`) sits next
//! to the interpolation node. Capturing the fragment alone produced phantom
//! keys ("Config not found: '.export_connection'") and zeroed reference
//! counts. Two behaviors are under test:
//!
//! 1. Every pattern kind SKIPS fragments of interpolated strings — no
//!    garbage view/route/translation/config keys.
//! 2. Config keys additionally try to reconstruct the full key when the
//!    interpolated variable resolves to a single same-scope literal
//!    assignment (lightweight constant propagation).

use crate::parser::{language_php, parse_php};
use crate::queries::extract_all_php_patterns;

fn config_keys(php_code: &str) -> Vec<String> {
    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns =
        extract_all_php_patterns(&tree, php_code, &lang).expect("Should extract patterns");
    patterns
        .config_calls
        .iter()
        .map(|c| c.config_key.to_string())
        .collect()
}

// ─── Constant propagation: resolvable cases ─────────────────────────────

#[test]
fn resolves_interpolated_config_key_from_same_scope_literal() {
    let php = r#"<?php
function sync() {
    $config = 'reporting.redshift_sync';
    $conn = config("{$config}.export_connection");
}
"#;
    assert_eq!(
        config_keys(php),
        vec!["reporting.redshift_sync.export_connection"]
    );
}

#[test]
fn resolves_interpolated_key_in_fluent_accessor() {
    let php = r#"<?php
class Exporter {
    public function connect() {
        $config = 'reporting.redshift_sync';
        $this->exportConnection = config()->string("{$config}.export_connection");
    }
}
"#;
    assert_eq!(
        config_keys(php),
        vec!["reporting.redshift_sync.export_connection"]
    );
}

#[test]
fn resolves_simple_unbraced_interpolation() {
    let php = r#"<?php
$prefix = 'app';
$name = config("$prefix.name");
"#;
    assert_eq!(config_keys(php), vec!["app.name"]);
}

#[test]
fn resolves_double_quoted_literal_assignment() {
    let php = r#"<?php
$config = "reporting.redshift_sync";
$x = config("{$config}.enabled");
"#;
    assert_eq!(config_keys(php), vec!["reporting.redshift_sync.enabled"]);
}

#[test]
fn resolved_key_position_spans_inner_string() {
    let php = "<?php\n$config = 'reporting.redshift_sync';\n$c = config(\"{$config}.export_connection\");\n";
    let tree = parse_php(php).expect("Should parse PHP");
    let lang = language_php();
    let patterns = extract_all_php_patterns(&tree, php, &lang).expect("Should extract patterns");
    assert_eq!(patterns.config_calls.len(), 1);
    let m = &patterns.config_calls[0];
    assert_eq!(m.row, 2);
    // Inner span starts right after the opening quote of the encapsed string.
    let line = php.lines().nth(2).unwrap();
    let quote = line.find('"').unwrap();
    assert_eq!(m.column, quote + 1);
    // …and ends right before the closing quote.
    let close = line.rfind('"').unwrap();
    assert_eq!(m.end_column, close);
}

// ─── Constant propagation: bail-out cases ───────────────────────────────

#[test]
fn skips_property_interpolation() {
    let php = r#"<?php
class Exporter {
    public function connect() {
        $x = config("{$this->prefix}.enabled");
    }
}
"#;
    assert!(config_keys(php).is_empty());
}

#[test]
fn skips_unassigned_variable() {
    let php = r#"<?php
function f($config) {
    $x = config("{$config}.enabled");
}
"#;
    assert!(config_keys(php).is_empty());
}

#[test]
fn skips_reassigned_variable() {
    let php = r#"<?php
$config = 'reporting.redshift_sync';
$config = 'reporting.other';
$x = config("{$config}.enabled");
"#;
    assert!(config_keys(php).is_empty());
}

#[test]
fn skips_assignment_after_use() {
    let php = r#"<?php
$x = config("{$config}.enabled");
$config = 'reporting.redshift_sync';
"#;
    assert!(config_keys(php).is_empty());
}

#[test]
fn skips_non_literal_assignment() {
    let php = r#"<?php
$config = get_prefix();
$x = config("{$config}.enabled");
"#;
    assert!(config_keys(php).is_empty());
}

#[test]
fn skips_assignment_from_other_function_scope() {
    let php = r#"<?php
function a() {
    $config = 'reporting.redshift_sync';
}
function b() {
    $x = config("{$config}.enabled");
}
"#;
    assert!(config_keys(php).is_empty());
}

#[test]
fn skips_interpolated_assignment_value() {
    let php = r#"<?php
$env = 'prod';
$config = "reporting.{$env}";
$x = config("{$config}.enabled");
"#;
    // $config's own value is interpolated — one level is supported, nesting
    // is not. Must bail, not resolve partially.
    assert!(config_keys(php).is_empty());
}

#[test]
fn plain_literal_keys_are_unaffected() {
    let php = r#"<?php
$a = config('app.name');
$b = config("database.default");
"#;
    assert_eq!(config_keys(php), vec!["app.name", "database.default"]);
}

// ─── Review findings (PR #84): dedup + binding-shape hardening ──────────

#[test]
fn middle_interpolation_resolves_exactly_once() {
    // Two literal fragments around one variable → two query captures of the
    // SAME encapsed string. Must yield one match, not a doubled count (B1).
    let php = r#"<?php
$provider = 'stripe';
$key = config("services.{$provider}.key");
"#;
    assert_eq!(config_keys(php), vec!["services.stripe.key"]);
}

#[test]
fn unresolvable_middle_interpolation_yields_nothing() {
    // Same two-fragment shape, but unresolvable — neither fragment may leak.
    let php = r#"<?php
$key = config("services.{$provider}.key");
"#;
    assert!(config_keys(php).is_empty());
}

#[test]
fn skips_augmented_assignment() {
    // `.=` mutates after the literal assignment — resolving the original
    // value would produce a stale key (S1).
    let php = r#"<?php
$config = 'reporting';
$config .= '.redshift_sync';
$x = config("{$config}.enabled");
"#;
    assert!(config_keys(php).is_empty());
}

#[test]
fn skips_foreach_rebinding() {
    let php = r#"<?php
$config = 'reporting.redshift_sync';
foreach ($sections as $config) {
    $x = config("{$config}.enabled");
}
"#;
    assert!(config_keys(php).is_empty());
}

#[test]
fn skips_foreach_key_rebinding() {
    let php = r#"<?php
$config = 'reporting.redshift_sync';
foreach ($map as $config => $value) {
    $x = config("{$config}.enabled");
}
"#;
    assert!(config_keys(php).is_empty());
}

#[test]
fn skips_destructuring_rebinding() {
    let php = r#"<?php
$config = 'reporting.redshift_sync';
[$other, $config] = $pair;
$x = config("{$config}.enabled");
"#;
    assert!(config_keys(php).is_empty());
}

#[test]
fn skips_static_declaration() {
    // `static` persists across calls — its initializer is not its value.
    let php = r#"<?php
function f() {
    static $config = 'reporting.redshift_sync';
    return config("{$config}.enabled");
}
"#;
    assert!(config_keys(php).is_empty());
}

#[test]
fn skips_conditional_assignment_shadowing_parameter() {
    // One branch assigns, the other path keeps the parameter — no single
    // provable value at the use site (S2).
    let php = r#"<?php
function f($config, $override) {
    if ($override) {
        $config = 'alt.prefix';
    }
    return config("{$config}.enabled");
}
"#;
    assert!(config_keys(php).is_empty());
}

#[test]
fn resolves_unconditional_reassignment_of_parameter() {
    // Counterpoint to the conditional case: a direct, unconditional literal
    // assignment before the use IS the value, parameter or not.
    let php = r#"<?php
function f($config) {
    $config = 'reporting.redshift_sync';
    return config("{$config}.enabled");
}
"#;
    assert_eq!(config_keys(php), vec!["reporting.redshift_sync.enabled"]);
}

// ─── Fragment skipping for non-config patterns ──────────────────────────

#[test]
fn skips_interpolated_fragments_for_all_pattern_kinds() {
    let php = r#"<?php
$v = view("emails.{$type}");
$r = route("admin.{$section}.index");
$t = __("messages.{$key}");
$e = env("APP_{$suffix}");
$a = asset("css/{$theme}.css");
"#;
    let tree = parse_php(php).expect("Should parse PHP");
    let lang = language_php();
    let patterns = extract_all_php_patterns(&tree, php, &lang).expect("Should extract patterns");

    assert!(patterns.views.is_empty(), "views: {:?}", patterns.views);
    assert!(
        patterns.route_calls.is_empty(),
        "routes: {:?}",
        patterns.route_calls
    );
    assert!(
        patterns.translation_calls.is_empty(),
        "translations: {:?}",
        patterns.translation_calls
    );
    assert!(
        patterns.env_calls.is_empty(),
        "env: {:?}",
        patterns.env_calls
    );
    assert!(
        patterns.asset_calls.is_empty(),
        "assets: {:?}",
        patterns.asset_calls
    );
}
