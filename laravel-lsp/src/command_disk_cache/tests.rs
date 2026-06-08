//! Tests for the on-disk command index cache.
//!
//! Each test gets its own `TempDir` so they don't share state — the cache
//! path is derived from a hash of the project root, so distinct roots get
//! distinct cache files. Entries are validated against their declaring file's
//! mtime, so the tests write real files.

use super::*;
use crate::command_index::CommandPriority;
use std::path::Path;

/// Write a real PHP-ish file into `dir` and return its path. A real file is
/// required because the cache validates entries against their on-disk mtime —
/// without one, `read_mtime` returns `None` and `load_index` drops the entry.
fn touch(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, contents).unwrap();
    p
}

/// A minimal `CommandEntry` pointing at `file`.
fn entry(name: &str, class: &str, file: PathBuf, priority: CommandPriority) -> CommandEntry {
    CommandEntry {
        name: name.to_string(),
        class_name: class.to_string(),
        raw_signature: name.to_string(),
        file,
        line: 5,
        start_column: 24,
        end_column: 24 + name.len() as u32,
        priority,
    }
}

#[test]
fn save_then_load_restores_entries() {
    let project = tempfile::TempDir::new().unwrap();
    let file = touch(
        project.path(),
        "SendEmails.php",
        "<?php class SendEmails {}",
    );

    let mut index = CommandIndex::default();
    index.insert_entry(entry(
        "emails:send",
        "SendEmails",
        file.clone(),
        CommandPriority::App,
    ));

    let saved = save_index(&index, project.path()).unwrap();
    assert_eq!(saved, 1, "save should report one entry written");

    let restored = load_index(project.path()).expect("cache should load");
    assert_eq!(restored.len(), 1);
    let got = restored.resolve("emails:send").expect("command restored");
    assert_eq!(got.class_name, "SendEmails");
    assert_eq!(got.file, file);
    assert_eq!(got.priority, CommandPriority::App);
}

#[test]
fn load_returns_none_when_no_cache_exists() {
    let project = tempfile::TempDir::new().unwrap();
    assert!(
        load_index(project.path()).is_none(),
        "a project with no cache file should load nothing"
    );
}

#[test]
fn load_drops_entry_whose_file_was_deleted() {
    let project = tempfile::TempDir::new().unwrap();
    let file = touch(project.path(), "Gone.php", "<?php class Gone {}");

    let mut index = CommandIndex::default();
    index.insert_entry(entry("a:gone", "Gone", file.clone(), CommandPriority::App));
    save_index(&index, project.path()).unwrap();

    std::fs::remove_file(&file).unwrap();

    let restored = load_index(project.path()).expect("cache file still loads");
    assert!(
        restored.resolve("a:gone").is_none(),
        "an entry whose declaring file is gone must be dropped"
    );
    assert!(restored.is_empty());
}

#[test]
fn load_drops_entry_whose_file_changed() {
    let project = tempfile::TempDir::new().unwrap();
    let file = touch(project.path(), "Changed.php", "<?php class Changed {}");

    let mut index = CommandIndex::default();
    index.insert_entry(entry(
        "a:changed",
        "Changed",
        file.clone(),
        CommandPriority::App,
    ));
    save_index(&index, project.path()).unwrap();

    // Sleep just long enough that the OS records a different mtime, then
    // rewrite the file. 50ms is enough for APFS / ext4 / NTFS (matches the
    // sibling pattern_disk_cache test).
    std::thread::sleep(std::time::Duration::from_millis(50));
    std::fs::write(&file, "<?php class Changed { /* edited */ }").unwrap();

    let restored = load_index(project.path()).expect("cache file still loads");
    assert!(
        restored.resolve("a:changed").is_none(),
        "a changed declaring file must invalidate its cached entry"
    );
}

#[test]
fn load_reapplies_priority_merge() {
    // Two declarations of the same command name at different tiers must
    // collapse to the highest-priority one on restore.
    let project = tempfile::TempDir::new().unwrap();
    let app_file = touch(project.path(), "AppQueue.php", "<?php class AppQueue {}");
    let pkg_file = touch(project.path(), "PkgQueue.php", "<?php class PkgQueue {}");

    let mut index = CommandIndex::default();
    index.insert_entry(entry(
        "queue:work",
        "PkgQueue",
        pkg_file,
        CommandPriority::Package,
    ));
    index.insert_entry(entry(
        "queue:work",
        "AppQueue",
        app_file,
        CommandPriority::App,
    ));
    // Sanity: the in-memory merge already kept the App entry.
    assert_eq!(index.resolve("queue:work").unwrap().class_name, "AppQueue");

    save_index(&index, project.path()).unwrap();
    let restored = load_index(project.path()).expect("cache should load");
    assert_eq!(restored.len(), 1, "same name collapses to one entry");
    assert_eq!(
        restored.resolve("queue:work").unwrap().class_name,
        "AppQueue",
        "App declaration must win after a disk round-trip"
    );
}

#[test]
fn save_skips_entry_whose_file_vanished() {
    let project = tempfile::TempDir::new().unwrap();
    let present = touch(project.path(), "Here.php", "<?php class Here {}");

    let mut index = CommandIndex::default();
    index.insert_entry(entry("a:here", "Here", present, CommandPriority::App));
    index.insert_entry(entry(
        "a:missing",
        "Missing",
        project.path().join("Missing.php"), // never created
        CommandPriority::App,
    ));

    let saved = save_index(&index, project.path()).unwrap();
    assert_eq!(saved, 1, "the entry with no file on disk is skipped");

    let restored = load_index(project.path()).unwrap();
    assert!(restored.resolve("a:here").is_some());
    assert!(restored.resolve("a:missing").is_none());
}
