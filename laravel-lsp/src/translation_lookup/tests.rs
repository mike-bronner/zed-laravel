use super::*;
use std::fs;
use tempfile::TempDir;

/// Build a fake Laravel project root with a `lang/` directory ready for tests.
fn fake_project_with_lang() -> TempDir {
    let dir = TempDir::new().unwrap();
    let lang = dir.path().join("lang");
    fs::create_dir_all(lang.join("en")).unwrap();
    dir
}

#[test]
fn resolves_dotted_key_from_php_file() {
    let project = fake_project_with_lang();
    let validation = project.path().join("lang/en/validation.php");
    fs::write(
        &validation,
        "<?php\nreturn [\n    'required' => 'The :attribute field is required.',\n];\n",
    )
    .unwrap();

    let got = resolve_translation(project.path(), "validation.required", "en");
    assert_eq!(got.as_deref(), Some("'The :attribute field is required.'"));
}

#[test]
fn resolves_nested_dotted_key_from_php_file() {
    let project = fake_project_with_lang();
    let auth = project.path().join("lang/en/auth.php");
    fs::write(
        &auth,
        "<?php\nreturn [\n    'failed' => 'These credentials do not match.',\n    'throttle' => [\n        'message' => 'Too many attempts.',\n    ],\n];\n",
    )
    .unwrap();

    assert_eq!(
        resolve_translation(project.path(), "auth.failed", "en").as_deref(),
        Some("'These credentials do not match.'")
    );
    assert_eq!(
        resolve_translation(project.path(), "auth.throttle.message", "en").as_deref(),
        Some("'Too many attempts.'")
    );
}

#[test]
fn resolves_text_key_from_json_file() {
    let project = fake_project_with_lang();
    let json = project.path().join("lang/en.json");
    fs::write(
        &json,
        r#"{
    "Welcome to our app": "Welcome to our app",
    "Sign in": "Sign in"
}
"#,
    )
    .unwrap();

    assert_eq!(
        resolve_translation(project.path(), "Welcome to our app", "en").as_deref(),
        Some("'Welcome to our app'")
    );
}

#[test]
fn returns_none_for_missing_dotted_key() {
    let project = fake_project_with_lang();
    let validation = project.path().join("lang/en/validation.php");
    fs::write(&validation, "<?php\nreturn ['present' => 'x'];\n").unwrap();

    assert_eq!(
        resolve_translation(project.path(), "validation.missing", "en"),
        None
    );
}

