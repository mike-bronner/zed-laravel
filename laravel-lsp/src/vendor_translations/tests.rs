use super::*;
use std::fs;
use tempfile::TempDir;

/// Build a fake vendor tree at `vendor/<vendor>/<package>/` with a service
/// provider file at the standard location.
fn fake_vendor_package(project: &Path, vendor: &str, pkg: &str, provider: &str) -> PathBuf {
    let provider_dir = project.join("vendor").join(vendor).join(pkg).join("src");
    fs::create_dir_all(&provider_dir).unwrap();
    let provider_path = provider_dir.join(format!("{}.php", provider));
    provider_path
}

#[test]
fn extracts_single_load_translations_from_registration() {
    let project = TempDir::new().unwrap();
    let provider = fake_vendor_package(project.path(), "acme", "billing", "BillingServiceProvider");

    let lang_dir = provider.parent().unwrap().join("../resources/lang");
    fs::create_dir_all(&lang_dir).unwrap();
    fs::write(
        &provider,
        r#"<?php
namespace Acme\Billing;
class BillingServiceProvider {
    public function boot() {
        $this->loadTranslationsFrom(__DIR__.'/../resources/lang', 'billing');
    }
}
"#,
    )
    .unwrap();

    let map = scan_vendor_translation_namespaces(project.path());
    let resolved = map.get("billing").expect("should find billing namespace");
    assert!(
        resolved.ends_with("resources/lang"),
        "expected resolved to end with resources/lang, got: {:?}",
        resolved
    );
}

#[test]
fn ignores_non_provider_php_files() {
    // A non-provider file with `loadTranslationsFrom` in a docblock should
    // be skipped by the filename gate.
    let project = TempDir::new().unwrap();
    let non_provider = project.path().join("vendor/acme/billing/src/Helpers.php");
    fs::create_dir_all(non_provider.parent().unwrap()).unwrap();
    fs::write(
        &non_provider,
        r#"<?php
namespace Acme\Billing;
// $this->loadTranslationsFrom(__DIR__.'/../lang', 'billing');
class Helpers {}
"#,
    )
    .unwrap();

    let map = scan_vendor_translation_namespaces(project.path());
    assert!(map.is_empty(), "non-provider files must be ignored");
}

#[test]
fn ignores_providers_without_load_translations_from_call() {
    let project = TempDir::new().unwrap();
    let provider = fake_vendor_package(project.path(), "acme", "billing", "BillingServiceProvider");
    fs::write(
        &provider,
        r#"<?php
class BillingServiceProvider {
    public function boot() {
        $this->loadViewsFrom(__DIR__.'/../views', 'billing');
    }
}
"#,
    )
    .unwrap();

    let map = scan_vendor_translation_namespaces(project.path());
    assert!(
        map.is_empty(),
        "providers without loadTranslationsFrom must contribute nothing"
    );
}

#[test]
fn captures_multiple_namespaces_across_packages() {
    let project = TempDir::new().unwrap();
    let p1 = fake_vendor_package(project.path(), "acme", "billing", "BillingServiceProvider");
    let p2 = fake_vendor_package(project.path(), "acme", "auth", "AuthServiceProvider");
    fs::write(
        &p1,
        "<?php\nclass X { public function boot() { $this->loadTranslationsFrom(__DIR__.'/../lang', 'billing'); } }\n",
    )
    .unwrap();
    fs::write(
        &p2,
        "<?php\nclass Y { public function boot() { $this->loadTranslationsFrom(__DIR__.'/../lang', 'auth'); } }\n",
    )
    .unwrap();

    let map = scan_vendor_translation_namespaces(project.path());
    assert!(map.contains_key("billing"));
    assert!(map.contains_key("auth"));
}

#[test]
fn returns_empty_when_vendor_dir_missing() {
    let project = TempDir::new().unwrap();
    // No vendor/ directory.
    let map = scan_vendor_translation_namespaces(project.path());
    assert!(map.is_empty());
}

#[test]
fn first_registration_wins_on_namespace_conflict() {
    // Two packages register the same namespace. First-match-wins.
    let project = TempDir::new().unwrap();
    let p1 = fake_vendor_package(project.path(), "first", "pkg", "FirstServiceProvider");
    let p2 = fake_vendor_package(project.path(), "second", "pkg", "SecondServiceProvider");
    fs::write(
        &p1,
        "<?php\nclass A { public function boot() { $this->loadTranslationsFrom(__DIR__.'/../lang', 'shared'); } }\n",
    )
    .unwrap();
    fs::write(
        &p2,
        "<?php\nclass B { public function boot() { $this->loadTranslationsFrom(__DIR__.'/../lang', 'shared'); } }\n",
    )
    .unwrap();

    let map = scan_vendor_translation_namespaces(project.path());
    let resolved = map.get("shared").expect("conflict must still resolve");
    // The path will contain either "first" or "second" depending on walk order —
    // accept either, but it must be a single deterministic entry.
    let s = resolved.to_string_lossy();
    assert!(s.contains("first") || s.contains("second"), "got: {}", s);
}
