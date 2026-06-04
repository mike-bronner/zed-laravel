use crate::LaravelLanguageServer;

/// Prefix extracted by the `@vite()` completion context at the end of `line`.
fn prefix(line: &str) -> Option<String> {
    LaravelLanguageServer::get_vite_call_context(line, line.len() as u32).map(|c| c.prefix)
}

#[test]
fn string_form_detected() {
    assert_eq!(prefix("@vite('reso").as_deref(), Some("reso"));
    assert_eq!(prefix("@vite(\"reso").as_deref(), Some("reso"));
}

#[test]
fn array_first_element_detected() {
    assert_eq!(prefix("@vite(['reso").as_deref(), Some("reso"));
    assert_eq!(prefix("@vite([\"reso").as_deref(), Some("reso"));
}

#[test]
fn array_subsequent_element_detected() {
    // The gap (issue): cursor inside the 2nd array element. The opening-pattern
    // detector stops at the first element's closing quote and returns None.
    assert_eq!(
        prefix("@vite(['resources/css/app.css', 'reso").as_deref(),
        Some("reso"),
    );
    assert_eq!(
        prefix("@vite([\"resources/css/app.css\", \"reso").as_deref(),
        Some("reso"),
    );
}

#[test]
fn array_third_element_detected() {
    assert_eq!(
        prefix("@vite(['a.css', 'b.js', 'reso").as_deref(),
        Some("reso"),
    );
}

#[test]
fn subsequent_element_reports_correct_start_col() {
    // `@vite(['a.css', 'reso` — the 2nd string's content starts at index 17.
    let line = "@vite(['a.css', 'reso";
    let c = LaravelLanguageServer::get_vite_call_context(line, line.len() as u32).unwrap();
    assert_eq!(c.prefix, "reso");
    assert_eq!(c.start_col, 17);
}

#[test]
fn not_inside_a_string_returns_none() {
    // Between elements (after the comma, before the next quote).
    assert!(prefix("@vite(['a.css', ").is_none());
    // After the directive is closed.
    assert!(prefix("@vite(['a.css'])").is_none());
}

#[test]
fn vite_entry_files_lists_assets_and_respects_gitignore() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    let write = |rel: &str| {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "").unwrap();
    };
    std::fs::write(root.join(".gitignore"), "/vendor\n/node_modules\n").unwrap();
    write("resources/css/app.css");
    write("resources/js/app.js");
    write("resources/views/home.blade.php"); // not a Vite asset extension
    write("vendor/pkg/style.css"); // git-ignored
    write("node_modules/lib/index.js"); // git-ignored

    let files = LaravelLanguageServer::vite_entry_files(root);

    assert!(
        files.contains(&"resources/css/app.css".to_string()),
        "asset under resources/ should be listed: {files:?}",
    );
    assert!(files.contains(&"resources/js/app.js".to_string()));
    assert!(
        !files.iter().any(|f| f.contains("vendor/")),
        "vendor/ is git-ignored and must be pruned: {files:?}",
    );
    assert!(
        !files.iter().any(|f| f.contains("node_modules/")),
        "node_modules/ is git-ignored and must be pruned: {files:?}",
    );
    assert!(
        !files.iter().any(|f| f.ends_with(".blade.php")),
        "non-asset extensions must be excluded: {files:?}",
    );
}
