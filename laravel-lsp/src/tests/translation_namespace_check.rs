//! Namespaced translation keys (`package::file.key`) in the diagnostic's
//! existence check. The check delegates to `translation_lookup` with the
//! vendor-package map — the same machinery hover uses — so unpublished
//! package translations (e.g. `filament-tables::table.…`) stop false-flagging
//! while genuinely missing keys still do.

use crate::LaravelLanguageServer;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

/// A project whose vendor package ships `lang/en/table.php` with one key,
/// plus the vendor map the translation scan would have produced for it.
fn project_with_vendor_translations() -> (TempDir, PathBuf, HashMap<String, PathBuf>) {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let lang_dir = root.join("vendor/filament/tables/resources/lang");
    fs::create_dir_all(lang_dir.join("en")).unwrap();
    fs::write(
        lang_dir.join("en/table.php"),
        "<?php return ['grouping' => ['label' => 'Group']];",
    )
    .unwrap();

    let mut vendor_map = HashMap::new();
    vendor_map.insert("filament-tables".to_string(), lang_dir);
    (dir, root, vendor_map)
}

#[test]
fn namespaced_key_resolves_through_vendor_map() {
    let (_dir, root, vendor_map) = project_with_vendor_translations();

    let check = LaravelLanguageServer::check_translation_file(
        &root,
        "filament-tables::table.grouping.label",
        Some(&vendor_map),
    );
    assert!(
        check.exists,
        "an unpublished package translation must resolve via the vendor map"
    );
}

#[test]
fn missing_namespaced_key_still_flags_with_vendor_path() {
    let (_dir, root, vendor_map) = project_with_vendor_translations();

    let check = LaravelLanguageServer::check_translation_file(
        &root,
        "filament-tables::table.does.not.exist",
        Some(&vendor_map),
    );
    assert!(!check.exists, "a genuinely missing key must still flag");
    // The diagnostic should point at the package's real lang file, not the
    // bogus `lang/en/filament-tables::table.php` guess it used to emit.
    let expected = check.expected_path.expect("expected path set");
    assert!(
        expected.ends_with("vendor/filament/tables/resources/lang/en/table.php"),
        "expected path must target the package lang dir: {expected:?}"
    );
    assert!(
        check.file_exists,
        "the file itself exists — only the key is missing"
    );
}

#[test]
fn namespaced_key_without_vendor_map_expects_published_path() {
    let (_dir, root, _vendor_map) = project_with_vendor_translations();

    let check =
        LaravelLanguageServer::check_translation_file(&root, "unknown-pkg::messages.hi", None);
    assert!(!check.exists);
    let expected = check.expected_path.expect("expected path set");
    assert!(
        expected.ends_with("lang/vendor/unknown-pkg/en/messages.php"),
        "without a vendor-map hit the published location is the expectation: {expected:?}"
    );
}
