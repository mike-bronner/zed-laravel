//! Slot-aware navigation for `<x-slot:name>` Blade tags.
//!
//! `<x-slot:title>` isn't a component. It's named-slot syntax used inside a
//! parent `<x-component>` to populate that component's `$title` variable.
//! Go-to-definition on a slot tag should jump to the parent component's view
//! file — ideally to the line that references `{{ $title }}`.
//!
//! The flow:
//!   1. `find_slot_at_position` checks whether the cursor sits on `<x-slot:NAME>`.
//!   2. `find_enclosing_parent_component` walks the Blade AST upward from the
//!      slot tag to the nearest enclosing `<x-COMP>` (non-slot) element.
//!   3. `find_slot_variable_line` scans the resolved parent template for the
//!      line containing `{{ $NAME }}` to pin navigation to the right location.

use std::path::Path;
use tree_sitter::{Node, Tree};

use crate::parser::parse_blade;

/// Information about a slot tag found at the cursor position.
#[derive(Debug, Clone, PartialEq)]
pub struct SlotInfo {
    /// The slot name after `x-slot:` (e.g., `title` from `<x-slot:title>`).
    pub name: String,
    /// Byte offset of the slot tag's opening `<` in source.
    pub byte_start: usize,
    /// Byte offset just past the slot tag's closing `>`.
    pub byte_end: usize,
}

/// Information about a parent component element wrapping a slot tag.
#[derive(Debug, Clone, PartialEq)]
pub struct ParentComponentInfo {
    /// The component name (the part after `x-`, e.g., `card` from `<x-card>`).
    pub name: String,
}

/// Find the slot tag at a given (line, column) cursor position in Blade source.
///
/// Recognizes all three slot syntaxes:
/// - `<x-slot:NAME>` — colon form (modern)
/// - `<x-slot name="NAME">` — attribute form (legacy)
/// - `<x-slot>` — bare default slot (resolves to `$slot` in parent)
///
/// Plus their closing/self-closing variants. Returns the slot info if the
/// cursor sits anywhere inside the tag — `<`, `>`, the slot name, the
/// attribute area, etc. Returns None when the cursor isn't on a slot tag.
///
/// Implementation uses a line-based scan because tree-sitter's
/// `descendant_for_byte_range` can land on punctuation nodes that aren't
/// ancestors of the `tag_name` capture, making AST-walk detection fragile at
/// tag boundaries. Lines aren't a problem here — slot tags don't wrap.
pub fn find_slot_at_position(source: &str, line: u32, character: u32) -> Option<SlotInfo> {
    let line_content = source.lines().nth(line as usize)?;
    let cursor_col = character as usize;

    for prefix in &["<x-slot", "</x-slot"] {
        let mut search_from = 0;
        while let Some(rel) = line_content[search_from..].find(prefix) {
            let tag_start = search_from + rel;
            let after_prefix = tag_start + prefix.len();

            // The character right after `<x-slot` (or `</x-slot`) determines
            // whether this is really a slot tag — it must be `:`, `>`, `/`,
            // whitespace, or end-of-line. Anything else means we matched a
            // prefix of a longer tag name (e.g., `<x-slotmachine>`).
            let next_char = line_content[after_prefix..].chars().next();
            let is_slot_tag = matches!(next_char, Some(':') | Some('>') | Some('/') | None)
                || next_char.is_some_and(|c| c.is_whitespace());

            if !is_slot_tag {
                search_from = after_prefix;
                continue;
            }

            // Extract the slot name from whichever form is present.
            let (slot_name, name_search_end) = if next_char == Some(':') {
                // <x-slot:NAME> form
                let name_start = after_prefix + 1;
                let rest = &line_content[name_start..];
                let name_len = rest
                    .find(|c: char| !is_slot_name_char(c))
                    .unwrap_or(rest.len());
                if name_len == 0 {
                    // `<x-slot:>` with empty name — not navigable.
                    search_from = name_start;
                    continue;
                }
                (rest[..name_len].to_string(), name_start + name_len)
            } else {
                (String::new(), after_prefix)
            };

            // Bound the tag by its closing `>` on this line — fall back to
            // end of line if absent (truncated source).
            let close_rel = line_content[name_search_end..]
                .find('>')
                .map(|p| p + 1)
                .unwrap_or(line_content.len() - name_search_end);
            let tag_end = name_search_end + close_rel;

            // For the attribute-form (no colon), look for `name="X"` inside
            // the tag body. Bare `<x-slot>` falls back to "slot" — that's the
            // default-slot variable name in Laravel components.
            let final_name = if slot_name.is_empty() {
                let tag_inner = &line_content[after_prefix..tag_end];
                extract_name_attribute(tag_inner)
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "slot".to_string())
            } else {
                slot_name
            };

            if cursor_col >= tag_start && cursor_col <= tag_end {
                let line_byte_start = line_byte_offset(source, line as usize)?;
                let byte_start = line_byte_start + tag_start;
                let byte_end = line_byte_start + tag_end;
                return Some(SlotInfo {
                    name: final_name,
                    byte_start,
                    byte_end,
                });
            }

            search_from = tag_end;
        }
    }

    None
}

