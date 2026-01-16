//! Laravel project configuration utilities
//!
//! This module provides utilities for discovering Laravel projects
//! and working with Laravel naming conventions.

use std::path::{Path, PathBuf};
use tracing::info;

/// Find the Laravel project root by walking up from a file path
///
/// Looks for Laravel-specific markers:
/// - composer.json + artisan (Laravel app)
/// - composer.json + app/ + resources/ (Laravel app)
/// - composer.json + src/ + vendor/ (Laravel package)
///
/// Returns None if no Laravel project root is found.
pub fn find_project_root(file_path: &Path) -> Option<PathBuf> {
    let mut current = file_path;

    // If it's a file, start from its parent directory
    if current.is_file() {
        current = current.parent()?;
    }

    // Walk up the directory tree
    loop {
        // Check for Laravel markers
        let has_composer = current.join("composer.json").exists();
        let has_artisan = current.join("artisan").exists();
        let has_app = current.join("app").is_dir();
        let has_resources = current.join("resources").is_dir();
        let has_src = current.join("src").is_dir();
        let has_vendor = current.join("vendor").is_dir();

        // If we find composer.json + artisan, it's very likely a Laravel app
        if has_composer && has_artisan {
            info!("Found Laravel project root at {:?} (composer.json + artisan)", current);
            return Some(current.to_path_buf());
        }

        // Or if we find composer.json + app/ + resources/ (Laravel app)
        if has_composer && has_app && has_resources {
            info!("Found Laravel project root at {:?} (composer.json + app + resources)", current);
            return Some(current.to_path_buf());
        }

        // Or if we find composer.json + src/ + vendor/ (Laravel package)
        // This pattern recognizes Laravel package development
        if has_composer && has_src && has_vendor {
            info!("Found Laravel package root at {:?} (composer.json + src + vendor)", current);
            return Some(current.to_path_buf());
        }

        // Move up one directory
        current = current.parent()?;
    }
}

/// Convert kebab-case to PascalCase
///
/// Used for converting Livewire component names to class names.
/// Examples:
/// - "user-profile" -> "UserProfile"
/// - "admin-dashboard" -> "AdminDashboard"
pub fn kebab_to_pascal_case(s: &str) -> String {
    s.split('-')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().chain(chars).collect(),
            }
        })
        .collect()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Extract base_path(...) calls from a line (test helper)
    fn extract_base_path(line: &str) -> Option<&str> {
        // Match: base_path('some/path') or base_path("some/path")
        if let Some(start) = line.find("base_path(") {
            let after = &line[start + 10..];
            if let Some(quote_start) = after.find(['\'', '"']) {
                let quote_char = after.chars().nth(quote_start)?;
                let after_quote = &after[quote_start + 1..];
                if let Some(quote_end) = after_quote.find(quote_char) {
                    return Some(&after_quote[..quote_end]);
                }
            }
        }
        None
    }

    #[test]
    fn test_kebab_to_pascal_case() {
        assert_eq!(kebab_to_pascal_case("user-profile"), "UserProfile");
        assert_eq!(kebab_to_pascal_case("admin-dashboard"), "AdminDashboard");
        assert_eq!(kebab_to_pascal_case("simple"), "Simple");
    }

    #[test]
    fn test_extract_base_path() {
        let line = "base_path('resources/templates'),";
        assert_eq!(extract_base_path(line), Some("resources/templates"));

        let line = "base_path(\"some/other/path\"),";
        assert_eq!(extract_base_path(line), Some("some/other/path"));
    }
}
