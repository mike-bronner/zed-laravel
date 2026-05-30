use super::*;
use tempfile::TempDir;

#[test]
fn test_cache_round_trip() {
    let temp = TempDir::new().unwrap();
    let project_root = temp.path();

    // Create a cache manager and add some data
    let mut manager = CacheManager::load(project_root);

    let mut vendor_scan = ScanResult::new();
    vendor_scan.middleware.insert(
        "auth".to_string(),
        MiddlewareEntry {
            class: "Illuminate\\Auth\\Middleware\\Authenticate".to_string(),
            class_file: Some(
                "vendor/laravel/framework/src/Illuminate/Auth/Middleware/Authenticate.php"
                    .to_string(),
            ),
            source_file: Some("bootstrap/app.php".to_string()),
            line: 10,
        },
    );
    manager.set_vendor_scan(vendor_scan);

    // Save
    manager.save().unwrap();

    // Load fresh
    let loaded = CacheManager::load(project_root);
    assert!(loaded.has_cached_data());

    let middleware = loaded.get_all_middleware();
    assert!(middleware.contains_key("auth"));
}

#[test]
fn test_mtime_comparison() {
    let mtime1 = FileMtime {
        mtime_secs: 1000,
        mtime_nanos: 500,
    };
    let mtime2 = FileMtime {
        mtime_secs: 1000,
        mtime_nanos: 500,
    };
    let mtime3 = FileMtime {
        mtime_secs: 1001,
        mtime_nanos: 0,
    };

    assert_eq!(mtime1, mtime2);
    assert_ne!(mtime1, mtime3);
}

#[test]
fn test_xdg_cache_path() {
    use std::path::Path;

    let project_root = Path::new("/Users/mike/Developer/some-project");

    // Verify we can get a cache path
    let cache_file = get_cache_file(project_root);
    assert!(
        cache_file.is_some(),
        "Should be able to determine cache path"
    );

    let cache_path = cache_file.unwrap();
    println!("Cache path for {:?}: {:?}", project_root, cache_path);

    // Verify the path structure on macOS
    #[cfg(target_os = "macos")]
    {
        let path_str = cache_path.to_string_lossy();
        assert!(
            path_str.contains("Library/Caches/org.mike-bronner.laravel-lsp"),
            "macOS cache should be in ~/Library/Caches/org.mike-bronner.laravel-lsp, got: {}",
            path_str
        );
        assert!(
            path_str.ends_with("cache.json"),
            "Cache file should be cache.json, got: {}",
            path_str
        );
    }

    // Verify the cache directory can be determined
    let cache_dir = get_cache_dir(project_root);
    assert!(cache_dir.is_some());
    println!("Cache dir for {:?}: {:?}", project_root, cache_dir.unwrap());
}
