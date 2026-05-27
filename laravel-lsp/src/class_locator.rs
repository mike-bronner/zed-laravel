//! Find a PHP class's source file anywhere under the project's `app/` tree.
//!
//! Used by the LSP to power hover, property completion, and goto-definition
//! for variables whose type resolves to a class name (e.g. `$form` →
//! `ContactForm` → `app/Livewire/Forms/ContactForm.php`).
//!
//! The strategy is intentionally simple: walk `app/**/*.php` and match by
//! basename. This avoids parsing `composer.json` PSR-4 mappings and works for
//! any standard Laravel layout (`app/Models/`, `app/Livewire/`, `app/Http/`,
//! `app/Livewire/Forms/`, `app/Services/`, etc.). The walker skips `vendor/`
//! and `node_modules/` (we never want to land in dependency code).
//!
//! Filesystem traversal is bounded by the `app/` directory depth, which is
//! typically modest (~tens of subdirs even in large apps). For projects with
//! atypical layouts (e.g. `src/` instead of `app/`), the caller can extend
//! the search roots.

use std::path::{Path, PathBuf};

use walkdir::WalkDir;

/// Locate the PHP source file for a given class name.
///
/// Searches the project's `app/` directory recursively for `<ClassName>.php`,
/// preferring files whose path segments match the class's namespace shape when
/// possible.
///
/// Returns the first matching file path, or `None` when the class can't be
/// found. Does not parse the file to verify the class name inside — relies on
/// Laravel's strong convention that file basename matches class name.
pub fn find_php_class_file(class_name: &str, root: &Path) -> Option<PathBuf> {
    // Try the namespace-aware path mapping first — `App\Models\User`
    // should land at `app/Models/User.php`, not at the first `User.php`
    // we happen to walk into (which could be `app/Nova/User.php` or
    // similar). Falls back to a basename walk under app/ if the FQCN
    // doesn't map to an existing file.
    if let Some(path) = find_php_class_file_by_fqcn(class_name, root, false) {
        return Some(path);
    }
    find_php_class_file_impl(class_name, root, false)
}

/// Same as [`find_php_class_file`] but ALSO searches `vendor/` so the
/// inheritance walker can pick up parent classes shipped by Laravel
/// packages (e.g. `OAuthAccessToken extends Laravel\Passport\Token`
/// — Token lives in `vendor/laravel/passport/src/Token.php`).
///
/// Slower than the app-only variant because vendor trees are huge. Use
/// it only for inheritance walking, where the search depth is bounded
/// (≤10 levels) and the result is cached behind ModelMetadata anyway.
/// app/-side definitions still win — they're checked first.
pub fn find_php_class_file_in_app_or_vendor(class_name: &str, root: &Path) -> Option<PathBuf> {
    // Try FQCN-aware lookup across app/ AND vendor/ first. If neither
    // PSR-4-shaped path exists, fall through to a basename walk —
    // again preferring app/ over vendor/ since project classes shadow
    // vendor classes with matching basenames.
    if let Some(path) = find_php_class_file_by_fqcn(class_name, root, false) {
        return Some(path);
    }
    if let Some(path) = find_php_class_file_by_fqcn(class_name, root, true) {
        return Some(path);
    }
    if let Some(path) = find_php_class_file_impl(class_name, root, false) {
        return Some(path);
    }
    find_php_class_file_impl(class_name, root, true)
}

