//! Extract Artisan command metadata from a PHP `Command` class.
//!
//! The discovery half of issue #62: a class that extends
//! `Illuminate\Console\Command` declares the command it provides through a
//! `protected $signature = 'emails:send {user} {--force}';` property. Goto-
//! definition and hover for command-string call sites (`Artisan::call('emails:send')`)
//! need two things out of such a class:
//!
//! 1. the **command name** — the leading token of the signature, before the
//!    first whitespace (`emails:send`), which is what call sites reference; and
//! 2. the **source position** of the signature string's content, so a later
//!    goto-definition can land the cursor precisely on the declaration.
//!
//! This module is the pure, file-content-only primitive for that extraction —
//! the same shape as [`crate::route_name_locator`] (positions of a string
//! literal's content) and [`crate::php_class`] (regex-based PHP parsing). It
//! does no I/O and holds no index state; the Salsa-backed command index and the
//! call-site locator build on top of it.
//!
//! ## Position convention
//!
//! Mirrors the rest of the stack (see `CLAUDE.md` → *Position Indexing
//! Convention*): all rows/columns are **0-based**, and `start_column` /
//! `end_column` bracket the string *content* — the first character after the
//! opening quote through one past the last character before the closing quote.

use lazy_static::lazy_static;
use regex::Regex;

/// One Artisan command discovered on a `Command` subclass.
///
/// `name` is the resolvable identifier call sites use; `raw_signature` keeps the
/// full declaration (arguments and options included) for hover summaries. The
/// position fields point at the signature string's *content* so goto-definition
/// can land on the declaration without re-parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSignature {
    /// The command name — the leading token of the signature, before the first
    /// whitespace (`emails:send` from `emails:send {user} {--force}`).
    pub name: String,
    /// The full signature string content (no surrounding quotes), as written.
    pub raw_signature: String,
    /// 0-based row of the signature string content.
    pub line: u32,
    /// 0-based column of the first content character (after the opening quote).
    pub start_column: u32,
    /// 0-based column one past the last content character (before the closing
    /// quote).
    pub end_column: u32,
}

/// The command name carried by a `$signature` value: everything up to the first
/// ASCII whitespace, trimmed. Laravel parses the signature the same way — the
/// leading token is the command name and the remainder declares arguments and
/// options.
///
/// Returns an empty string for an all-whitespace input; callers treat an empty
/// name as "no usable command" (see [`extract_command_signature`]).
pub fn command_name_from_signature(raw: &str) -> &str {
    raw.trim_start()
        .split(|c: char| c.is_ascii_whitespace())
        .next()
        .unwrap_or("")
        .trim()
}

/// Whether `content` declares a class that extends `Command` — the marker for
/// an Artisan command class. Matches both the bare `extends Command` (the
/// common case, with a `use Illuminate\Console\Command;` import) and a
/// fully/partially qualified `extends \Illuminate\Console\Command`.
///
/// This is a deliberately permissive heuristic: any `Command`-suffixed base
/// class counts (e.g. a project's own `BaseCommand` intermediate is itself
/// resolved through its own `extends`). The command index relies on the
/// presence of a `$signature` to confirm — a class extending some unrelated
/// `Command` but carrying no signature is dropped by [`extract_command_signature`].
pub fn extends_console_command(content: &str) -> bool {
    lazy_static! {
        static ref EXTENDS_COMMAND_RE: Regex =
            Regex::new(r"\bextends\s+\\?(?:[A-Za-z_][A-Za-z0-9_]*\\)*[A-Za-z0-9_]*Command\b")
                .unwrap();
    }
    EXTENDS_COMMAND_RE.is_match(content)
}

/// Extract the Artisan command signature from a `Command` subclass's source.
///
/// Returns `None` — gracefully, never panicking (AC: edge cases) — when:
/// - the class does not extend a `Command` base (not an Artisan command);
/// - there is no `protected $signature = '...'` literal (missing signature, or
///   the signature is built dynamically / from a constant we can't read
///   statically); or
/// - the literal resolves to an empty command name (malformed signature).
///
/// The signature is matched as a single-line string literal in either quote
/// style. Heredoc/nowdoc and interpolated signatures are intentionally skipped
/// rather than guessed — better to offer no navigation than a wrong target.
pub fn extract_command_signature(content: &str) -> Option<CommandSignature> {
    if !extends_console_command(content) {
        return None;
    }

    lazy_static! {
        // `protected $signature = '...';` — single- or double-quoted, matched
        // as separate alternatives because the `regex` crate has no
        // backreferences to pin the closing quote to the opening one. The inner
        // content is group 1 (single quotes) or group 2 (double quotes). Accept
        // any visibility and an optional `static` for robustness against
        // `public`/`protected static` shapes.
        static ref SIGNATURE_RE: Regex = Regex::new(
            r#"(?:public|protected|private)\s+(?:static\s+)?\$signature\s*=\s*(?:'([^'\n]*)'|"([^"\n]*)")"#,
        )
        .unwrap();
    }

    let caps = SIGNATURE_RE.captures(content)?;
    let content_match = caps.get(1).or_else(|| caps.get(2))?;
    let raw_signature = content_match.as_str().to_string();

    let name = command_name_from_signature(&raw_signature);
    if name.is_empty() {
        return None;
    }
    let name = name.to_string();

    let byte_offset = content_match.start();
    let mut line = 0u32;
    let mut last_line_start = 0usize;
    for (i, ch) in content[..byte_offset].char_indices() {
        if ch == '\n' {
            line += 1;
            last_line_start = i + 1;
        }
    }
    let start_column = (byte_offset - last_line_start) as u32;
    let end_column = start_column + raw_signature.len() as u32;

    Some(CommandSignature {
        name,
        raw_signature,
        line,
        start_column,
        end_column,
    })
}

#[cfg(test)]
mod tests;
