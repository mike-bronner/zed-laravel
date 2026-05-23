//! Resolve dotted Laravel config keys to their source-text values.
//!
//! `resolve_value(root, "app.name")` reads `config/app.php`, finds the
//! `'name' => ...` entry in the returned array, and returns the source text
//! of the value (e.g. `"env('APP_NAME', 'Laravel')"`).
//!
//! Nested keys recurse into nested array literals — `"database.connections.mysql.host"`
//! walks through three levels of nesting. The resolver is deliberately
//! conservative: when the path leads through something other than an array
//! literal (a function-call result, a constant, an object), it returns `None`
//! and the caller falls back to a less-specific hover.
//!
//! Pure parsing — no I/O outside the initial `read_to_string`. Easy to unit-test
//! with synthetic PHP source.

use std::path::Path;

/// Resolve a dotted Laravel config key (`"app.name"`) against a project root.
/// Returns the source text of the resolved value, trimmed of surrounding
/// whitespace. `None` when the file or key is missing.
pub fn resolve_value(root: &Path, dotted_key: &str) -> Option<String> {
    let mut parts = dotted_key.split('.');
    let file = parts.next()?;
    let key_path: Vec<&str> = parts.collect();

    let config_path = root.join("config").join(format!("{}.php", file));
    let content = std::fs::read_to_string(&config_path).ok()?;
    resolve_in_source(&content, &key_path)
}

/// Source-only variant for unit tests — operates on a string rather than
/// reading from disk.
pub fn resolve_in_source(source: &str, key_path: &[&str]) -> Option<String> {
    let bytes = source.as_bytes();
    let array_open = find_return_array_open(bytes)?;
    let raw = walk_path(bytes, array_open, key_path)?;
    Some(raw.trim().to_string())
}

/// Locate the opening `[` of the `return [...];` literal at the top of a
/// Laravel config file. Skips strings, line comments, and block comments so
/// stray `return`s in docblocks or in strings don't confuse the search.
fn find_return_array_open(bytes: &[u8]) -> Option<usize> {
    let mut i = 0usize;
    let mut in_string: Option<u8> = None;
    while i < bytes.len() {
        if let Some(quote) = in_string {
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if bytes[i] == quote {
                in_string = None;
            }
            i += 1;
            continue;
        }
        // Comments
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = i.saturating_add(2);
            continue;
        }
        // Strings
        if bytes[i] == b'\'' || bytes[i] == b'"' {
            in_string = Some(bytes[i]);
            i += 1;
            continue;
        }
        // Match `return` with word boundaries.
        if i + 6 <= bytes.len() && &bytes[i..i + 6] == b"return" {
            let before_ok = i == 0 || !is_identifier_byte(bytes[i - 1]);
            let after_ok = i + 6 >= bytes.len() || !is_identifier_byte(bytes[i + 6]);
            if before_ok && after_ok {
                let mut j = i + 6;
                while j < bytes.len() {
                    let b = bytes[j];
                    if b == b'[' {
                        return Some(j + 1);
                    }
                    if b == b';' {
                        return None;
                    }
                    j += 1;
                }
                return None;
            }
        }
        i += 1;
    }
    None
}

/// Walk down the key path within the array starting at byte `array_open`
/// (which points to the first byte AFTER the opening `[`). Returns the raw
/// source bytes of the matched value, untrimmed.
fn walk_path(bytes: &[u8], array_open: usize, path: &[&str]) -> Option<String> {
    if path.is_empty() {
        return None;
    }
    let head = path[0];
    let tail = &path[1..];
    let mut i = array_open;

    while i < bytes.len() {
        skip_ws_and_comments(bytes, &mut i);
        if i >= bytes.len() || bytes[i] == b']' {
            return None;
        }

        let (key, after_key) = read_string_key(bytes, i)?;
        i = after_key;
        skip_ws_and_comments(bytes, &mut i);

        if i + 2 > bytes.len() || &bytes[i..i + 2] != b"=>" {
            return None;
        }
        i += 2;
        skip_ws_and_comments(bytes, &mut i);

        let value_start = i;
        let value_end = read_value_end(bytes, i);

        if key == head {
            if tail.is_empty() {
                let s = std::str::from_utf8(&bytes[value_start..value_end]).ok()?;
                return Some(s.to_string());
            }
            // Recurse into a nested array literal.
            let mut j = value_start;
            skip_ws_and_comments(bytes, &mut j);
            if j < bytes.len() && bytes[j] == b'[' {
                return walk_path(bytes, j + 1, tail);
            }
            return None;
        }

        i = value_end;
        if i < bytes.len() && bytes[i] == b',' {
            i += 1;
        }
    }
    None
}

/// Skip whitespace, `//` line comments, `/* */` block comments, and `#` shell
/// comments. Advances `i` until the next significant byte.
fn skip_ws_and_comments(bytes: &[u8], i: &mut usize) {
    loop {
        while *i < bytes.len() && bytes[*i].is_ascii_whitespace() {
            *i += 1;
        }
        if *i + 1 < bytes.len() && bytes[*i] == b'/' && bytes[*i + 1] == b'/' {
            while *i < bytes.len() && bytes[*i] != b'\n' {
                *i += 1;
            }
            continue;
        }
        if *i + 1 < bytes.len() && bytes[*i] == b'/' && bytes[*i + 1] == b'*' {
            *i += 2;
            while *i + 1 < bytes.len() && !(bytes[*i] == b'*' && bytes[*i + 1] == b'/') {
                *i += 1;
            }
            *i = i.saturating_add(2);
            continue;
        }
        if *i < bytes.len() && bytes[*i] == b'#' {
            while *i < bytes.len() && bytes[*i] != b'\n' {
                *i += 1;
            }
            continue;
        }
        break;
    }
}

/// Parse a quoted string key at `start`. Returns the unescaped key and the
/// byte offset just past the closing quote. `None` for non-string keys
/// (integers, constants) — those aren't supported for hover lookup yet.
fn read_string_key(bytes: &[u8], start: usize) -> Option<(String, usize)> {
    if start >= bytes.len() {
        return None;
    }
    let quote = bytes[start];
    if quote != b'\'' && quote != b'"' {
        return None;
    }
    let mut i = start + 1;
    let mut out = String::new();
    while i < bytes.len() && bytes[i] != quote {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            out.push(bytes[i + 1] as char);
            i += 2;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    Some((out, i + 1))
}

/// Find the byte offset where the current value ends — at the next `,` or `]`
/// at depth 0, skipping over nested parens/brackets/braces and string literals.
fn read_value_end(bytes: &[u8], start: usize) -> usize {
    let mut i = start;
    let mut depth = 0i32;
    let mut in_string: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(quote) = in_string {
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if b == quote {
                in_string = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' | b'"' => in_string = Some(b),
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b'}' => depth -= 1,
            b']' => {
                if depth == 0 {
                    return i;
                }
                depth -= 1;
            }
            b',' if depth == 0 => return i,
            _ => {}
        }
        i += 1;
    }
    i
}

fn is_identifier_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests;
