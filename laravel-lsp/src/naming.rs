//! String-case and path-segment conversions used across Phase 3 rename work.
//!
//! Laravel's component-naming surface has four shapes the same name can take:
//!
//!   - **dotted** — `admin.user-list` (the form that appears in Blade tags and
//!     `view()` / `route()` calls)
//!   - **kebab segments** — `user-list` (each dotted segment in Blade-tag form)
//!   - **Pascal segments** — `UserList` (each segment in PHP class-name form)
//!   - **PSR-4 path** — `Admin/UserList` or `Admin\UserList` (file or namespace
//!     join over Pascal segments)
//!
//! Plus Livewire 4's `⚡` filename prefix, which is technically optional —
//! disabled via `config/livewire.php` → `make_command.emoji = false` — and so
//! must be handled symmetrically on input (strip) and output (preserve).
//!
//! The functions here are pure and allocation-conscious where reasonable.
//! Phase 2 doesn't use them; they exist to back the class-backed rename kinds
//! landing in Phase 3 (View, Component, Livewire) plus the file-rename inverse
//! path in `workspace/willRenameFiles`.

/// The Livewire 4 single-file / multi-file component filename prefix.
/// See `config/livewire.php` → `make_command.emoji`.
pub const LIVEWIRE_EMOJI: char = '\u{26A1}';

/// Variation selectors that may follow the emoji in some sources. Matches the
/// pattern Livewire itself uses to strip the prefix:
/// `preg_replace('/⚡[\x{FE0E}\x{FE0F}]?/u', '', $name)`.
const VARIATION_SELECTOR_TEXT: char = '\u{FE0E}';
const VARIATION_SELECTOR_EMOJI: char = '\u{FE0F}';

/// Re-export of the existing kebab→Pascal helper. Lives in `config` for
/// historical reasons; new Phase 3 code should reach for it via `naming`.
pub use crate::config::kebab_to_pascal_case as kebab_to_pascal;

/// Convert a `PascalCase` identifier to `kebab-case`.
///
/// Used when going from a PHP class name (`UserProfile`) back to a Blade-tag
/// form (`user-profile`). Treats every uppercase character past index 0 as a
/// word boundary — adequate for the simple Pascal names Laravel conventions
/// produce. Acronyms (`HTTPClient`) become `h-t-t-p-client`, which matches
/// the simple convention; if real-world acronym handling is needed later,
/// a smarter variant can land alongside.
pub fn pascal_to_kebab(s: &str) -> String {
    let mut result = String::with_capacity(s.len() + 4);
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            result.push('-');
        }
        result.extend(c.to_lowercase());
    }
    result
}

/// Split a dotted component name into its segments.
///
/// `"admin.user-list"` → `["admin", "user-list"]`.
/// Single-segment names return a single-element vec.
pub fn split_dotted(s: &str) -> Vec<&str> {
    s.split('.').collect()
}

/// Convert a dotted component name to a PSR-4 namespace path with backslash
/// separators.
///
/// `"admin.user-list"` → `"Admin\\UserList"`. Each segment is kebab→Pascal
/// converted and joined with `\`. Used when computing a class FQN from a
/// Livewire or Blade-component tag name.
pub fn dotted_to_namespace(s: &str) -> String {
    join_pascal_segments(s, '\\')
}

/// Convert a dotted component name to a forward-slash file path with each
/// segment in PascalCase.
///
/// `"admin.user-list"` → `"Admin/UserList"`. The caller appends `.php` or
/// joins with a root directory.
pub fn dotted_to_class_path(s: &str) -> String {
    join_pascal_segments(s, '/')
}

fn join_pascal_segments(s: &str, sep: char) -> String {
    let segments: Vec<String> = split_dotted(s).into_iter().map(kebab_to_pascal).collect();
    segments.join(&sep.to_string())
}

/// True if `s` starts with the Livewire `⚡` prefix (with or without a
/// trailing variation selector).
pub fn has_emoji(s: &str) -> bool {
    s.starts_with(LIVEWIRE_EMOJI)
}

/// Return `s` with any leading `⚡` (and its optional variation selector)
/// removed. Returns the input unchanged when no prefix is present.
pub fn strip_emoji(s: &str) -> &str {
    let Some(rest) = s.strip_prefix(LIVEWIRE_EMOJI) else {
        return s;
    };
    // The variation selector is optional — match Livewire's regex which
    // treats both U+FE0E and U+FE0F as discardable here.
    rest.strip_prefix(VARIATION_SELECTOR_TEXT)
        .or_else(|| rest.strip_prefix(VARIATION_SELECTOR_EMOJI))
        .unwrap_or(rest)
}

/// Return `s` with the `⚡` prefix added or removed based on `enabled`.
///
/// `enabled = true` and the input lacks the prefix → prepend it.
/// `enabled = true` and the input already has it → return it unchanged.
/// `enabled = false` → strip any existing prefix.
///
/// Used when generating a new MFC directory or SFC filename during rename:
/// the desired prefix state is read from `config/livewire.php` →
/// `make_command.emoji`, but we don't want to double-prefix existing names.
pub fn with_emoji(s: &str, enabled: bool) -> String {
    if enabled {
        if has_emoji(s) {
            s.to_string()
        } else {
            let mut out = String::with_capacity(s.len() + LIVEWIRE_EMOJI.len_utf8());
            out.push(LIVEWIRE_EMOJI);
            out.push_str(s);
            out
        }
    } else {
        strip_emoji(s).to_string()
    }
}

#[cfg(test)]
mod tests;
