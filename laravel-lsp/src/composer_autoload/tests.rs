use super::*;
use std::path::PathBuf;
use tempfile::TempDir;

/// Build a Laravel-shaped tempdir with the given (path, body) pairs.
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

#[test]
fn resolves_app_classes_via_project_composer_json() {
    // composer.json maps `App\` → `app/`, so App\Models\User must land
    // at app/Models/User.php — same answer as the FQCN heuristic, but
    // now we got there by reading the source of truth.
    let composer = r#"{
        "autoload": {
            "psr-4": {
                "App\\": "app/"
            }
        }
    }"#;
    let (_dir, root) = project_with_files(&[
        ("composer.json", composer),
        ("app/Models/User.php", "<?php class User {}"),
    ]);
    let autoload = ComposerAutoload::load(&root);
    let resolved = autoload
        .resolve("App\\Models\\User")
        .expect("App PSR-4 hit");
    assert!(
        resolved.ends_with("app/Models/User.php"),
        "got {resolved:?}"
    );
}

#[test]
fn resolves_hyphenated_vendor_packages_from_installed_json() {
    // This is the exact crossbible-vapor failure case: the package dir
    // is `bible-models` (with hyphen) but the namespace is
    // `CrossBibleInc\BibleModels\` (no hyphen). The lowercased-namespace
    // heuristic computes `biblemodels` and misses. The real PSR-4 map
    // tells us the truth.
    let installed = r#"{
        "packages": [
            {
                "name": "crossbibleinc/bible-models",
                "autoload": {
                    "psr-4": { "CrossBibleInc\\BibleModels\\": "src/" }
                },
                "install-path": "../crossbibleinc/bible-models"
            }
        ]
    }"#;
    let (_dir, root) = project_with_files(&[
        ("vendor/composer/installed.json", installed),
        (
            "vendor/crossbibleinc/bible-models/src/Models/Version.php",
            "<?php\nnamespace CrossBibleInc\\BibleModels\\Models;\nclass Version {}",
        ),
    ]);
    let autoload = ComposerAutoload::load(&root);
    let resolved = autoload
        .resolve("CrossBibleInc\\BibleModels\\Models\\Version")
        .expect("hyphenated vendor package should resolve");
    assert!(
        resolved.ends_with("vendor/crossbibleinc/bible-models/src/Models/Version.php"),
        "got {resolved:?}"
    );
}

#[test]
fn longest_prefix_wins_when_multiple_match() {
    // If both `App\` → app/ and `App\Models\` → custom/models/ are
    // declared, the more specific prefix must win for `App\Models\User`.
    let composer = r#"{
        "autoload": {
            "psr-4": {
                "App\\": "app/",
                "App\\Models\\": "custom/models/"
            }
        }
    }"#;
    let (_dir, root) = project_with_files(&[
        ("composer.json", composer),
        ("custom/models/User.php", "<?php class User {}"),
        ("app/Models/User.php", "<?php class User {}"),
    ]);
    let autoload = ComposerAutoload::load(&root);
    let resolved = autoload.resolve("App\\Models\\User").expect("resolve");
    assert!(
        resolved.ends_with("custom/models/User.php"),
        "longest prefix should win; got {resolved:?}"
    );
}

#[test]
fn psr4_value_can_be_array_of_paths() {
    // Some packages declare PSR-4 paths as an array — Composer tries
    // each in order. We try them all and return the first that exists.
    let composer = r#"{
        "autoload": {
            "psr-4": {
                "App\\": ["app/", "src/"]
            }
        }
    }"#;
    let (_dir, root) = project_with_files(&[
        ("composer.json", composer),
        ("src/Models/User.php", "<?php class User {}"),
        // Note: NO app/Models/User.php — must fall through to src/.
    ]);
    let autoload = ComposerAutoload::load(&root);
    let resolved = autoload.resolve("App\\Models\\User").expect("resolve");
    assert!(
        resolved.ends_with("src/Models/User.php"),
        "got {resolved:?}"
    );
}

