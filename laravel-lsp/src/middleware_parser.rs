//! Middleware and class resolution utilities
//!
//! This module provides utilities for resolving PHP class names to file paths
//! using PSR-4 autoloading conventions.

use std::path::{Path, PathBuf};

/// Strip parameters from a middleware reference (e.g., "auth:sanctum" -> "auth").
///
/// Laravel middleware can be invoked with parameters that get passed to the
/// middleware's `handle()` method — `auth:sanctum` is the `auth` alias with
/// `sanctum` as a guard parameter, `throttle:60,1` is `throttle` with rate-limit
/// parameters. The portion after the colon is not part of the alias and must
/// be stripped before looking the alias up in the registry.
pub fn middleware_base_alias(name: &str) -> &str {
    name.split(':').next().unwrap_or(name)
}

/// Resolve a fully qualified class name to a file path
///
/// Converts namespace notation to file path using PSR-4 autoloading conventions
/// Example: App\Http\Middleware\Authenticate -> app/Http/Middleware/Authenticate.php
pub fn resolve_class_to_file(class_name: &str, root_path: &Path) -> Option<PathBuf> {
    // Convert namespace separators to path separators
    let path_str = class_name.replace("\\", "/");

    // Common Laravel namespace mappings
    let mappings = [
        ("App/", "app/"),
        ("Illuminate/", "vendor/laravel/framework/src/Illuminate/"),
    ];

    for (namespace_prefix, path_prefix) in &mappings {
        if path_str.starts_with(namespace_prefix) {
            let relative = path_str.strip_prefix(namespace_prefix).unwrap();
            let file_path = root_path
                .join(path_prefix)
                .join(relative)
                .with_extension("php");

            // Return the expected path regardless of whether it exists
            // The caller will check existence and create appropriate diagnostics
            return Some(file_path);
        }
    }

    None
}

#[cfg(test)]
mod tests;