/// Extract the value of a `name="X"` (or `name='X'`) attribute from a tag's
/// inner text. Returns None when the attribute isn't present or is malformed.
fn extract_name_attribute(tag_inner: &str) -> Option<&str> {
    let mut search_from = 0;
    while let Some(rel) = tag_inner[search_from..].find("name") {
        let pos = search_from + rel;

        // Reject substrings like `wirename=` — the char before must be a
        // boundary (whitespace or the start of the inner text).
        let before_ok = pos == 0 || tag_inner.as_bytes()[pos - 1].is_ascii_whitespace();
        if !before_ok {
            search_from = pos + 4;
            continue;
        }

        let after_name = tag_inner[pos + 4..].trim_start();
        let Some(rest) = after_name.strip_prefix('=') else {
            search_from = pos + 4;
            continue;
        };
        let rest = rest.trim_start();
        let quote = rest.chars().next()?;
        if quote != '"' && quote != '\'' {
            search_from = pos + 4;
            continue;
        }
        let body = &rest[1..];
        let end = body.find(quote)?;
        return Some(&body[..end]);
    }
    None
}

fn is_slot_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'
}

fn line_byte_offset(source: &str, line: usize) -> Option<usize> {
    if line == 0 {
        return Some(0);
    }
    let mut count = 0usize;
    let mut byte = 0usize;
    for (i, ch) in source.char_indices() {
        if ch == '\n' {
            count += 1;
            if count == line {
                return Some(i + 1);
            }
        }
        byte = i;
    }
    let _ = byte;
    None
}

/// Walk the Blade AST upward from a slot tag's byte range to find the nearest
/// enclosing `<x-COMPONENT>` element that **isn't** a slot. Returns None when
/// the slot tag is at the top level (no parent component wraps it).
pub fn find_enclosing_parent_component(
    source: &str,
    slot_byte_start: usize,
) -> Option<ParentComponentInfo> {
    let tree = parse_blade(source).ok()?;
    find_enclosing_parent_component_in_tree(&tree, source, slot_byte_start)
}

fn find_enclosing_parent_component_in_tree(
    tree: &Tree,
    source: &str,
    slot_byte_start: usize,
) -> Option<ParentComponentInfo> {
    let root = tree.root_node();
    let mut node = root.descendant_for_byte_range(slot_byte_start, slot_byte_start)?;

    loop {
        let parent = node.parent()?;
        if parent.kind() == "element" {
            if let Some(tag_text) = element_opening_tag_name(parent, source) {
                let is_slot = tag_text == "x-slot" || tag_text.starts_with("x-slot:");
                let is_component = tag_text.starts_with("x-");
                if is_component
                    && !is_slot
                    && parent.start_byte() < slot_byte_start
                    && parent.end_byte() > slot_byte_start
                {
                    let name = tag_text.strip_prefix("x-").unwrap_or(tag_text);
                    return Some(ParentComponentInfo {
                        name: name.to_string(),
                    });
                }
            }
        }
        node = parent;
    }
}

/// Find the line + column in `view_source` where `{{ $slot_name }}` appears.
///
/// Searches for `$slot_name` preceded by `{` or `$` boundaries to avoid
/// catching `$slotName` substrings. Used to pin slot go-to-def at the exact
/// usage in the parent template instead of just opening it at line 0.
///
/// Returns `Some((line, col))` (both 0-based) on the first match, or None.
pub fn find_slot_variable_line(view_source: &str, slot_name: &str) -> Option<(u32, u32)> {
    let needle = format!("${}", slot_name);

    for (line_idx, line) in view_source.lines().enumerate() {
        let mut search_from = 0;
        while let Some(rel) = line[search_from..].find(&needle) {
            let pos = search_from + rel;
            // Ensure the next character isn't an identifier continuation
            // (so `$slot` doesn't match `$slotName`).
            let after = line[pos + needle.len()..].chars().next();
            let is_word_boundary = match after {
                Some(c) => !is_php_ident_continuation(c),
                None => true,
            };
            if is_word_boundary {
                return Some((line_idx as u32, pos as u32));
            }
            search_from = pos + needle.len();
        }
    }

    None
}

/// Read a Blade template file and search it for the slot variable usage.
///
/// Convenience wrapper that combines file reading with the line search. Returns
/// None when the file can't be read or no `{{ $slot_name }}` reference exists.
pub fn locate_slot_in_view(view_path: &Path, slot_name: &str) -> Option<(u32, u32)> {
    let source = std::fs::read_to_string(view_path).ok()?;
    find_slot_variable_line(&source, slot_name)
}

// ============================================================================
// Internal helpers
// ============================================================================

/// Given an `element` node, extract the text of its opening tag's `tag_name`.
fn element_opening_tag_name<'a>(element: Node, source: &'a str) -> Option<&'a str> {
    let mut cursor = element.walk();
    for child in element.children(&mut cursor) {
        if child.kind() == "start_tag" || child.kind() == "self_closing_tag" {
            let mut inner = child.walk();
            for inner_child in child.children(&mut inner) {
                if inner_child.kind() == "tag_name" {
                    return inner_child.utf8_text(source.as_bytes()).ok();
                }
            }
        }
    }
    None
}

fn is_php_ident_continuation(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests;