#[test]
fn returns_none_for_fqcn_with_no_matching_prefix() {
    // Class lives in a namespace nobody declared autoload for —
    // resolution must return None so the caller can fall back to other
    // strategies (basename walk, etc.).
    let composer = r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#;
    let (_dir, root) = project_with_files(&[("composer.json", composer)]);
    let autoload = ComposerAutoload::load(&root);
    assert!(autoload.resolve("Unrelated\\Vendor\\Something").is_none());
}

#[test]
fn returns_none_when_psr4_matches_but_file_doesnt_exist() {
    // PSR-4 says "this is where it should be", but the file isn't there
    // (deleted, renamed, never created). We don't lie — return None.
    let composer = r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#;
    let (_dir, root) = project_with_files(&[("composer.json", composer)]);
    let autoload = ComposerAutoload::load(&root);
    assert!(autoload.resolve("App\\Models\\Missing").is_none());
}

#[test]
fn autoload_dev_is_included() {
    // Database\Factories\ lives in autoload-dev, not autoload. Tests
    // and seeders need to resolve from there.
    let composer = r#"{
        "autoload-dev": {
            "psr-4": { "Database\\Factories\\": "database/factories/" }
        }
    }"#;
    let (_dir, root) = project_with_files(&[
        ("composer.json", composer),
        (
            "database/factories/UserFactory.php",
            "<?php\nnamespace Database\\Factories;\nclass UserFactory {}",
        ),
    ]);
    let autoload = ComposerAutoload::load(&root);
    let resolved = autoload
        .resolve("Database\\Factories\\UserFactory")
        .expect("autoload-dev should be honored");
    assert!(
        resolved.ends_with("database/factories/UserFactory.php"),
        "got {resolved:?}"
    );
}

#[test]
fn leading_backslash_in_fqcn_is_tolerated() {
    // PHP allows `\App\Models\User` (fully qualified marker). We treat
    // it identically to the version without the leading slash.
    let composer = r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#;
    let (_dir, root) = project_with_files(&[
        ("composer.json", composer),
        ("app/Models/User.php", "<?php class User {}"),
    ]);
    let autoload = ComposerAutoload::load(&root);
    let resolved = autoload.resolve("\\App\\Models\\User").expect("resolve");
    assert!(
        resolved.ends_with("app/Models/User.php"),
        "got {resolved:?}"
    );
}

#[test]
fn resolve_namespace_dirs_maps_namespace_to_existing_directory() {
    // Blade::componentNamespace('App\\View\\Components\\Nightshade', 'nightshade')
    // must resolve to the on-disk directory so its class files can be walked
    // for completion candidates.
    let composer = r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#;
    let (_dir, root) = project_with_files(&[
        ("composer.json", composer),
        (
            "app/View/Components/Nightshade/Alert.php",
            "<?php class Alert {}",
        ),
    ]);
    let autoload = ComposerAutoload::load(&root);

    let dirs = autoload.resolve_namespace_dirs("App\\View\\Components\\Nightshade");

    assert_eq!(dirs.len(), 1, "expected one resolved dir, got {dirs:?}");
    assert!(
        dirs[0].ends_with("app/View/Components/Nightshade"),
        "got {:?}",
        dirs[0],
    );
}

#[test]
fn resolve_namespace_dirs_returns_empty_for_unknown_or_nonexistent() {
    let composer = r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#;
    let (_dir, root) = project_with_files(&[("composer.json", composer)]);
    let autoload = ComposerAutoload::load(&root);

    // No matching PSR-4 prefix.
    assert!(autoload
        .resolve_namespace_dirs("Vendor\\Pkg\\Components")
        .is_empty());
    // Matching prefix but the directory doesn't exist on disk.
    assert!(autoload
        .resolve_namespace_dirs("App\\View\\Components")
        .is_empty());
}