#[test]
fn returns_none_for_missing_json_key() {
    let project = fake_project_with_lang();
    let json = project.path().join("lang/en.json");
    fs::write(&json, r#"{"Present": "Present"}"#).unwrap();

    assert_eq!(
        resolve_translation(project.path(), "Missing entry", "en"),
        None
    );
}

#[test]
fn returns_none_when_file_does_not_exist() {
    let project = fake_project_with_lang();
    // No files written.
    assert_eq!(
        resolve_translation(project.path(), "validation.required", "en"),
        None
    );
    assert_eq!(
        resolve_translation(project.path(), "Free-form text", "en"),
        None
    );
}

#[test]
fn dotted_key_classifier_distinguishes_shapes() {
    assert!(is_dotted_key("validation.required"));
    assert!(is_dotted_key("auth.throttle.message"));
    // A user-facing sentence with a period — has spaces, so treated as a text key.
    assert!(!is_dotted_key("Welcome to our app."));
    // No dot, no spaces — treat as text key (degenerate case).
    assert!(!is_dotted_key("single"));
}

#[test]
fn namespace_splitter_separates_vendor_from_rest() {
    assert_eq!(
        split_namespace("filament-tables::table.actions.label"),
        Some(("filament-tables", "table.actions.label"))
    );
    assert_eq!(split_namespace("validation.required"), None);
    assert_eq!(split_namespace("plain text"), None);
}

#[test]
fn resolves_namespaced_translation_from_published_path() {
    let project = fake_project_with_lang();
    let vendor_dir = project.path().join("lang/vendor/filament-tables/en");
    fs::create_dir_all(&vendor_dir).unwrap();
    let table = vendor_dir.join("table.php");
    fs::write(
        &table,
        "<?php\nreturn [\n    'actions' => [\n        'filter' => [\n            'label' => 'Filter',\n        ],\n    ],\n];\n",
    )
    .unwrap();

    let got = resolve_translation(
        project.path(),
        "filament-tables::table.actions.filter.label",
        "en",
    );
    assert_eq!(got.as_deref(), Some("'Filter'"));
}

#[test]
fn resolves_namespaced_translation_with_source_path() {
    let project = fake_project_with_lang();
    let vendor_dir = project.path().join("lang/vendor/livewire/en");
    fs::create_dir_all(&vendor_dir).unwrap();
    fs::write(
        vendor_dir.join("validation.php"),
        "<?php\nreturn ['required' => 'This field is required.'];\n",
    )
    .unwrap();

    let resolved =
        resolve_translation_detailed(project.path(), "livewire::validation.required", "en", None)
            .expect("namespaced lookup should hit");

    assert_eq!(resolved.value, "'This field is required.'");
    assert!(
        resolved
            .source_file
            .ends_with("lang/vendor/livewire/en/validation.php"),
        "got: {:?}",
        resolved.source_file
    );
}

#[test]
fn returns_none_for_missing_namespaced_file() {
    let project = fake_project_with_lang();
    assert_eq!(
        resolve_translation(project.path(), "filament-tables::table.actions.label", "en"),
        None
    );
}

#[test]
fn falls_back_to_unpublished_vendor_dir_when_published_path_missing() {
    use std::collections::HashMap;
    use std::path::PathBuf;

    let project = fake_project_with_lang();
    // No published translations at lang/vendor/<ns>. Instead, simulate an
    // unpublished package at vendor/<ns>/lang/en/<file>.php.
    let vendor_lang = project.path().join("vendor/acme/billing/resources/lang");
    let en_dir = vendor_lang.join("en");
    fs::create_dir_all(&en_dir).unwrap();
    fs::write(
        en_dir.join("invoice.php"),
        "<?php\nreturn ['total' => 'Total'];\n",
    )
    .unwrap();

    let mut vendor_map: HashMap<String, PathBuf> = HashMap::new();
    vendor_map.insert("billing".to_string(), vendor_lang.clone());

    let resolved = resolve_translation_detailed(
        project.path(),
        "billing::invoice.total",
        "en",
        Some(&vendor_map),
    )
    .expect("unpublished vendor fallback should resolve");
    assert_eq!(resolved.value, "'Total'");
    assert!(
        resolved
            .source_file
            .ends_with("vendor/acme/billing/resources/lang/en/invoice.php"),
        "got: {:?}",
        resolved.source_file
    );
}

#[test]
fn published_path_still_wins_over_vendor_map_when_both_exist() {
    use std::collections::HashMap;
    use std::path::PathBuf;

    let project = fake_project_with_lang();
    // Published value
    let published = project.path().join("lang/vendor/billing/en");
    fs::create_dir_all(&published).unwrap();
    fs::write(
        published.join("invoice.php"),
        "<?php\nreturn ['total' => 'Published total'];\n",
    )
    .unwrap();
    // Unpublished value with the same key but different string
    let vendor_lang = project.path().join("vendor/acme/billing/lang");
    let en_dir = vendor_lang.join("en");
    fs::create_dir_all(&en_dir).unwrap();
    fs::write(
        en_dir.join("invoice.php"),
        "<?php\nreturn ['total' => 'Vendor total'];\n",
    )
    .unwrap();

    let mut vendor_map: HashMap<String, PathBuf> = HashMap::new();
    vendor_map.insert("billing".to_string(), vendor_lang);

    let resolved = resolve_translation_detailed(
        project.path(),
        "billing::invoice.total",
        "en",
        Some(&vendor_map),
    )
    .expect("should resolve");
    // Published overrides — the user's choice when they ran
    // `php artisan vendor:publish` should take precedence.
    assert_eq!(resolved.value, "'Published total'");
}

#[test]
fn dotted_key_without_path_returns_none() {
    let project = fake_project_with_lang();
    // A bare file name like "validation" — no key segment after the dot.
    assert_eq!(
        resolve_translation(project.path(), "validation", "en"),
        None
    );
}

#[test]
fn respects_locale_argument() {
    let project = fake_project_with_lang();
    fs::create_dir_all(project.path().join("lang/fr")).unwrap();
    let en = project.path().join("lang/en/validation.php");
    let fr = project.path().join("lang/fr/validation.php");
    fs::write(&en, "<?php\nreturn ['required' => 'English'];\n").unwrap();
    fs::write(&fr, "<?php\nreturn ['required' => 'Français'];\n").unwrap();

    assert_eq!(
        resolve_translation(project.path(), "validation.required", "en").as_deref(),
        Some("'English'")
    );
    assert_eq!(
        resolve_translation(project.path(), "validation.required", "fr").as_deref(),
        Some("'Français'")
    );
}
