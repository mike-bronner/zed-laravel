//! Extract `@props([...])` declarations from Blade view source.
//!
//! Hover uses this to surface the variables a view expects. The extractor
//! is paren-balanced and string-aware so multi-line `@props([...])` blocks
//! with embedded parentheses inside string keys/defaults are captured
//! intact — for example:
//!
//! ```text
//! @props([
//!     'note' => 'something (with parens)',
//!     'count' => 0,
//! ])
//! ```
//!
//! …returns the full multi-line declaration without truncation at the first
//! `)` it sees.

use std::path::Path;

/// Read a Blade file and extract its first `@props([...])` declaration.
/// Returns `None` when the file doesn't exist, can't be read, or has no
/// `@props(...)` directive.
///
/// The returned string starts with `@props(` and ends with the matching
/// `)` — multi-line declarations are preserved verbatim so the hover code
/// block reads the same as the source.
pub fn extract_props_directive(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    extract_props_directive_from_source(&content)
}

/// Source-only variant used for unit tests — operates on an in-memory
/// string rather than reading from disk.
pub fn extract_props_directive_from_source(content: &str) -> Option<String> {
    let bytes = content.as_bytes();
    let needle = b"@props";
    let mut i = 0;
    while i + needle.len() < bytes.len() {
        if &bytes[i..i + needle.len()] != needle {
            i += 1;
            continue;
        }
        // Word boundary after `@props` — reject `@propsExtended` or similar.
        let after = i + needle.len();
        let mut j = after;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'(' {
            i = after;
            continue;
        }
        // Paren-balance to find the matching `)`, tracking string literals
        // so embedded `(` / `)` characters don't throw off the count.
        let mut depth = 1i32;
        let mut k = j + 1;
        let mut in_string: Option<u8> = None;
        while k < bytes.len() {
            let b = bytes[k];
            if let Some(q) = in_string {
                if b == b'\\' && k + 1 < bytes.len() {
                    k += 2;
                    continue;
                }
                if b == q {
                    in_string = None;
                }
                k += 1;
                continue;
            }
            match b {
                b'\'' | b'"' => in_string = Some(b),
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(content[i..=k].to_string());
                    }
                }
                _ => {}
            }
            k += 1;
        }
        return None;
    }
    None
}

#[cfg(test)]
mod tests;
