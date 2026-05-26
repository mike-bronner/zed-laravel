//! Find the registration site span for a middleware alias or container
//! binding so rename can rewrite it in place.
//!
//! ## Why this exists
//!
//! `MiddlewareRegistrationData` and `BindingRegistrationData` (in
//! `salsa_impl`) stamp the source FILE + LINE where each alias/binding
//! lives, but not the column range of the alias STRING. That's enough
//! for goto-definition (which jumps to the line) and hover (which only
//! needs the alias to look up its concrete class), but rename has to
//! produce a precise text-edit range so we don't disturb the surrounding
//! `=> SomeClass::class` or registration call.
//!
//! Re-extending the registration parsers to stamp columns is a bigger
//! lift (touches multiple service-provider parsers and the disk cache
//! schema). The simpler path: at rename time, open the file, read the
//! one line that holds the registration, and search for the quoted
//! alias name. Laravel's registration patterns put one alias per line in
//! practice, and the alias is always in a quoted PHP string literal —
//! so this is unambiguous on real code.
//!
//! ## Scope
//!
//! Used by Phase 3e for middleware aliases and container bindings
//! (`bind`, `singleton`, `scoped` in service providers). Both kinds
//! share the same registration shape (quoted string before `=>` or
//! as the first argument to a bind/singleton call) so they share this
//! locator.

use std::path::Path;

/// Error variants surfaced as user-facing toasts when rename input is
/// invalid. Kept simple — middleware aliases and binding names are far
/// less restrictive than PHP identifiers (Laravel happily registers
/// `'a-b.c'`), so only obviously-broken shapes trip a rejection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AliasNameError {
    Empty,
    ContainsQuote,
    ContainsWhitespace,
}

impl AliasNameError {
    pub fn message(&self) -> &'static str {
        match self {
            AliasNameError::Empty => "name cannot be empty",
            AliasNameError::ContainsQuote => "name cannot contain quote characters",
            AliasNameError::ContainsWhitespace => "name cannot contain whitespace",
        }
    }
}

/// Validate that `new_name` is something Laravel could plausibly accept
/// as a middleware alias or binding name. We're conservative on the
/// shape only enough to keep callers from producing PHP that fails to
/// parse — names like `auth.api` or `cache-store` are fine.
pub fn validate_alias_name(new_name: &str) -> Result<(), AliasNameError> {
    if new_name.is_empty() {
        return Err(AliasNameError::Empty);
    }
    if new_name.contains('\'') || new_name.contains('"') {
        return Err(AliasNameError::ContainsQuote);
    }
    if new_name.chars().any(char::is_whitespace) {
        return Err(AliasNameError::ContainsWhitespace);
    }
    Ok(())
}

/// One-based-line, zero-based-column span pointing at the BARE alias
/// name (the characters between the quotes — not including the quotes
/// themselves). Matches the parser's convention for call-site spans, so
/// rename can mix call-site EditTargets and decl-site EditTargets
/// without quote bookkeeping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasSpan {
    /// 0-based line number in the file.
    pub line: u32,
    /// 0-based column where the bare alias name starts (first char
    /// inside the quotes).
    pub start_column: u32,
    /// 0-based column one past the last char of the alias name (the
    /// position of the closing quote).
    pub end_column: u32,
}

/// Search forward from the registration line for the quoted alias and
/// return the span of its bare characters.
///
/// `source_line` is 1-based here because that's how
/// `MiddlewareRegistrationData::source_line` / `BindingRegistrationData::source_line`
/// are stored. The returned span is 0-based to match LSP / call-site
/// conventions — the caller does not need to convert.
///
/// ## Why we scan a range, not just `source_line`
///
/// The cached `source_line` points at the start of the registration
/// statement, which is NOT always the line that contains the alias's
/// quoted string. The parser stamps it at the structural anchor it
/// recognized — for the per-entry Kernel.php form that's the entry
/// line itself, but for the Laravel 11 bulk form
/// `$middleware->alias([ 'auth' => …, … ])` it's the line with
/// `$middleware->alias([`. The aliases are on the lines that follow.
///
/// We scan up to [`SEARCH_LINE_WINDOW`] lines forward (including
/// `source_line` itself) looking for the quoted alias. First match
/// wins — middleware aliases / binding names are unique within a
/// project so a forward scan that finds `'account.active'` once is
/// finding the right one.
///
/// Returns `None` if the file can't be read, the line is out of range,
/// or the alias isn't found in either quoted form anywhere in the
/// scan window.
pub fn locate_alias_on_line(file_path: &Path, source_line: u32, alias: &str) -> Option<AliasSpan> {
    if alias.is_empty() {
        return None;
    }
    let content = std::fs::read_to_string(file_path).ok()?;
    // source_line is 1-based; nth() is 0-based.
    let start_index = source_line.checked_sub(1)? as usize;
    for (offset, line) in content
        .lines()
        .skip(start_index)
        .take(SEARCH_LINE_WINDOW)
        .enumerate()
    {
        if let Some((start, end)) = find_alias_in_line(line, alias) {
            return Some(AliasSpan {
                line: (start_index + offset) as u32,
                start_column: start as u32,
                end_column: end as u32,
            });
        }
    }
    None
}

/// Maximum number of lines we'll search past `source_line` looking for
/// the alias's quoted string. Bulk middleware arrays in real projects
/// run a few dozen entries; 500 lines is overkill-as-a-favor for
/// pathological cases and still bounded so a corrupt cache can't make
/// us read megabytes of unrelated code.
const SEARCH_LINE_WINDOW: usize = 500;

/// Locate the quoted `alias` inside a single line and return the
/// `(start, end)` column range of the bare alias content. Helper exposed
/// for unit testing — the file I/O wrapper above is hard to fixture.
///
/// Tries single-quoted form first because Laravel's published Kernel
/// and service-provider stubs use single quotes by default; falls back
/// to double-quoted only if single-quoted isn't present. If the line has
/// both forms with the same alias, single-quoted wins — which is
/// fine since both rewrite to the same range shape and finding either
/// is correct.
pub fn find_alias_in_line(line: &str, alias: &str) -> Option<(usize, usize)> {
    for quote in &['\'', '"'] {
        let needle = format!("{q}{a}{q}", q = quote, a = alias);
        if let Some(byte_idx) = line.find(&needle) {
            // Column = char count up to byte index. PHP source is
            // overwhelmingly ASCII so byte_idx == char_idx in practice,
            // but Laravel apps with non-ASCII paths or comments above
            // could disagree. Be precise.
            let start_chars = line[..byte_idx].chars().count();
            // +1 to skip the opening quote, then alias length in chars.
            let alias_chars = alias.chars().count();
            let start = start_chars + 1;
            let end = start + alias_chars;
            return Some((start, end));
        }
    }
    None
}

#[cfg(test)]
mod tests;
