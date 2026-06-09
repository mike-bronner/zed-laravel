//! Locate Artisan command-string call sites in PHP source.
//!
//! The call-site half of issue #62. Where [`crate::command_signature`] reads a
//! `Command` class to learn *what command it provides*, this module scans
//! ordinary PHP for the places that *reference* a command by name, so goto-
//! definition and hover know there's something to resolve under the cursor.
//!
//! ## Supported call sites (AC: call site coverage)
//!
//! | Pattern | Example |
//! |---------|---------|
//! | Direct dispatch | `Artisan::call('emails:send', [...])` |
//! | Artisan queue   | `Artisan::queue('emails:send')` |
//! | Task scheduling | `->command('emails:send')->daily()` |
//! | Testing         | `->artisan('emails:send')` |
//!
//! Only the **first string argument** of each call is treated as the command
//! name — that's where Laravel expects it across all four shapes. Arguments and
//! options are passed separately (an array, or fluent calls), so the string is
//! the bare command name (`emails:send`), occasionally with inline options
//! (`emails:send --force`); [`command_name`](CommandCallSite::command_name)
//! returns the resolvable leading token either way.
//!
//! ## Position convention
//!
//! 0-based throughout; `start_column`/`end_column` bracket the string *content*
//! (between the quotes), matching the rest of the stack — see `CLAUDE.md`.

use lazy_static::lazy_static;
use regex::Regex;

use crate::command_signature::command_name_from_signature;

/// One Artisan command-string reference found in source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandCallSite {
    /// The raw string content as written (no surrounding quotes), e.g.
    /// `emails:send` or `emails:send --force`.
    pub raw: String,
    /// 0-based row of the string content.
    pub line: u32,
    /// 0-based column of the first content character (after the opening quote).
    pub start_column: u32,
    /// 0-based column one past the last content character (before the closing
    /// quote).
    pub end_column: u32,
}

impl CommandCallSite {
    /// The resolvable command name: the leading token of [`raw`](Self::raw),
    /// before any inline options. Matches how a `$signature` resolves to a
    /// command name, so a call site and its declaration agree on the key.
    pub fn command_name(&self) -> &str {
        command_name_from_signature(&self.raw)
    }

    /// Whether a cursor at (`line`, `character`) — both 0-based — falls within
    /// this call site's string content. The end is inclusive of the position
    /// one past the last character so a cursor resting just before the closing
    /// quote still resolves.
    pub fn contains(&self, line: u32, character: u32) -> bool {
        self.line == line && character >= self.start_column && character <= self.end_column
    }
}

/// Every Artisan command-string call site in `content`, in source order.
pub fn extract_command_call_sites(content: &str) -> Vec<CommandCallSite> {
    lazy_static! {
        // Method/static-call prefix, then the first string argument. Single- and
        // double-quoted alternatives are spelled out separately because the
        // `regex` crate has no backreferences (see command_signature).
        static ref CALL_SITE_RE: Regex = Regex::new(
            r#"(?:Artisan::call|Artisan::queue|->command|->artisan)\s*\(\s*(?:'([^'\n]*)'|"([^"\n]*)")"#,
        )
        .unwrap();
    }

    let mut out = Vec::new();
    for caps in CALL_SITE_RE.captures_iter(content) {
        let Some(content_match) = caps.get(1).or_else(|| caps.get(2)) else {
            continue;
        };
        let raw = content_match.as_str().to_string();
        // A blank command string has nothing to resolve — skip it rather than
        // emitting a site that can never match an index entry.
        if command_name_from_signature(&raw).is_empty() {
            continue;
        }

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
        let end_column = start_column + raw.len() as u32;

        out.push(CommandCallSite {
            raw,
            line,
            start_column,
            end_column,
        });
    }
    out
}

/// The command-string call site whose content contains the cursor at
/// (`line`, `character`) — both 0-based — or `None` if the cursor isn't on one.
///
/// This is the entry point goto-definition and hover call: resolve the cursor
/// to a command name, then look it up in the command index.
pub fn command_call_at_position(
    content: &str,
    line: u32,
    character: u32,
) -> Option<CommandCallSite> {
    extract_command_call_sites(content)
        .into_iter()
        .find(|site| site.contains(line, character))
}

#[cfg(test)]
mod tests;
