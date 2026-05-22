//! Route discovery — find named routes across the project, packages, and framework.
//!
//! Laravel route names are registered via `->name('X')` calls in many places:
//! - `routes/*.php` (project, recursively — catches `auth.php`, custom splits)
//! - `vendor/*/routes/*.php` (packages — Fortify, Telescope, Filament, Horizon, etc.)
//! - Service provider `boot()` methods that call `Route::get(...)->name(...)` directly
//! - Macro definitions in `Route::macro('foo', function () { ... })`
//! - Files registered via `loadRoutesFrom('path')` at non-standard locations
//! - Filament `Panel::routes(fn () => ...)` closures
//!
//! Rather than hard-code well-known files, this module discovers candidates by
//! scanning for files whose content shows route-registration shape (a route
//! facade/router token AND a `->name(` call). This naturally captures every
//! pattern listed above without needing per-package knowledge.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

/// A located route definition. Stored in [`RouteIndex`] keyed by route name.
#[derive(Debug, Clone)]
pub struct RouteDefinition {
    /// Absolute file path containing the `->name('X')` call.
    pub file: PathBuf,
    /// 0-based line of the `->name(` callsite.
    pub line: u32,
    /// 0-based column where the `->name(` callsite begins.
    pub column: u32,
    /// 0-based column where the `->name(` callsite ends (exclusive).
    pub end_column: u32,
    /// Source priority. Higher wins on conflict (app overrides package overrides framework).
    pub priority: u8,
}

/// Priority levels used when multiple files define the same route name.
/// Higher beats lower — if app and Fortify both register `login`, the app's wins.
pub const PRIORITY_FRAMEWORK: u8 = 0;
pub const PRIORITY_PACKAGE: u8 = 1;
pub const PRIORITY_APP: u8 = 2;

/// In-memory map of route name → definition location.
#[derive(Debug, Default, Clone)]
pub struct RouteIndex {
    pub routes: HashMap<String, RouteDefinition>,
}

impl RouteIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a definition. Higher priority replaces lower; equal priority keeps first.
    pub fn insert(&mut self, name: String, def: RouteDefinition) {
        match self.routes.get(&name) {
            Some(existing) if existing.priority >= def.priority => {}
            _ => {
                self.routes.insert(name, def);
            }
        }
    }

    pub fn get(&self, name: &str) -> Option<&RouteDefinition> {
        self.routes.get(name)
    }

    pub fn len(&self) -> usize {
        self.routes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }
}

/// A file containing route definitions, paired with its priority tier.
#[derive(Debug, Clone)]
pub struct RouteFile {
    pub path: PathBuf,
    pub priority: u8,
}

/// Walk the project to discover every file likely to define named routes.
///
/// The returned list is deduplicated by path. Order is not significant — the
/// final index resolves conflicts via priority.
pub fn discover_route_files(root: &Path) -> Vec<RouteFile> {
    let mut seen: HashMap<PathBuf, u8> = HashMap::new();

    // Project routes/ — recursive, every *.php
    let project_routes = root.join("routes");
    if project_routes.exists() {
        for path in walk_php_files(&project_routes, 6) {
            promote(&mut seen, path, PRIORITY_APP);
        }
    }

    // Package routes/*.php and any vendor file whose content registers routes.
    let vendor = root.join("vendor");
    if vendor.exists() {
        for entry in WalkDir::new(&vendor)
            .max_depth(8)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if path.extension().is_none_or(|ext| ext != "php") {
                continue;
            }

            // Anything under a vendor `routes/` subdirectory is a route file
            // by Laravel package convention.
            if is_under_routes_dir(path) {
                promote(&mut seen, path.to_path_buf(), priority_for_vendor_path(path));
                continue;
            }

            // Otherwise content-match: register the file only if it both
            // contains a route-registration token AND a `->name(` call.
            // This is what catches macro bodies (Laravel UI's
            // AuthRouteMethods), service-provider `boot()` registrations,
            // and Filament-style `Panel::routes(fn () => ...)` panels.
            if file_registers_named_routes(path) {
                promote(&mut seen, path.to_path_buf(), priority_for_vendor_path(path));
            }
        }
    }

    // App-level service providers and bootstrap/app.php often register routes
    // directly in `boot()`. Scan with content match to avoid pulling in
    // unrelated *.php files in app/.
    for candidate in app_provider_candidates(root) {
        if candidate.exists() && file_registers_named_routes(&candidate) {
            promote(&mut seen, candidate, PRIORITY_APP);
        }
    }

    seen.into_iter()
        .map(|(path, priority)| RouteFile { path, priority })
        .collect()
}

/// Build a complete route name → location index from the given files.
pub fn build_route_index(files: &[RouteFile]) -> RouteIndex {
    let mut index = RouteIndex::new();
    for file in files {
        if let Ok(content) = std::fs::read_to_string(&file.path) {
            for def in extract_named_routes(&content, &file.path, file.priority) {
                if let Some(name) = def.0 {
                    index.insert(name, def.1);
                }
            }
        }
    }
    index
}

