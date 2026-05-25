//! Tests for the file-watcher glob construction. The notification
//! handling itself is wired in `main.rs` against a real `Backend` and
//! exercised via integration paths — these tests just verify we ask
//! for the right things from the client.

use super::*;
use std::path::PathBuf;

#[test]
fn watchers_cover_all_four_indexed_directories() {
    let root = PathBuf::from("/projects/laravel-app");
    let view_paths = vec![root.join("resources/views")];
    let livewire = root.join("app/Livewire");
    let watchers = build_watchers(&root, &view_paths, Some(&livewire));

    let globs: Vec<String> = watchers
        .iter()
        .map(|w| match &w.glob_pattern {
            GlobPattern::String(s) => s.clone(),
            GlobPattern::Relative(_) => unreachable!("we always emit absolute"),
        })
        .collect();

    // The four indexed-by-Salsa categories must each have a watcher.
    assert!(
        globs
            .iter()
            .any(|g| g.contains("app/Http/Controllers") && g.ends_with("*.php")),
        "missing controllers glob: {:?}",
        globs
    );
    assert!(
        globs
            .iter()
            .any(|g| g.contains("routes/") && g.ends_with("*.php")),
        "missing routes glob: {:?}",
        globs
    );
    assert!(
        globs
            .iter()
            .any(|g| g.contains("resources/views") && g.ends_with("*.blade.php")),
        "missing blade-views glob: {:?}",
        globs
    );
    assert!(
        globs
            .iter()
            .any(|g| g.contains("app/Livewire") && g.ends_with("*.php")),
        "missing livewire glob: {:?}",
        globs
    );
}

#[test]
fn watchers_omit_livewire_when_not_configured() {
    let root = PathBuf::from("/projects/no-livewire-app");
    let view_paths = vec![root.join("resources/views")];
    let watchers = build_watchers(&root, &view_paths, None);

    let has_livewire = watchers.iter().any(|w| match &w.glob_pattern {
        GlobPattern::String(s) => s.contains("Livewire"),
        _ => false,
    });
    assert!(
        !has_livewire,
        "should not register livewire glob when path is None"
    );
}

#[test]
fn watchers_register_each_configured_view_path() {
    // Themed apps configure multiple view paths — each gets its own
    // pair of watchers (blade + bare php).
    let root = PathBuf::from("/projects/themed-app");
    let view_paths = vec![
        root.join("resources/views"),
        root.join("themes/dark/views"),
        root.join("themes/light/views"),
    ];
    let watchers = build_watchers(&root, &view_paths, None);

    for view_path in &view_paths {
        let has_blade = watchers.iter().any(|w| match &w.glob_pattern {
            GlobPattern::String(s) => {
                s.contains(view_path.to_string_lossy().as_ref()) && s.ends_with("*.blade.php")
            }
            _ => false,
        });
        assert!(has_blade, "missing blade watcher for {:?}", view_path);
    }
}

#[test]
fn watchers_request_create_change_and_delete_events() {
    let root = PathBuf::from("/projects/test");
    let watchers = build_watchers(&root, &[root.join("resources/views")], None);

    let all_three = WatchKind::Create | WatchKind::Change | WatchKind::Delete;
    for w in &watchers {
        assert_eq!(
            w.kind,
            Some(all_three),
            "every watcher should request all three event kinds"
        );
    }
}

#[test]
fn registration_has_correct_method_and_id() {
    let root = PathBuf::from("/projects/test");
    let reg = build_registration(&root, &[root.join("resources/views")], None);
    assert_eq!(reg.method, METHOD);
    assert_eq!(reg.id, REGISTRATION_ID);
    assert!(reg.register_options.is_some(), "options must be serialized");
}

#[test]
fn registration_options_round_trip_through_serde() {
    let root = PathBuf::from("/projects/test");
    let view_paths = vec![root.join("resources/views")];
    let livewire = root.join("app/Livewire");
    let reg = build_registration(&root, &view_paths, Some(&livewire));

    // The client deserializes our register_options into
    // DidChangeWatchedFilesRegistrationOptions. Verify the value we
    // emit is shaped correctly.
    let json = reg.register_options.unwrap();
    let parsed: DidChangeWatchedFilesRegistrationOptions = serde_json::from_value(json).unwrap();
    assert!(
        !parsed.watchers.is_empty(),
        "must have at least one watcher"
    );

    // We constructed exactly 1 (controllers) + 1 (routes) + 2 (view
    // blade + php) + 1 (livewire) + 2 (vendor php + blade) = 7
    // watchers. If the construction changes, this assertion will
    // flag it for review.
    assert_eq!(parsed.watchers.len(), 7);
}

#[test]
fn watchers_include_vendor_php_and_blade_globs() {
    let root = PathBuf::from("/projects/laravel-app");
    let watchers = build_watchers(&root, &[root.join("resources/views")], None);

    let globs: Vec<String> = watchers
        .iter()
        .map(|w| match &w.glob_pattern {
            GlobPattern::String(s) => s.clone(),
            GlobPattern::Relative(_) => unreachable!(),
        })
        .collect();

    assert!(
        globs
            .iter()
            .any(|g| g.contains("/vendor/") && g.ends_with("*.php")),
        "missing vendor php glob: {:?}",
        globs
    );
    assert!(
        globs
            .iter()
            .any(|g| g.contains("/vendor/") && g.ends_with("*.blade.php")),
        "missing vendor blade glob: {:?}",
        globs
    );
}