/// Map a fully-qualified class name to its source file path.
///
/// Resolution order:
///
/// 1. **Real Composer PSR-4 data** ([`crate::composer_autoload::ComposerAutoload`])
///    — reads the project's `composer.json` + `vendor/composer/installed.json`
///    and uses the actual declared PSR-4 prefix → directory mappings.
///    This handles packages whose directory name is hyphenated even
///    though the namespace isn't (e.g. `CrossBibleInc\BibleModels\` →
///    `vendor/crossbibleinc/bible-models/src/`).
/// 2. **Conventional heuristic fallback** — for projects without an
///    `installed.json` (rare), the original mapping kicks in:
///    - `App\Models\User` → `app/Models/User.php` (or `src/Models/User.php`)
///    - `Laravel\Passport\Token` → `vendor/laravel/passport/src/Token.php`
///      (lowercased vendor + package, then `src/`)
///
/// `search_vendor`: when `true`, only consider the vendor/ mapping;
/// when `false`, only consider the App/ mapping. Callers chain both
/// modes to cover the realistic search space. The Composer step
/// honors this split too — it segregates project-rooted and
/// vendor-rooted entries.
///
/// Returns `None` if neither strategy finds a file on disk. The caller
/// then falls back to a basename walk so we still resolve
/// unconventional PSR-4 layouts.
fn find_php_class_file_by_fqcn(
    fqcn: &str,
    project_root: &Path,
    search_vendor: bool,
) -> Option<PathBuf> {
    // Try the real Composer PSR-4 map first. Filter results by
    // whether the resolved path is under vendor/ so the caller's
    // app-vs-vendor segregation still holds.
    let autoload = crate::composer_autoload::ComposerAutoload::for_project(project_root);
    if let Some(path) = autoload.resolve(fqcn) {
        let is_vendor = path.starts_with(project_root.join("vendor"));
        if is_vendor == search_vendor {
            return Some(path);
        }
        // Path exists but on the wrong side of the app/vendor split
        // for this call. Fall through to the heuristic — the OTHER
        // call (with the flipped flag) will pick up the Composer hit.
    }
    let segments: Vec<&str> = fqcn.split('\\').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return None;
    }
    let class_name = *segments.last().unwrap();
    let ns_segments = &segments[..segments.len() - 1];

    if !search_vendor {
        // App\Models\User → app/Models/User.php (and src/ alternative for
        // projects that use src/ for app code).
        if ns_segments.first().map(|s| s.to_ascii_lowercase()) == Some("app".to_string()) {
            let rest = &ns_segments[1..];
            for app_dir in ["app", "src"] {
                let mut path = project_root.join(app_dir);
                for seg in rest {
                    path = path.join(seg);
                }
                path = path.join(format!("{class_name}.php"));
                if path.exists() {
                    return Some(path);
                }
            }
        }
        return None;
    }

    // Vendor convention: lowercase first two segments → package
    // directory; remaining segments are paths under `src/` (or under
    // the package root if `src/` doesn't exist for this package).
    if ns_segments.len() < 2 {
        return None;
    }
    let vendor = ns_segments[0].to_ascii_lowercase();
    let pkg = ns_segments[1].to_ascii_lowercase();
    let rest = &ns_segments[2..];

    for src_segment in ["src", ""] {
        let mut path = project_root.join("vendor").join(&vendor).join(&pkg);
        if !src_segment.is_empty() {
            path = path.join(src_segment);
        }
        for seg in rest {
            path = path.join(seg);
        }
        path = path.join(format!("{class_name}.php"));
        if path.exists() {
            return Some(path);
        }
    }
    None
}

fn find_php_class_file_impl(class_name: &str, root: &Path, search_vendor: bool) -> Option<PathBuf> {
    if class_name.is_empty() {
        return None;
    }
    let simple_name = class_name.rsplit('\\').next().unwrap_or(class_name);
    let target_filename = format!("{}.php", simple_name);

    let roots: Vec<PathBuf> = if search_vendor {
        vec![root.join("vendor")]
    } else {
        search_roots(root)
    };

    for app_root in roots {
        if !app_root.is_dir() {
            continue;
        }
        let walker = WalkDir::new(&app_root).into_iter().filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            // When searching vendor itself, allow descent INTO vendor —
            // only skip nested vendor/.git/.node_modules dirs.
            if search_vendor {
                !matches!(name.as_ref(), "node_modules" | ".git")
            } else {
                !matches!(name.as_ref(), "vendor" | "node_modules" | ".git")
            }
        });
        for entry in walker.filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }
            if entry.file_name() == target_filename.as_str() {
                return Some(entry.into_path());
            }
        }
    }

    None
}

