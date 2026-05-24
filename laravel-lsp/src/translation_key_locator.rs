//! Locate Laravel translation-key declarations across every locale's lang
//! file via tree-sitter — column-accurate positions suitable for building a
//! rename `WorkspaceEdit`.
//!
//! Translation keys live in `lang/<locale>/<file>.php` as nested PHP array
//! keys, just like config keys. The structural walk is identical (we delegate
//! to [`crate::config_key_locator::locate_in_source`]); the difference is
//! that translations exist in *every* registered locale, so a single rename
//! must update the corresponding key in `lang/en/auth.php`,
//! `lang/es/auth.php`, `lang/fr/auth.php`, etc. simultaneously.
//!
//! JSON-format translation files (`lang/en.json`) are out of scope for now —
//! the AST walker is PHP-specific. JSON support follows once we add a small
//! JSON walker.

use std::path::{Path, PathBuf};

use crate::config_key_locator::KeyPosition;

/// One declaration of a translation key, paired with the locale file it lives in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslationKeyLocation {
    pub file_path: PathBuf,
    pub position: KeyPosition,
}

/// Find every locale-file site where `dotted_key` appears as a declaration.
/// Walks `lang/<locale>/<file>.php` for each locale directory under
/// `<root>/lang`. Locales without the relevant file or key are simply skipped.
pub fn locate_keys_across_locales(root: &Path, dotted_key: &str) -> Vec<TranslationKeyLocation> {
    let mut parts = dotted_key.split('.');
    let Some(file_stem) = parts.next() else {
        return Vec::new();
    };
    let path_segments: Vec<&str> = parts.collect();
    if path_segments.is_empty() {
        return Vec::new();
    }

    let lang_dir = root.join("lang");
    let Ok(entries) = std::fs::read_dir(&lang_dir) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let locale_file = path.join(format!("{file_stem}.php"));
        if !locale_file.exists() {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&locale_file) else {
            continue;
        };
        if let Some(pos) = crate::config_key_locator::locate_in_source(&content, &path_segments) {
            out.push(TranslationKeyLocation {
                file_path: locale_file,
                position: pos,
            });
        }
    }

    out
}

#[cfg(test)]
mod tests;
