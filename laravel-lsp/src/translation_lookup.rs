//! Resolve Laravel translation keys to their localized strings.
//!
//! Laravel supports three translation shapes:
//!
//! - **Dotted keys** (`__('validation.required')`) — resolved through PHP files
//!   under `lang/{locale}/`. `validation.required` → `lang/en/validation.php`,
//!   key `required`.
//!
//! - **Namespaced dotted keys** (`__('filament-tables::table.label')`) — resolved
//!   through `lang/vendor/{namespace}/{locale}/{file}.php` (the published
//!   location for package translations). Vendor packages that haven't been
//!   published still hold their source translations under
//!   `vendor/{vendor}/{package}/...` but this resolver only checks the
//!   published path. Scanning unpublished package translations is a separate
//!   piece of work tracked elsewhere.
//!
//! - **Text keys** (`__('Welcome to our app')`) — resolved through the single
//!   JSON file `lang/{locale}.json`. The key IS the source string and the
//!   value is the translated string.
//!
//! All three shapes route to the same PHP-array walker from [`config_lookup`]
//! since Laravel's `.php` translation files share their exact shape with
//! config files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::config_lookup;

/// A resolved translation along with the file the value was read from.
/// The source file is rendered as a display-friendly path in hover output so
/// users can tell whether a key came from an app file, a published vendor
/// translation, or the JSON catalogue.
#[derive(Debug, Clone)]
pub struct ResolvedTranslation {
    pub value: String,
    pub source_file: PathBuf,
}

/// Resolve a translation key against a project root and locale. Returns both
/// the translated value and the file it was read from — see
/// [`ResolvedTranslation`].
///
/// For namespaced keys (`package::file.key`), the resolver tries the
/// published location first (`lang/vendor/<namespace>/...`) and falls back
/// to the unpublished vendor location when `vendor_map` is provided. See
/// [`crate::vendor_translations`] for how that map is built.
pub fn resolve_translation_detailed(
    root: &Path,
    key: &str,
    locale: &str,
    vendor_map: Option<&HashMap<String, PathBuf>>,
) -> Option<ResolvedTranslation> {
    if let Some((namespace, rest)) = split_namespace(key) {
        if let Some(r) = resolve_namespaced(root, namespace, rest, locale) {
            return Some(r);
        }
        // Published path missed — try the unpublished vendor directory.
        if let Some(map) = vendor_map {
            if let Some(dir) = map.get(namespace) {
                return resolve_namespaced_in_dir(dir, rest, locale);
            }
        }
        return None;
    }
    if is_dotted_key(key) {
        return resolve_dotted(root, key, locale);
    }
    resolve_text_key(root, key, locale)
}

/// Backwards-compatible wrapper that returns only the value, matching the
/// pre-source-file API. Used by tests that don't care about the source path.
pub fn resolve_translation(root: &Path, key: &str, locale: &str) -> Option<String> {
    resolve_translation_detailed(root, key, locale, None).map(|r| r.value)
}

/// Split a namespaced key (`package::file.key.path`) into its namespace and
/// the rest. Returns `None` for keys without a `::` separator.
fn split_namespace(key: &str) -> Option<(&str, &str)> {
    let idx = key.find("::")?;
    Some((&key[..idx], &key[idx + 2..]))
}

/// Distinguish a dotted PHP-file key (`validation.required`) from a text key
/// (`"Welcome to our app"`). Heuristic: dotted keys contain a `.` and no
/// whitespace.
fn is_dotted_key(key: &str) -> bool {
    key.contains('.') && !key.contains(' ')
}

/// Resolve a dotted key against `lang/{locale}/{file}.php`.
fn resolve_dotted(root: &Path, key: &str, locale: &str) -> Option<ResolvedTranslation> {
    let mut parts = key.split('.');
    let file = parts.next()?;
    let key_path: Vec<&str> = parts.collect();
    if key_path.is_empty() {
        return None;
    }
    let path = root.join("lang").join(locale).join(format!("{}.php", file));
    read_php_value(&path, &key_path)
}

/// Resolve a published namespaced key against
/// `lang/vendor/{namespace}/{locale}/{file}.php`.
fn resolve_namespaced(
    root: &Path,
    namespace: &str,
    rest: &str,
    locale: &str,
) -> Option<ResolvedTranslation> {
    let mut parts = rest.split('.');
    let file = parts.next()?;
    let key_path: Vec<&str> = parts.collect();
    if key_path.is_empty() {
        return None;
    }
    let path = root
        .join("lang")
        .join("vendor")
        .join(namespace)
        .join(locale)
        .join(format!("{}.php", file));
    read_php_value(&path, &key_path)
}

/// Resolve a namespaced key against an explicit lang directory — the
/// fallback used when the published path missed and the namespace was
/// discovered via [`crate::vendor_translations`].
fn resolve_namespaced_in_dir(
    lang_dir: &Path,
    rest: &str,
    locale: &str,
) -> Option<ResolvedTranslation> {
    let mut parts = rest.split('.');
    let file = parts.next()?;
    let key_path: Vec<&str> = parts.collect();
    if key_path.is_empty() {
        return None;
    }
    let path = lang_dir.join(locale).join(format!("{}.php", file));
    read_php_value(&path, &key_path)
}

/// Resolve a text key against `lang/{locale}.json`.
fn resolve_text_key(root: &Path, key: &str, locale: &str) -> Option<ResolvedTranslation> {
    let path = root.join("lang").join(format!("{}.json", locale));
    let content = std::fs::read_to_string(&path).ok()?;
    let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&content).ok()?;
    let value = map.get(key)?.as_str()?;
    Some(ResolvedTranslation {
        value: format!("'{}'", value),
        source_file: path,
    })
}

/// Shared PHP-file read + walk. Returns the bundled value + source path on hit.
fn read_php_value(path: &Path, key_path: &[&str]) -> Option<ResolvedTranslation> {
    let content = std::fs::read_to_string(path).ok()?;
    let value = config_lookup::resolve_in_source(&content, key_path)?;
    Some(ResolvedTranslation {
        value,
        source_file: path.to_path_buf(),
    })
}

#[cfg(test)]
mod tests;
