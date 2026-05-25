//! Tests for the on-disk pattern cache.
//!
//! Each test gets its own `TempDir` so they don't share state — the
//! cache path is derived from a hash of the project root, so distinct
//! roots get distinct cache files.

use super::*;
use crate::salsa_impl::ViewReferenceData;
use std::sync::Arc;
use tempfile::TempDir;

/// Build a minimal `ParsedPatternsData` with one view ref so we can
/// assert that loaded entries match what was saved.
fn fake_patterns(view_name: &str) -> ParsedPatternsData {
    let mut data = ParsedPatternsData::default();
    data.views.push(Arc::new(ViewReferenceData {
        name: view_name.to_string(),
        line: 1,
        column: 0,
        end_column: 10,
        is_route_view: false,
    }));
    data.build_position_index();
    data
}

/// Write a real PHP-ish file into `dir` and return its path. We need an
/// actual file because the cache validates entries against their on-disk
/// mtime — without a real file `read_mtime` returns None and load_into
/// would drop the entry as stale.
fn touch(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, contents).unwrap();
    p
}

#[test]
fn save_then_load_restores_entries() {
    let project = TempDir::new().unwrap();
    let cache = Arc::new(DashMap::new());

    let file = touch(project.path(), "home.blade.php", "<x-foo/>");
    cache.insert(file.clone(), (0, Arc::new(fake_patterns("home"))));

    let saved = save_from(&cache, project.path()).unwrap();
    assert_eq!(saved, 1, "save should report one entry written");

    // Fresh DashMap simulates a new LSP startup.
    let restored_cache = Arc::new(DashMap::new());
    let (restored, dropped) = load_into(&restored_cache, project.path());
    assert_eq!(restored, 1);
    assert_eq!(dropped, 0);

    let entry = restored_cache.get(&file).expect("entry should be restored");
    assert_eq!(entry.value().1.views[0].name, "home");
}

#[test]
fn entry_dropped_when_file_mtime_changes() {
    let project = TempDir::new().unwrap();
    let cache = Arc::new(DashMap::new());

    let file = touch(project.path(), "users.blade.php", "<x-bar/>");
    cache.insert(file.clone(), (0, Arc::new(fake_patterns("users"))));
    save_from(&cache, project.path()).unwrap();

    // Sleep just long enough that the OS records a different mtime,
    // then rewrite the file. Different FSes have different resolutions;
    // 50ms is enough for APFS / ext4 / NTFS.
    std::thread::sleep(std::time::Duration::from_millis(50));
    std::fs::write(&file, "<x-baz/>").unwrap();

    let restored_cache = Arc::new(DashMap::new());
    let (restored, dropped) = load_into(&restored_cache, project.path());
    assert_eq!(restored, 0, "stale entry should not be restored");
    assert_eq!(dropped, 1, "stale entry should be counted as dropped");
}

#[test]
fn entry_dropped_when_file_is_deleted() {
    let project = TempDir::new().unwrap();
    let cache = Arc::new(DashMap::new());

    let file = touch(project.path(), "gone.blade.php", "<x-foo/>");
    cache.insert(file.clone(), (0, Arc::new(fake_patterns("gone"))));
    save_from(&cache, project.path()).unwrap();

    std::fs::remove_file(&file).unwrap();

    let restored_cache = Arc::new(DashMap::new());
    let (restored, dropped) = load_into(&restored_cache, project.path());
    assert_eq!(restored, 0);
    assert_eq!(dropped, 1);
}

#[test]
fn unchanged_file_is_restored_after_save() {
    let project = TempDir::new().unwrap();
    let cache = Arc::new(DashMap::new());

    let file = touch(project.path(), "kept.blade.php", "<x-foo/>");
    cache.insert(file.clone(), (0, Arc::new(fake_patterns("kept"))));
    save_from(&cache, project.path()).unwrap();

    // No write between save and load — same mtime, so cache hits.
    let restored_cache = Arc::new(DashMap::new());
    let (restored, dropped) = load_into(&restored_cache, project.path());
    assert_eq!(restored, 1);
    assert_eq!(dropped, 0);
}

#[test]
fn missing_cache_file_loads_zero() {
    let project = TempDir::new().unwrap();
    let restored_cache = Arc::new(DashMap::new());
    let (restored, dropped) = load_into(&restored_cache, project.path());
    assert_eq!(restored, 0);
    assert_eq!(dropped, 0);
    assert!(restored_cache.is_empty());
}

#[test]
fn position_index_is_rebuilt_on_load() {
    let project = TempDir::new().unwrap();
    let cache = Arc::new(DashMap::new());

    let file = touch(project.path(), "indexed.blade.php", "<x-foo/>");
    cache.insert(file.clone(), (0, Arc::new(fake_patterns("indexed"))));
    save_from(&cache, project.path()).unwrap();

    let restored_cache = Arc::new(DashMap::new());
    load_into(&restored_cache, project.path());

    // find_at_position uses the position index. If it works after a
    // load, the index was rebuilt successfully — which is the whole
    // point of running `build_position_index()` in load_into.
    let entry = restored_cache.get(&file).unwrap();
    let patterns = &entry.value().1;
    let found = patterns.find_at_position(1, 5);
    assert!(
        found.is_some(),
        "position index should be reconstructed so find_at_position works"
    );
}

#[test]
fn corrupted_cache_file_loads_zero() {
    let project = TempDir::new().unwrap();

    // Write garbage to where the cache file would live.
    let cache_path = cache_file_path(project.path()).unwrap();
    std::fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
    std::fs::write(&cache_path, b"not a valid bincode payload at all").unwrap();

    let restored_cache = Arc::new(DashMap::new());
    let (restored, dropped) = load_into(&restored_cache, project.path());
    assert_eq!(restored, 0, "garbage cache should yield zero entries");
    assert_eq!(
        dropped, 0,
        "garbage isn't counted as dropped — it's not even decoded"
    );
    assert!(restored_cache.is_empty());
}