/// Directories worth searching for class files. Standard Laravel uses `app/`;
/// some projects also use `src/` for libraries living alongside the app.
fn search_roots(root: &Path) -> Vec<PathBuf> {
    vec![root.join("app"), root.join("src")]
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Spin up a Laravel-shaped tempdir with the given (path, body)
    /// pairs. Paths are relative to the project root.
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
    fn fqcn_aware_lookup_prefers_namespace_shape_match() {
        // Mike's crossbible-vapor case: TWO files named Version.php live
        // in the project. The FQCN `App\Models\Version` should map to
        // `app/Models/Version.php`, NOT to `app/Nova/Filters/Version.php`
        // even though the latter is also a Version.php with the same
        // basename.
        let (_dir, root) = project_with_files(&[
            (
                "app/Models/Version.php",
                "<?php\nnamespace App\\Models;\nclass Version {}",
            ),
            (
                "app/Nova/Filters/Version.php",
                "<?php\nnamespace App\\Nova\\Filters;\nclass Version {}",
            ),
        ]);
        let path =
            find_php_class_file("App\\Models\\Version", &root).expect("should find the model");
        assert!(
            path.ends_with("app/Models/Version.php"),
            "should pick the namespace-matching file; got: {path:?}"
        );
    }

    #[test]
    fn fqcn_aware_lookup_falls_back_to_basename_when_no_shape_match() {
        // If only one Version.php exists in the project (no PSR-4 match
        // possible), the basename walk still finds it.
        let (_dir, root) =
            project_with_files(&[("app/SomeOtherPlace/Version.php", "<?php\nclass Version {}")]);
        let path = find_php_class_file("App\\Models\\Version", &root).expect("fallback walk");
        assert!(path.ends_with("app/SomeOtherPlace/Version.php"));
    }

    #[test]
    fn fqcn_lookup_routes_vendor_classes_to_psr4_path() {
        // `Laravel\Passport\Token` should resolve to the standard
        // Composer PSR-4 path. Note: only `find_php_class_file_in_app_or_vendor`
        // searches vendor — `find_php_class_file` stays app-side only.
        let (_dir, root) = project_with_files(&[(
            "vendor/laravel/passport/src/Token.php",
            "<?php\nnamespace Laravel\\Passport;\nclass Token {}",
        )]);
        let path = find_php_class_file_in_app_or_vendor("Laravel\\Passport\\Token", &root)
            .expect("vendor PSR-4 lookup");
        assert!(path.ends_with("vendor/laravel/passport/src/Token.php"));
    }

    #[test]
    fn fqcn_lookup_app_class_shadows_vendor_match() {
        // Both an app/-side and a vendor/-side file with the same FQCN
        // exist? App wins (matches PSR-4 autoload behavior).
        let (_dir, root) = project_with_files(&[
            (
                "app/Models/Token.php",
                "<?php\nnamespace App\\Models;\nclass Token {}",
            ),
            (
                "vendor/laravel/passport/src/Token.php",
                "<?php\nnamespace Laravel\\Passport;\nclass Token {}",
            ),
        ]);
        let path = find_php_class_file_in_app_or_vendor("App\\Models\\Token", &root).unwrap();
        assert!(
            path.ends_with("app/Models/Token.php"),
            "App\\Models\\Token should resolve to the project file; got {path:?}"
        );
    }

    #[test]
    fn bare_class_name_with_no_namespace_still_uses_basename_walk() {
        // `Foo` (no namespace) doesn't have a PSR-4 shape — should fall
        // through to basename walking.
        let (_dir, root) = project_with_files(&[("app/Services/Foo.php", "<?php\nclass Foo {}")]);
        let path = find_php_class_file("Foo", &root).expect("bare-name walk");
        assert!(path.ends_with("app/Services/Foo.php"));
    }
}
