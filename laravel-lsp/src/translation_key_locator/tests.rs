use super::*;
use std::fs;
use tempfile::TempDir;

/// Build a fake Laravel project with a `lang/` directory and a list of
/// (locale, file_stem, content) entries seeded as locale lang files.
fn fake_project_with_lang(entries: &[(&str, &str, &str)]) -> TempDir {
    let dir = TempDir::new().unwrap();
    let lang = dir.path().join("lang");
    fs::create_dir_all(&lang).unwrap();
    for (locale, file_stem, content) in entries {
        let locale_dir = lang.join(locale);
        fs::create_dir_all(&locale_dir).unwrap();
        fs::write(locale_dir.join(format!("{file_stem}.php")), content).unwrap();
    }
    dir
}

const AUTH_EN: &str = r#"<?php
return [
    'failed' => 'These credentials do not match our records.',
    'password' => 'The provided password is incorrect.',
];
"#;

const AUTH_ES: &str = r#"<?php
return [
    'failed' => 'Estas credenciales no coinciden.',
    'password' => 'La contraseña proporcionada es incorrecta.',
];
"#;

const AUTH_NESTED_EN: &str = r#"<?php
return [
    'throttle' => [
        'message' => 'Too many attempts. Try again in :seconds seconds.',
    ],
];
"#;

#[test]
fn locates_key_across_multiple_locales() {
    let project = fake_project_with_lang(&[("en", "auth", AUTH_EN), ("es", "auth", AUTH_ES)]);
    let mut locs = locate_keys_across_locales(project.path(), "auth.failed");
    // Sort by file path so the assertion is order-independent.
    locs.sort_by(|a, b| a.file_path.cmp(&b.file_path));

    assert_eq!(locs.len(), 2, "key exists in both locales");
    assert!(locs[0].file_path.ends_with("lang/en/auth.php"));
    assert!(locs[1].file_path.ends_with("lang/es/auth.php"));
    for loc in &locs {
        let content = fs::read_to_string(&loc.file_path).unwrap();
        let line = content.lines().nth(loc.position.line as usize).unwrap();
        let slice = &line[loc.position.start_column as usize..loc.position.end_column as usize];
        assert_eq!(slice, "failed");
    }
}

#[test]
fn skips_locale_without_the_key() {
    // en has 'auth.failed', es has 'auth.password' but no 'auth.failed' would
    // be impossible to express in this format — instead model a locale that
    // simply doesn't define the auth file at all.
    let project = fake_project_with_lang(&[("en", "auth", AUTH_EN)]);
    fs::create_dir_all(project.path().join("lang/es")).unwrap();

    let locs = locate_keys_across_locales(project.path(), "auth.failed");
    assert_eq!(locs.len(), 1, "only en defines auth.php");
    assert!(locs[0].file_path.ends_with("lang/en/auth.php"));
}

#[test]
fn handles_nested_keys() {
    let project = fake_project_with_lang(&[("en", "auth", AUTH_NESTED_EN)]);
    let locs = locate_keys_across_locales(project.path(), "auth.throttle.message");
    assert_eq!(locs.len(), 1);
    let loc = &locs[0];
    let content = fs::read_to_string(&loc.file_path).unwrap();
    let line = content.lines().nth(loc.position.line as usize).unwrap();
    let slice = &line[loc.position.start_column as usize..loc.position.end_column as usize];
    assert_eq!(slice, "message");
}

#[test]
fn returns_empty_when_no_lang_dir() {
    let dir = TempDir::new().unwrap();
    assert!(locate_keys_across_locales(dir.path(), "auth.failed").is_empty());
}

#[test]
fn returns_empty_for_missing_key_in_all_locales() {
    let project = fake_project_with_lang(&[("en", "auth", AUTH_EN), ("es", "auth", AUTH_ES)]);
    assert!(locate_keys_across_locales(project.path(), "auth.missing").is_empty());
}

#[test]
fn returns_empty_when_dotted_key_has_no_segments() {
    let project = fake_project_with_lang(&[("en", "auth", AUTH_EN)]);
    // A bare "auth" without anything after the dot can't reach a leaf.
    assert!(locate_keys_across_locales(project.path(), "auth").is_empty());
}
