use crate::LaravelLanguageServer;
use laravel_lsp::salsa_impl::AssetHelperType;
use std::path::Path;

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
