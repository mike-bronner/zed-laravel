//! Locate environment-variable key declarations across a project's
//! `.env*` files — column-accurate positions suitable for building a
//! rename `WorkspaceEdit`.
//!
//! `.env` files are line-oriented `KEY=value` text — no PHP AST walk
//! needed, just a small parser that:
//!   * skips blank lines and `#` comments
//!   * finds the first `=` on each remaining line
//!   * treats everything before the `=` (whitespace-trimmed) as the
//!     key
//!
//! `locate_keys_across_env_files` walks every `.env*` file at the
//! project root and returns one `EnvKeyLocation` per file that
//! declares the key. The same key in `.env`, `.env.example`,
//! `.env.testing`, `.env.local`, etc. all get rewritten together so
//! the project's env files stay in sync after a rename.

use std::path::{Path, PathBuf};

use crate::config_key_locator::KeyPosition;

/// One declaration of an env key, paired with the file it lives in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvKeyLocation {
    pub file_path: PathBuf,
    pub position: KeyPosition,
}

/// Find every `.env*` file at the project root that declares `key`.
/// Returns one entry per matching file. A key absent from a particular
/// file simply contributes nothing — that file is left alone.
///
/// The match strategy:
///   * Match any regular file whose name is exactly `.env` OR starts
///     with `.env.` — every Laravel-recognised variant is covered:
///     `.env`, `.env.local`, `.env.testing`, `.env.production`,
///     `.env.staging`, `.env.example`, plus any custom suffix a
///     project might invent (`.env.qa`, `.env.docker`, etc.).
///   * Skip directories named `.env` — we've seen this in misconfigured
///     projects.
///   * Read each match as UTF-8; non-UTF-8 files are silently skipped
///     (Laravel itself doesn't support non-UTF-8 env files).
pub fn locate_keys_across_env_files(root: &Path, key: &str) -> Vec<EnvKeyLocation> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // `.env` exact match OR `.env.<suffix>` prefix match. We
        // deliberately don't recurse into subdirectories — Laravel
        // doesn't read env files from anywhere but the project root.
        if !(name == ".env" || name.starts_with(".env.")) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Some(pos) = locate_in_source(&content, key) {
            out.push(EnvKeyLocation {
                file_path: path,
                position: pos,
            });
        }
    }
    out
}

/// Find `key` in `source` (a `.env`-format string) and return the
/// position of the key text only — no leading whitespace, no `=`, no
/// value. Returns `None` if the key isn't declared in this source.
///
/// First-match-wins: if a malformed env file declares `KEY=` on two
/// separate lines, we rewrite only the first. Real `.env` files don't
/// duplicate keys, so this matches Laravel's own behaviour (later
/// duplicates would override the earlier, but `phpdotenv`'s parser
/// warns on duplicates).
pub fn locate_in_source(source: &str, key: &str) -> Option<KeyPosition> {
    for (line_idx, line) in source.lines().enumerate() {
        // Blank line or comment? Skip — neither can declare a key.
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // No `=`? Not a declaration. Laravel's env parser ignores
        // lines without `=` so we do too.
        let Some(eq_pos) = line.find('=') else {
            continue;
        };
        // Pull out the key text (everything before `=`, whitespace-
        // trimmed) and compare. Exact match only — `APP_NAM` must NOT
        // match `APP_NAME`.
        let key_part = &line[..eq_pos];
        let key_trimmed = key_part.trim();
        if key_trimmed != key {
            continue;
        }
        // Column accounting: where in the line does the trimmed key
        // actually start? Subtract trailing-trim length from the
        // pre-`=` slice length to find any leading whitespace offset.
        let leading_ws = key_part.len() - key_part.trim_start().len();
        let start_col = leading_ws as u32;
        let end_col = (leading_ws + key_trimmed.len()) as u32;
        return Some(KeyPosition {
            line: line_idx as u32,
            start_column: start_col,
            end_column: end_col,
        });
    }
    None
}

#[cfg(test)]
mod tests;
