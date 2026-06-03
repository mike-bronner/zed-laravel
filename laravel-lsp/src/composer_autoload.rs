//! Real PSR-4 class resolution from Composer's autoload data.
//!
//! The previous heuristic mapped FQCN → file path by lowercasing namespace
//! segments (e.g. `Laravel\Passport\Token` → `vendor/laravel/passport/...`).
//! That works for packages whose directory name exactly matches the
//! lowercased namespace, but breaks for hyphenated packages:
//!
//! - Namespace `CrossBibleInc\BibleModels\` → package dir `crossbibleinc/bible-models`
//! - Namespace `Symfony\Component\HttpFoundation\` → `symfony/http-foundation`
//!
//! Composer normalizes package names with hyphens by convention, but the
//! namespace itself is camelCase. There's no reliable way to derive one
//! from the other — Composer reads it from each package's `composer.json`.
//!
//! This module loads the canonical autoload data once per project:
//!
//! - `<project>/composer.json` — the project's own `autoload` + `autoload-dev`
//!   (typically `App\\` → `app/`, `Database\\Factories\\` → `database/factories/`)
//! - `<project>/vendor/composer/installed.json` — every installed package's
//!   `autoload.psr-4` plus its `install-path` (so we know where to look)
//!
//! Resolution: walk the prefix map, find the longest matching PSR-4 prefix
//! for the FQCN, compute the relative path, append `.php`. If the resulting
//! file exists on disk, return it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Cached autoload data for one Laravel project. The PSR-4 prefix list is
/// sorted longest-first so a more specific prefix wins over a less specific
/// one (e.g. `App\Models\` over `App\`).
#[derive(Debug)]
pub struct ComposerAutoload {
    /// (psr4_prefix_no_trailing_backslash, absolute_source_roots)
    prefixes: Vec<(String, Vec<PathBuf>)>,
}

