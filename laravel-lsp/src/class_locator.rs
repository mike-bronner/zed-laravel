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
    if class_name.is_empty() {
        return None;
    }
    let simple_name = class_name.rsplit('\\').next().unwrap_or(class_name);
    let target_filename = format!("{}.php", simple_name);

    for app_root in search_roots(root) {
        if !app_root.is_dir() {
            continue;
        }
        let walker = WalkDir::new(&app_root).into_iter().filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !matches!(name.as_ref(), "vendor" | "node_modules" | ".git")
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
