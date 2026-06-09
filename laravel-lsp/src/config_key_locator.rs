//! Locate the source-text position of a Laravel config-array key via
//! tree-sitter — column-accurate, suitable for building a rename
//! `WorkspaceEdit`.
//!
//! The companion to `config_lookup.rs`: that module resolves the *value* for
//! a dotted config key (`"app.name"` → `"env('APP_NAME', 'Laravel')"`),
//! whereas this one finds the (line, start_column, end_column) of the *key*
//! string itself so the rename machinery can rewrite it in place.
//!
//! Walks the PHP AST rather than scanning bytes:
//!
//! 1. Find the top-level `return [...];` array literal (skipping any leading
//!    `<?php`, declarations, or comments — tree-sitter handles that for us).
//! 2. Descend into nested `array_creation_expression`s following the dotted
//!    path. `"database.connections.mysql.host"` walks `database` → array →
//!    `connections` → array → `mysql` → array → `host`.
//! 3. Match keys by their `string_content` text, ignoring quote style.
//! 4. When the path reaches the leaf, return that key string's content
//!    position.
//!
//! The walker is conservative: anything that's not a literal `string =>
//! value` array entry (e.g. dynamic keys built from `env(...)`, spread
//! syntax, numeric keys) is skipped silently. The rename operation simply
//! produces fewer `TextEdit`s — never an incorrect edit.

use std::path::Path;
use tree_sitter::Node;

/// Position of a config-key string literal's *content* (no quotes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyPosition {
    pub line: u32,
    pub start_column: u32,
    pub end_column: u32,
}

/// Locate the source position of `dotted_key` (e.g. `"app.name"`,
/// `"database.connections.mysql.host"`) under a Laravel project root.
/// Returns `None` if the file or any path segment is missing.
pub fn locate_key(root: &Path, dotted_key: &str) -> Option<KeyPosition> {
    let mut parts = dotted_key.split('.');
    let file = parts.next()?;
    let path_segments: Vec<&str> = parts.collect();
    if path_segments.is_empty() {
        return None;
    }

    let config_path = root.join("config").join(format!("{file}.php"));
    let content = std::fs::read_to_string(&config_path).ok()?;
    locate_in_source(&content, &path_segments)
}

/// Source-only variant for unit tests — operates on a string rather than
/// reading from disk.
pub fn locate_in_source(source: &str, key_path: &[&str]) -> Option<KeyPosition> {
    if key_path.is_empty() {
        return None;
    }
    let tree = crate::parser::parse_php(source).ok()?;
    let bytes = source.as_bytes();
    let array_node = find_return_array(tree.root_node())?;
    locate_at_path(array_node, bytes, key_path)
}

