use crate::LaravelLanguageServer;
use laravel_lsp::salsa_impl::{parse_vite_directive_assets, AssetHelperType, AssetReferenceData};
use std::path::Path;
use tower_lsp::lsp_types::DiagnosticSeverity;

/// Minimal asset reference at line 0 for diagnostic tests.
fn asset_ref(helper: AssetHelperType, path: &str) -> AssetReferenceData {
    AssetReferenceData {
        path: path.to_string(),
        helper_type: helper,
        line: 0,
        column: 0,
        end_column: 5,
    }
}

#[test]
fn vite_entry_is_resolved_relative_to_project_root() {
    // Laravel's Vite directive treats the entry path as PROJECT-ROOT-relative,
    // verbatim (Vite::hotAsset → hotUrl + entry; Vite::chunk → manifest[entry]).
    // `@vite('resources/css/app.scss')` must resolve to
    // `<root>/resources/css/app.scss` — NOT `<root>/resources/resources/...`
    // (issue #46: the `resources/` segment was doubled).
    let root = Path::new("/project");
    let (path, name) = LaravelLanguageServer::asset_expected_path(
        AssetHelperType::ViteAsset,
        root,
        "resources/css/app.scss",
    );
    assert_eq!(path, Path::new("/project/resources/css/app.scss"));
    assert_eq!(name, "@vite");
}

#[test]
fn asset_helper_resolves_against_public() {
    // Regression guard: asset() stays public/-relative.
    let root = Path::new("/project");
    let (path, _) =
        LaravelLanguageServer::asset_expected_path(AssetHelperType::Asset, root, "css/app.css");
    assert_eq!(path, Path::new("/project/public/css/app.css"));
}

#[test]
fn resource_path_resolves_against_resources() {
    // Regression guard / contrast: resource_path('views') IS resources-relative,
    // unlike @vite.
    let root = Path::new("/project");
    let (path, _) =
        LaravelLanguageServer::asset_expected_path(AssetHelperType::ResourcePath, root, "views");
    assert_eq!(path, Path::new("/project/resources/views"));
}

// ── Severity follows runtime behavior: @vite throws (ERROR), others degrade (WARNING) ──

#[test]
fn vite_missing_entry_is_an_error() {
    // @vite() resolves its entry via Vite::chunk(), which throws ViteException
    // on a manifest miss — a production 500. So a missing entry is an ERROR.
    let dir = tempfile::TempDir::new().unwrap();
    let r = asset_ref(AssetHelperType::ViteAsset, "resources/css/missing.css");
    let d = LaravelLanguageServer::asset_diagnostic(&r, dir.path())
        .expect("missing @vite entry must be flagged");
    assert_eq!(d.severity, Some(DiagnosticSeverity::ERROR));
}

#[test]
fn vite_directory_entry_is_an_error() {
    // A directory is not a manifest file — Vite::chunk still throws. Requiring
    // is_file() (not just exists) catches `@vite('resources')`.
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join("resources")).unwrap();
    let r = asset_ref(AssetHelperType::ViteAsset, "resources");
    let d = LaravelLanguageServer::asset_diagnostic(&r, dir.path())
        .expect("a directory is not a valid @vite entry");
    assert_eq!(d.severity, Some(DiagnosticSeverity::ERROR));
}

#[test]
fn vite_empty_entry_is_an_error() {
    // `@vite('')` can't resolve and throws at build time — an ERROR with a
    // message that names it rather than the confusing "file not found: ''".
    let dir = tempfile::TempDir::new().unwrap();
    let r = asset_ref(AssetHelperType::ViteAsset, "");
    let d = LaravelLanguageServer::asset_diagnostic(&r, dir.path())
        .expect("empty @vite entry must be flagged");
    assert_eq!(d.severity, Some(DiagnosticSeverity::ERROR));
    assert!(
        d.message.contains("Empty"),
        "message should name the empty entry: {}",
        d.message
    );
}

#[test]
fn vite_real_file_is_clean() {
    let dir = tempfile::TempDir::new().unwrap();
    let p = dir.path().join("resources/css/app.css");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, "").unwrap();
    let r = asset_ref(AssetHelperType::ViteAsset, "resources/css/app.css");
    assert!(LaravelLanguageServer::asset_diagnostic(&r, dir.path()).is_none());
}

#[test]
fn asset_helper_miss_is_a_warning_not_error() {
    // asset('x') just builds a URL; a miss 404s in the browser but the page
    // still renders, so it's a WARNING — not app-breaking like @vite.
    let dir = tempfile::TempDir::new().unwrap();
    let r = asset_ref(AssetHelperType::Asset, "css/missing.css");
    let d = LaravelLanguageServer::asset_diagnostic(&r, dir.path())
        .expect("missing asset should be flagged");
    assert_eq!(d.severity, Some(DiagnosticSeverity::WARNING));
}

#[test]
fn asset_helper_existing_file_is_clean() {
    let dir = tempfile::TempDir::new().unwrap();
    let p = dir.path().join("public/css/app.css");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, "").unwrap();
    let r = asset_ref(AssetHelperType::Asset, "css/app.css");
    assert!(LaravelLanguageServer::asset_diagnostic(&r, dir.path()).is_none());
}

#[test]
fn empty_vite_entry_is_parsed_with_a_visible_range() {
    // The parser keeps empty entries (it used to silently drop them) so the
    // diagnostic can flag them, and spans the two quote chars so the squiggle
    // renders instead of collapsing to a zero-width range.
    let entries = parse_vite_directive_assets("(['app.css', ''])", 0, 0, 5);
    assert_eq!(entries.len(), 2, "both entries kept: {entries:?}");
    let (path, _row, col, end_col) = &entries[1];
    assert_eq!(path, "");
    assert_eq!(*end_col - *col, 2, "empty entry should span the two quotes");
}
