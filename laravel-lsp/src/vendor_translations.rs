//! Discover translation-namespace registrations across vendor packages.
//!
//! Laravel packages register their translations in a `ServiceProvider::boot()`
//! method via:
//!
//! ```php
//! $this->loadTranslationsFrom(__DIR__.'/../resources/lang', 'package-namespace');
//! ```
//!
//! The published location for those translations is `lang/vendor/<namespace>/`
//! in the host project, which [`crate::translation_lookup`] already handles.
//! This module fills the gap for translations that **haven't been published** —
//! it walks `vendor/` for service providers that call `loadTranslationsFrom`,
//! extracts each `(namespace, directory)` pair, and returns a map the
//! resolver can fall back to when the published path doesn't exist.
//!
//! No on-disk cache yet — the scan runs once at LSP startup and the result
//! lives in memory. A composer.lock-keyed cache (like
//! [`crate::config::scan_vendor_for_component_aliases`]) is a worthwhile
//! follow-up once the scan time becomes a noticeable cost on first hover.

use lazy_static::lazy_static;
use regex::Regex;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

lazy_static! {
    /// Matches `$this->loadTranslationsFrom(__DIR__.'/relative/path', 'namespace')`.
    /// Captures the relative path and the namespace.
    static ref LOAD_TRANSLATIONS_RE: Regex = Regex::new(
        r#"\$this->loadTranslationsFrom\s*\(\s*__DIR__\s*\.\s*['"]([^'"]+)['"]\s*,\s*['"]([^'"]+)['"]\s*\)"#
    ).unwrap();

    /// Matches a fluent package-builder name declaration: `->name('package')`.
    /// Builder-convention providers (e.g. Filament via laravel-package-tools)
    /// never call `loadTranslationsFrom` with literal arguments — the real call
    /// runs in a base class as
    /// `$this->loadTranslationsFrom($computedDir, $this->package->shortName())`.
    /// This pair of patterns reconstructs that registration form, the same way
    /// the view-namespace discovery in [`crate::salsa_impl`] does for
    /// `->hasViews()`.
    static ref BUILDER_NAME_RE: Regex = Regex::new(
        r#"->name\s*\(\s*['"]([^'"]+)['"]\s*\)"#
    ).unwrap();

    /// Matches the builder translation capability: `->hasTranslations()`.
    /// Unlike `->hasViews('ns')` there is no explicit-namespace argument —
    /// the namespace is always the package short-name.
    static ref BUILDER_HAS_TRANSLATIONS_RE: Regex = Regex::new(
        r#"->hasTranslations\s*\(\s*\)"#
    ).unwrap();
}

/// Walk `vendor/` for service providers that register translation namespaces.
/// Returns a map of `namespace → absolute lang directory`.
///
/// The scan applies two cheap gates before parsing any file:
/// - **Filename**: must contain `ServiceProvider`
/// - **Content substring**: must contain `loadTranslationsFrom`
///
/// Roughly the same shape as
/// [`crate::config::scan_vendor_for_component_aliases`] — these two scans
/// could share a single vendor-walk pass once we add the persistent cache.
pub fn scan_vendor_translation_namespaces(root: &Path) -> HashMap<String, PathBuf> {
    let vendor = root.join("vendor");
    if !vendor.is_dir() {
        return HashMap::new();
    }

    let mut namespaces: HashMap<String, PathBuf> = HashMap::new();

    for entry in walkdir::WalkDir::new(&vendor)
        .max_depth(10)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("php") {
            continue;
        }
        let filename_matches = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.contains("ServiceProvider"))
            .unwrap_or(false);
        if !filename_matches {
            continue;
        }

        let Ok(source) = fs::read_to_string(path) else {
            continue;
        };
        if !source.contains("loadTranslationsFrom") && !source.contains("hasTranslations") {
            continue;
        }

        extract_translations_from(&source, path, &mut namespaces);
        extract_builder_translations_from(&source, path, &mut namespaces);
    }

    namespaces
}

/// Apply [`LOAD_TRANSLATIONS_RE`] to the given source. Each match contributes
/// a `namespace → absolute_lang_dir` entry. The relative path is resolved
/// against the provider file's directory (the `__DIR__` reference).
///
/// First-match-wins on namespace conflict — service-provider boot order is
/// non-deterministic and we have no good way to rank packages without a full
/// composer dependency graph.
fn extract_translations_from(
    source: &str,
    provider_path: &Path,
    namespaces: &mut HashMap<String, PathBuf>,
) {
    let provider_dir = match provider_path.parent() {
        Some(d) => d,
        None => return,
    };

    for cap in LOAD_TRANSLATIONS_RE.captures_iter(source) {
        let (Some(rel), Some(ns)) = (cap.get(1), cap.get(2)) else {
            continue;
        };
        // PHP source: `__DIR__.'/../resources/lang'` — the captured fragment
        // starts with `/`. Rust's `Path::join` treats any path starting with
        // `/` as absolute and discards the receiver, so we strip leading `/`
        // and `./` before joining onto the provider directory.
        let rel_str = rel
            .as_str()
            .trim_start_matches('/')
            .trim_start_matches("./");
        let lang_dir = provider_dir.join(rel_str);
        let resolved = lang_dir.canonicalize().unwrap_or(lang_dir);
        namespaces
            .entry(ns.as_str().to_string())
            .or_insert(resolved);
    }
}

/// Reconstruct the fluent package-builder translation registration:
/// `$package->name('filament-tables')->hasTranslations()`. The builder's base
/// class registers `loadTranslationsFrom(<pkg>/resources/lang, shortName())`
/// at runtime — both arguments computed, invisible to [`LOAD_TRANSLATIONS_RE`].
/// The namespace is the package short-name (leading `laravel-` stripped) and
/// the directory follows the builder's `basePath('/../resources/lang')`
/// convention: one level up from the provider's `src/` dir.
fn extract_builder_translations_from(
    source: &str,
    provider_path: &Path,
    namespaces: &mut HashMap<String, PathBuf>,
) {
    if !BUILDER_HAS_TRANSLATIONS_RE.is_match(source) {
        return;
    }
    let Some(name_cap) = BUILDER_NAME_RE.captures(source) else {
        return;
    };
    let Some(package_name) = name_cap.get(1) else {
        return;
    };
    let namespace = crate::salsa_impl::builder_short_name(package_name.as_str());
    if namespace.is_empty() {
        return;
    }

    let Some(provider_dir) = provider_path.parent() else {
        return;
    };
    let lang_dir = provider_dir.join("../resources/lang");
    let resolved = lang_dir.canonicalize().unwrap_or(lang_dir);
    namespaces.entry(namespace).or_insert(resolved);
}

#[cfg(test)]
mod tests;