impl ComposerAutoload {
    /// Resolve `Acme\Foo\Bar` to its absolute file path if a PSR-4 mapping
    /// matches and the file exists. Returns `None` for FQCNs with no
    /// matching prefix or whose computed path doesn't exist on disk.
    ///
    /// FQCNs may have a leading `\` (fully qualified) — stripped before
    /// lookup. The match must end on a namespace separator boundary so
    /// `App\Models` doesn't accidentally match a prefix `App\Mo`.
    pub fn resolve(&self, fqcn: &str) -> Option<PathBuf> {
        let normalized = fqcn.trim_start_matches('\\');
        for (prefix, source_roots) in &self.prefixes {
            // `prefix` here is the PSR-4 key with the trailing `\\` stripped.
            // For boundary safety, the FQCN must either equal the prefix
            // (impossible — would mean class without namespace) or start
            // with `<prefix>\` followed by at least one segment.
            if !normalized.starts_with(prefix.as_str()) {
                continue;
            }
            let after_prefix = &normalized[prefix.len()..];
            if !after_prefix.starts_with('\\') || after_prefix.len() < 2 {
                continue;
            }
            // Strip the boundary `\` and split the remainder into segments
            // so we can join them as path components.
            let rest = &after_prefix[1..];
            let mut rel_path = PathBuf::new();
            for seg in rest.split('\\') {
                rel_path.push(seg);
            }
            // PSR-4 always appends `.php` to the leaf component.
            rel_path.set_extension("php");
            for source_root in source_roots {
                let candidate = source_root.join(&rel_path);
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
        None
    }

    /// Resolve a PHP **namespace** (not a class) to the source directory/-ies
    /// that hold it, for every PSR-4 prefix that's an ancestor. The inverse of
    /// [`resolve`]: where `resolve` maps `Acme\Foo\Bar` → `.../Bar.php`, this
    /// maps the namespace `Acme\Foo` → `.../Foo/` so callers can walk it.
    ///
    /// Used by class-based component completion (`Blade::componentNamespace`):
    /// the registered PHP namespace is turned into a directory whose class
    /// files become `<x-prefix::name>` candidates. Only directories that exist
    /// on disk are returned; a namespace with no matching prefix yields empty.
    pub fn resolve_namespace_dirs(&self, namespace: &str) -> Vec<PathBuf> {
        let normalized = namespace.trim_start_matches('\\').trim_end_matches('\\');
        let mut dirs = Vec::new();
        for (prefix, source_roots) in &self.prefixes {
            // The namespace either equals the PSR-4 prefix exactly, or extends
            // it past a `\` boundary (so prefix `App` matches `App\View` but
            // not `Application`). `prefix` is stored without a trailing `\`.
            let rel = if normalized == prefix {
                Some(PathBuf::new())
            } else if let Some(after) = normalized.strip_prefix(prefix.as_str()) {
                after.strip_prefix('\\').map(|rest| {
                    let mut p = PathBuf::new();
                    for seg in rest.split('\\') {
                        p.push(seg);
                    }
                    p
                })
            } else {
                None
            };

            if let Some(rel) = rel {
                for source_root in source_roots {
                    let dir = source_root.join(&rel);
                    if dir.is_dir() {
                        dirs.push(dir);
                    }
                }
            }
        }
        dirs
    }

    /// Parse autoload data fresh from disk. Doesn't touch the cache —
    /// callers should normally use [`for_project`] instead.
    pub fn load(project_root: &Path) -> Self {
        let mut prefixes: Vec<(String, Vec<PathBuf>)> = Vec::new();

        // Project's own autoload (e.g. App\ → app/, Database\Factories\ → ...).
        let project_composer = project_root.join("composer.json");
        if let Ok(text) = std::fs::read_to_string(&project_composer) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                Self::collect_psr4(value.get("autoload"), project_root, &mut prefixes);
                Self::collect_psr4(value.get("autoload-dev"), project_root, &mut prefixes);
            }
        }

        // Installed packages — Composer writes this on `composer install`
        // / `composer update`. Each entry has its own PSR-4 mappings and an
        // `install-path` relative to vendor/composer/.
        let installed_path = project_root
            .join("vendor")
            .join("composer")
            .join("installed.json");
        if let Ok(text) = std::fs::read_to_string(&installed_path) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                // installed.json shape varies: Composer 2 uses { "packages": [...] },
                // Composer 1 was a bare array. Handle both for robustness.
                let packages = value
                    .get("packages")
                    .and_then(|v| v.as_array())
                    .or_else(|| value.as_array());
                if let Some(pkgs) = packages {
                    let vendor_composer = project_root.join("vendor").join("composer");
                    for pkg in pkgs {
                        let install_path = match pkg.get("install-path").and_then(|v| v.as_str()) {
                            Some(p) => p,
                            None => continue,
                        };
                        let pkg_root = vendor_composer.join(install_path);
                        let pkg_root = pkg_root.canonicalize().unwrap_or(pkg_root);
                        Self::collect_psr4(pkg.get("autoload"), &pkg_root, &mut prefixes);
                    }
                }
            }
        }

        // Longest prefix first so `App\Models\` beats `App\` when both
        // would otherwise match an FQCN like `App\Models\User`.
        prefixes.sort_by_key(|entry| std::cmp::Reverse(entry.0.len()));

        Self { prefixes }
    }

    /// Pull PSR-4 entries out of an `autoload` (or `autoload-dev`) JSON
    /// blob and append them to the accumulator. Handles both PSR-4 value
    /// shapes: a single path string, or an array of paths.
    fn collect_psr4(
        autoload: Option<&serde_json::Value>,
        base_dir: &Path,
        out: &mut Vec<(String, Vec<PathBuf>)>,
    ) {
        let Some(autoload) = autoload else { return };
        let Some(psr4) = autoload.get("psr-4").and_then(|v| v.as_object()) else {
            return;
        };
        for (prefix, paths_value) in psr4 {
            // Strip trailing `\` so we can match by string-prefix and then
            // verify the boundary character ourselves.
            let key = prefix.trim_end_matches('\\').to_string();
            let mut absolute_paths = Vec::new();
            match paths_value {
                serde_json::Value::String(s) => {
                    absolute_paths.push(base_dir.join(s));
                }
                serde_json::Value::Array(arr) => {
                    for p in arr {
                        if let Some(s) = p.as_str() {
                            absolute_paths.push(base_dir.join(s));
                        }
                    }
                }
                _ => {}
            }
            if !absolute_paths.is_empty() {
                out.push((key, absolute_paths));
            }
        }
    }

    /// Return the cached autoload for `project_root`, loading it lazily on
    /// first call. Cache key is the canonical project root path; cache
    /// outlives the LSP session.
    ///
    /// The autoload is loaded into a process-wide cache rather than per-LSP
    /// instance because Composer rarely changes during an editing session
    /// — when it does (e.g. `composer require ...`), the user typically
    /// restarts the editor or reloads the extension anyway.
    pub fn for_project(project_root: &Path) -> &'static ComposerAutoload {
        static CACHE: OnceLock<Mutex<HashMap<PathBuf, &'static ComposerAutoload>>> =
            OnceLock::new();
        let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));

        let key = project_root
            .canonicalize()
            .unwrap_or_else(|_| project_root.to_path_buf());

        // Fast path: already cached.
        {
            let map = cache.lock().expect("composer_autoload cache poisoned");
            if let Some(found) = map.get(&key) {
                return found;
            }
        }

        // Build fresh and intern. `Box::leak` is safe here because the
        // cache lives for the entire process lifetime — leaks bounded by
        // the number of distinct projects ever opened.
        let loaded = Box::leak(Box::new(ComposerAutoload::load(project_root)));
        let mut map = cache.lock().expect("composer_autoload cache poisoned");
        // Race-safe: if another thread already inserted while we built,
        // drop ours and use theirs. (We can't free the leak, but a
        // duplicate static autoload is harmless.)
        if let Some(existing) = map.get(&key) {
            return existing;
        }
        map.insert(key.clone(), loaded);
        loaded
    }

    /// Direct accessor for tests — count of registered PSR-4 prefixes.
    #[cfg(test)]
    pub fn prefix_count(&self) -> usize {
        self.prefixes.len()
    }
}

#[cfg(test)]
mod tests;