/// Enumerate every string-keyed entry in a config/lang `return [...]` array,
/// in document order, as `(in-file dotted path, key position)`. Both leaf and
/// intermediate keys are emitted (`database.connections` AND
/// `database.connections.mysql.host`), since each is a referenceable
/// `config()`/`__()` key. Non-string keys (numeric list entries, dynamic keys)
/// are skipped. The caller prepends the file stem to form the full dotted key
/// (`database.` / `auth.`). Powers config + translation code lenses (#59).
pub fn enumerate_keys_in_source(source: &str) -> Vec<(String, KeyPosition)> {
    let Ok(tree) = crate::parser::parse_php(source) else {
        return Vec::new();
    };
    let bytes = source.as_bytes();
    let Some(array) = find_return_array(tree.root_node()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    collect_keys(array, bytes, "", &mut out);
    out
}

/// Recurse an array literal, accumulating the dotted key path. `prefix` is the
/// path to `array` (empty at the top level).
fn collect_keys(array: Node, source: &[u8], prefix: &str, out: &mut Vec<(String, KeyPosition)>) {
    let mut cursor = array.walk();
    for child in array.children(&mut cursor) {
        if child.kind() != "array_element_initializer" {
            continue;
        }
        let Some(entry) = parse_array_entry(child, source) else {
            continue;
        };
        let dotted = if prefix.is_empty() {
            entry.key_text.clone()
        } else {
            format!("{prefix}.{}", entry.key_text)
        };
        out.push((dotted.clone(), entry.key_position));
        if let Some(nested) = find_array_in_expression(entry.value_node) {
            collect_keys(nested, source, &dotted, out);
        }
    }
}

/// Walk the AST looking for the top-level `return <array>;` statement and
/// return the array literal node. We scan the file root's children so the
/// usual `<?php` opener, `use` statements, etc. don't confuse us.
fn find_return_array(root: Node) -> Option<Node> {
    // tree-sitter-php wraps PHP at the file root in `program > php_tag …
    // statements`. Walk every descendant looking for a `return_statement`
    // whose expression is `array_creation_expression`. This handles both
    // top-level and namespaced files uniformly.
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "return_statement" {
            // The expression is typically a direct child, but tree-sitter
            // can wrap it in `expression`/`primary_expression` indirections.
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(arr) = find_array_in_expression(child) {
                    return Some(arr);
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    None
}

/// Recurse through `expression` / `primary_expression` wrappers to reach
/// the underlying `array_creation_expression`, if present.
fn find_array_in_expression(node: Node) -> Option<Node> {
    if node.kind() == "array_creation_expression" {
        return Some(node);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(arr) = find_array_in_expression(child) {
            return Some(arr);
        }
    }
    None
}

/// Walk an `array_creation_expression` along the given path. At each step,
/// find the entry whose key string content matches the path segment. If it's
/// the last segment, return the key position; otherwise descend into the
/// value (which must itself be an array) and recurse.
fn locate_at_path<'a>(array: Node<'a>, source: &[u8], path: &[&str]) -> Option<KeyPosition> {
    let (head, tail) = path.split_first()?;

    let mut cursor = array.walk();
    for child in array.children(&mut cursor) {
        if child.kind() != "array_element_initializer" {
            continue;
        }
        let entry = parse_array_entry(child, source)?;
        if entry.key_text != *head {
            // Try the next entry; we only act on a key match.
            continue;
        }
        if tail.is_empty() {
            return Some(entry.key_position);
        }
        // Descend into the value, which must be another array.
        if let Some(nested) = find_array_in_expression(entry.value_node) {
            return locate_at_path(nested, source, tail);
        }
        // Path expects more nesting but the value isn't an array.
        return None;
    }
    None
}

struct ParsedEntry<'a> {
    key_text: String,
    key_position: KeyPosition,
    value_node: Node<'a>,
}

/// Parse one `array_element_initializer` of the shape `key => value`.
/// Returns `None` when the key isn't a literal string (numeric keys,
/// expressions, etc. — we can't rename those positionally).
fn parse_array_entry<'a>(node: Node<'a>, source: &[u8]) -> Option<ParsedEntry<'a>> {
    // Layout (tree-sitter-php): the element has children separated by `=>`,
    // tagged via field names `key` and `value`. Some grammar revisions use
    // unnamed positional children — handle both.
    let key_node = node
        .child_by_field_name("key")
        .or_else(|| named_child(node, 0))?;
    let value_node = node
        .child_by_field_name("value")
        .or_else(|| named_child(node, 1))?;

    let (key_text, key_position) = string_literal_content(key_node, source)?;
    Some(ParsedEntry {
        key_text,
        key_position,
        value_node,
    })
}

/// Return the n-th named child of a node, ignoring whitespace and `=>`
/// punctuation. Fallback used when `child_by_field_name` isn't supported
/// for the current grammar revision.
fn named_child(node: Node, index: usize) -> Option<Node> {
    let mut cursor = node.walk();
    let result = node.named_children(&mut cursor).nth(index);
    result
}

/// Extract the literal string content + position from a `string` /
/// `encapsed_string` node. Returns `None` for non-string keys.
fn string_literal_content<'a>(node: Node<'a>, source: &[u8]) -> Option<(String, KeyPosition)> {
    if node.kind() != "string" && node.kind() != "encapsed_string" {
        return None;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "string_content" {
            let text = child.utf8_text(source).ok()?.to_string();
            let start = child.start_position();
            let end = child.end_position();
            return Some((
                text,
                KeyPosition {
                    line: start.row as u32,
                    start_column: start.column as u32,
                    end_column: if end.row == start.row {
                        end.column as u32
                    } else {
                        start.column as u32
                    },
                },
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests;