/// Extract every `->name('X')` callsite from the given source.
///
/// Returns `(name, RouteDefinition)` pairs. The match is line-based and tolerant
/// of whitespace inside the call: `->name('X')`, `->name ( "X" )`, etc.
///
/// Only matches single-quoted or double-quoted string literals. Variable
/// arguments (`->name($var)`) are skipped because we can't resolve them
/// statically.
pub fn extract_named_routes(
    content: &str,
    file: &Path,
    priority: u8,
) -> Vec<(Option<String>, RouteDefinition)> {
    let mut results = Vec::new();
    let bytes = content.as_bytes();
    let pattern = b"->name";
    let mut i = 0;

    while i + pattern.len() <= bytes.len() {
        if &bytes[i..i + pattern.len()] != pattern {
            i += 1;
            continue;
        }

        // After "->name", skip whitespace, expect '('.
        let mut j = i + pattern.len();
        while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'(' {
            i += 1;
            continue;
        }
        j += 1;
        while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
            j += 1;
        }
        if j >= bytes.len() {
            i += 1;
            continue;
        }
        let quote = bytes[j];
        if quote != b'\'' && quote != b'"' {
            i += 1;
            continue;
        }
        j += 1;
        let str_start = j;
        while j < bytes.len() && bytes[j] != quote && bytes[j] != b'\n' {
            // Allow simple escapes — skip the next byte.
            if bytes[j] == b'\\' && j + 1 < bytes.len() {
                j += 2;
            } else {
                j += 1;
            }
        }
        if j >= bytes.len() || bytes[j] != quote {
            i += 1;
            continue;
        }
        let name = match std::str::from_utf8(&bytes[str_start..j]) {
            Ok(s) => s.to_string(),
            Err(_) => {
                i += 1;
                continue;
            }
        };

        let (line, column) = byte_to_line_col(bytes, i);
        let end_column = column + (j - i + 1) as u32; // include closing quote
        results.push((
            Some(name),
            RouteDefinition {
                file: file.to_path_buf(),
                line,
                column,
                end_column,
                priority,
            },
        ));

        i = j + 1;
    }

    results
}

/// Quick content check — does this file likely register named routes?
///
/// Looks for both a route-registration token and a `->name(` call. False
/// positives are tolerable (the index lookup just won't find an entry); false
/// negatives are not (we'd miss valid route definitions).
///
/// Registration shape can be any of:
/// - `Route::` / `Router::` / `$router->` static or facade call
/// - `Route::macro(...)` or `RouteRegistrar` references
/// - An HTTP-verb method invocation (`->get(`, `->post(`, etc.) — covers
///   route macro bodies that bind via `$this->get(...)` (e.g., Laravel UI's
///   `AuthRouteMethods`).
fn file_registers_named_routes(path: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };
    content_registers_named_routes(&content)
}

/// Content-only variant for testability — same logic as
/// [`file_registers_named_routes`] but operates on a string.
fn content_registers_named_routes(content: &str) -> bool {
    if !content.contains("->name(") {
        return false;
    }
    if content.contains("Route::")
        || content.contains("Router::")
        || content.contains("$router->")
        || content.contains("$this->router->")
        || content.contains("RouteRegistrar")
    {
        return true;
    }

    // HTTP verb invocations also imply route registration shape.
    // Laravel's router/registrar exposes these methods, so finding any of
    // them paired with `->name(` strongly indicates a route definition.
    const VERB_CALLS: &[&str] = &[
        "->get(",
        "->post(",
        "->put(",
        "->patch(",
        "->delete(",
        "->options(",
        "->any(",
        "->match(",
        "->redirect(",
        "->view(",
        "->resource(",
        "->apiResource(",
        "->fallback(",
    ];
    VERB_CALLS.iter().any(|verb| content.contains(verb))
}

fn walk_php_files(dir: &Path, max_depth: usize) -> Vec<PathBuf> {
    WalkDir::new(dir)
        .max_depth(max_depth)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "php"))
        .map(|e| e.path().to_path_buf())
        .collect()
}

fn app_provider_candidates(root: &Path) -> Vec<PathBuf> {
    let mut paths = vec![
        root.join("bootstrap/app.php"),
        root.join("app/Http/Kernel.php"),
    ];
    let providers = root.join("app/Providers");
    if providers.exists() {
        paths.extend(walk_php_files(&providers, 4));
    }
    paths
}

fn is_under_routes_dir(path: &Path) -> bool {
    path.components()
        .any(|c| c.as_os_str().eq_ignore_ascii_case("routes"))
}

fn priority_for_vendor_path(path: &Path) -> u8 {
    let s = path.to_string_lossy();
    if s.contains("/laravel/framework/") || s.contains("\\laravel\\framework\\") {
        PRIORITY_FRAMEWORK
    } else {
        PRIORITY_PACKAGE
    }
}

fn promote(seen: &mut HashMap<PathBuf, u8>, path: PathBuf, priority: u8) {
    seen.entry(path)
        .and_modify(|p| {
            if priority > *p {
                *p = priority;
            }
        })
        .or_insert(priority);
}

fn byte_to_line_col(bytes: &[u8], byte_offset: usize) -> (u32, u32) {
    let mut line = 0u32;
    let mut last_newline: i64 = -1;
    for (idx, b) in bytes.iter().enumerate().take(byte_offset) {
        if *b == b'\n' {
            line += 1;
            last_newline = idx as i64;
        }
    }
    let column = (byte_offset as i64 - last_newline - 1) as u32;
    (line, column)
}

#[cfg(test)]
mod tests;
