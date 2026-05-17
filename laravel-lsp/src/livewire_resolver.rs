//! Helpers for locating the PHP source backing a Blade view's Livewire component.
//!
//! Livewire 4 supports three component formats — this module covers the two
//! view-co-located formats; the classic `app/Livewire/Foo.php` mapping is
//! handled separately by the Backend (it depends on project-root configuration).
//!
//!   - **Multi-File Component (MFC)** — `foo.blade.php` paired with sibling
//!     `foo.php` (typically inside a `⚡foo/` directory).
//!   - **Single-File Component (SFC)** — inline `<?php new class extends
//!     Component { ... }; ?>` block inside the `.blade.php` itself.
//!   - **Class-based** (Livewire 3 carry-over) — `app/Livewire/Foo.php` plus
//!     `resources/views/livewire/foo.blade.php`. Resolved upstream in `main.rs`.
//!
//! These pure path-based helpers are factored into the library so they can be
//! exercised from unit tests without spinning up a Backend.

use std::path::{Path, PathBuf};

/// If the blade file has a sibling `.php` with the same stem and that sibling
/// contains an inline Livewire component class signature, return the sibling
/// path. Returns `None` if there is no sibling, the sibling is unreadable, or
/// the sibling exists but doesn't carry the signature.
pub fn mfc_sibling(blade_path: &Path) -> Option<PathBuf> {
    let name = blade_path.file_name()?.to_str()?;
    let stem = name.strip_suffix(".blade.php")?;
    let sibling = blade_path.with_file_name(format!("{}.php", stem));
    if !sibling.exists() {
        return None;
    }
    let content = std::fs::read_to_string(&sibling).ok()?;
    if crate::php_class::detect_inline_livewire_class(&content) {
        Some(sibling)
    } else {
        None
    }
}

/// True when the blade file at `blade_path` contains an inline Livewire
/// `new class extends Component` declaration (single-file component pattern).
pub fn blade_contains_inline_class(blade_path: &Path) -> bool {
    std::fs::read_to_string(blade_path)
        .ok()
        .map(|content| crate::php_class::detect_inline_livewire_class(&content))
        .unwrap_or(false)
}

/// Given a line of text and a 0-based column position, identify what Blade
/// variable (and optional property access) the cursor is on. Returns:
///   - `("form", None)` for cursor anywhere on `$form`
///   - `("form", Some("name"))` for cursor on `name` in `$form->name`
///   - `("", None)` for cursor right after a bare `$` (used by `$` trigger completion)
///   - `None` if the cursor isn't on any `$variable` token
///
/// Used by the hover handler, the goto-definition fallback, and the `$`
/// trigger completion path.
pub fn extract_blade_variable_at_cursor(
    line: &str,
    cursor_col: u32,
) -> Option<(String, Option<String>)> {
    let cursor = cursor_col as usize;
    if cursor > line.len() {
        return None;
    }

    let bytes = line.as_bytes();

    // Walk back to find the start of the current identifier.
    let mut ident_start = cursor;
    while ident_start > 0 {
        let c = bytes[ident_start - 1];
        if c.is_ascii_alphanumeric() || c == b'_' {
            ident_start -= 1;
        } else {
            break;
        }
    }

    // Walk forward to find the end of the current identifier.
    let mut ident_end = cursor;
    while ident_end < bytes.len() {
        let c = bytes[ident_end];
        if c.is_ascii_alphanumeric() || c == b'_' {
            ident_end += 1;
        } else {
            break;
        }
    }

    if ident_start >= ident_end {
        // Cursor not on any identifier; handle the bare-`$` trigger case
        // (cursor sits immediately after a `$` with no identifier yet).
        if ident_start > 0 && bytes[ident_start - 1] == b'$' {
            return Some((String::new(), None));
        }
        return None;
    }

    let ident = &line[ident_start..ident_end];

    // Case A: cursor on the variable itself (preceded by `$`).
    if ident_start > 0 && bytes[ident_start - 1] == b'$' {
        return Some((ident.to_string(), None));
    }

    // Case B: cursor on a property name preceded by `->`. Walk back from
    // `ident_start` past `->` and look for the originating `$variable`.
    if ident_start >= 2 && &line[ident_start - 2..ident_start] == "->" {
        let mut probe = ident_start - 2;
        while probe > 0 {
            let c = bytes[probe - 1];
            if c.is_ascii_alphanumeric() || c == b'_' {
                probe -= 1;
            } else {
                break;
            }
        }
        if probe < ident_start - 2 && probe > 0 && bytes[probe - 1] == b'$' {
            let var_name = &line[probe..ident_start - 2];
            return Some((var_name.to_string(), Some(ident.to_string())));
        }
    }

    None
}
