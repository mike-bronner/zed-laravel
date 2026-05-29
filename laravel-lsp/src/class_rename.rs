//! PHP class rename — locate every reference to a class FQCN within a file and
//! rewrite it, so the LSP can rename an Eloquent model (or any project class)
//! across the whole project: declaration, `use` imports, `User::` static calls,
//! `new User`, type hints, `User::class`, `extends`/`implements`, `instanceof`,
//! and `@var`/`@param`/`@return` docblocks.
//!
//! # How references are resolved
//!
//! For a target FQCN (`App\Models\User`, basename `User`) we walk the tree for
//! tokens in **class-name positions** (an allowlist of tree-sitter contexts),
//! resolve each token to an FQCN using the file's `use` aliases + namespace,
//! and rewrite the basename segment **iff** it both resolves to the target AND
//! its last segment equals the old basename. That two-part test is what makes
//! aliases safe:
//!
//! - `use App\Models\User as U; U::find()` — the import's last segment `User`
//!   is rewritten; the alias `U` (basename ≠ `User`) is left alone, so it keeps
//!   pointing at the renamed class.
//! - A method/property/function coincidentally named `User` sits in a context
//!   that isn't on the allowlist, so it's never touched.
//!
//! Docblocks live in comments (tree-sitter doesn't descend them), so `@var` /
//! `@param` / `@return` references are matched with a targeted scan.

use crate::parser::parse_php;
use crate::query_chain::use_aliases::{extract_use_aliases, resolve_class_name, UseAliases};
use std::path::{Path, PathBuf};
use tree_sitter::Node;
use walkdir::WalkDir;

/// Directory names pruned when enumerating project PHP files for rename — we
/// never rewrite dependencies or build artefacts.
const SKIP_DIRS: &[&str] = &["vendor", "node_modules", ".git", "storage"];

/// Every project (non-dependency) `*.php` file under `root`. Used to find class
/// references for a project-wide rename. `vendor/`, `node_modules/`, `.git/`,
/// and `storage/` are pruned.
pub fn project_php_files(root: &Path) -> Vec<PathBuf> {
    WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| {
            !(entry.file_type().is_dir()
                && entry
                    .file_name()
                    .to_str()
                    .map(|n| SKIP_DIRS.contains(&n))
                    .unwrap_or(false))
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file() && e.path().extension().is_some_and(|x| x == "php"))
        .map(|e| e.path().to_path_buf())
        .collect()
}

/// Whether `path` is inside a pruned dependency dir (e.g. `vendor/`). We refuse
/// to rename a class whose declaration lives there.
pub fn is_dependency_path(path: &Path) -> bool {
    path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .map(|s| SKIP_DIRS.contains(&s))
            .unwrap_or(false)
    })
}

/// A resolved class-name occurrence: the FQCN it refers to and the byte span of
/// the **basename segment** to rewrite (just `User` in `App\Models\User`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct ClassRef {
    fqcn: String,
    basename: String,
    span: (usize, usize),
}

/// All byte spans in `content` that should be rewritten to the new basename
/// when renaming `target_fqcn` (whose current basename is `old_basename`).
/// Spans are sorted and de-duplicated; each covers exactly the old basename.
pub fn reference_spans(
    content: &str,
    target_fqcn: &str,
    old_basename: &str,
) -> Vec<(usize, usize)> {
    let Ok(tree) = parse_php(content) else {
        return Vec::new();
    };
    let bytes = content.as_bytes();
    let aliases = extract_use_aliases(&tree, content);
    let namespace = namespace_in(&tree, bytes);

    let mut spans: Vec<(usize, usize)> = refs_in(&tree, bytes, &aliases, namespace.as_deref())
        .into_iter()
        .filter(|r| r.fqcn == target_fqcn && r.basename == old_basename)
        .map(|r| r.span)
        .collect();
    spans.extend(docblock_spans_in(
        &tree,
        bytes,
        &aliases,
        namespace.as_deref(),
        target_fqcn,
        old_basename,
    ));
    spans.sort_unstable();
    spans.dedup();
    spans
}

/// If `byte_offset` falls on a class-name token, return its resolved FQCN and
/// the basename span. Returns `None` for aliases (cursor token ≠ basename) and
/// for non-class positions — so the caller only offers rename on real class
/// names.
pub fn class_at_cursor(content: &str, byte_offset: usize) -> Option<(String, (usize, usize))> {
    let tree = parse_php(content).ok()?;
    let bytes = content.as_bytes();
    let aliases = extract_use_aliases(&tree, content);
    let namespace = namespace_in(&tree, bytes);
    refs_in(&tree, bytes, &aliases, namespace.as_deref())
        .into_iter()
        .filter(|r| byte_offset >= r.span.0 && byte_offset <= r.span.1)
        .find(|r| r.fqcn.rsplit('\\').next() == Some(r.basename.as_str()))
        .map(|r| (r.fqcn, r.span))
}

/// Walk an already-parsed tree and resolve every class-name occurrence to a
/// [`ClassRef`]. Parsing + alias/namespace extraction happen once in the caller
/// — important on large projects where rename scans thousands of files.
fn refs_in(
    tree: &tree_sitter::Tree,
    bytes: &[u8],
    aliases: &UseAliases,
    namespace: Option<&str>,
) -> Vec<ClassRef> {
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if (node.kind() == "name" || node.kind() == "qualified_name") && is_class_ref(node) {
            if let Some(r) = resolve_ref(node, bytes, aliases, namespace) {
                out.push(r);
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    out
}

/// Resolve a class-name node to `(fqcn, basename, basename_span)`.
fn resolve_ref(
    node: Node,
    bytes: &[u8],
    aliases: &UseAliases,
    namespace: Option<&str>,
) -> Option<ClassRef> {
    let text = node.utf8_text(bytes).ok()?;
    // Basename segment + its span (the part we rewrite). For qualified names the
    // basename is the trailing `\`-segment; identifiers are ASCII so byte
    // arithmetic off the node end is exact.
    let (basename, span) = if node.kind() == "qualified_name" {
        let bn = text.rsplit('\\').next().unwrap_or(text);
        let end = node.end_byte();
        (bn.to_string(), (end - bn.len(), end))
    } else {
        (text.to_string(), (node.start_byte(), node.end_byte()))
    };

    let resolved = resolve_class_name(text, aliases);
    let fqcn = if resolved.contains('\\') {
        resolved
    } else {
        match namespace {
            Some(ns) => format!("{ns}\\{resolved}"),
            None => resolved,
        }
    };

    Some(ClassRef {
        fqcn,
        basename,
        span,
    })
}

/// Whether a `name`/`qualified_name` node sits in a class-name position.
/// Allowlist of tree-sitter contexts — anything else (member access, method /
/// function names, the `class` of `::class`, namespace segments) is excluded.
fn is_class_ref(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        // Segment of a larger qualified/namespace name — handled at the
        // `qualified_name` level, not per segment.
        "namespace_name" | "qualified_name" => false,
        // Type hints (parameter / return / property), all wrap a `named_type`.
        "named_type" => true,
        // `new User`
        "object_creation_expression" => true,
        // `class X extends User`
        "base_clause" => true,
        // `class X implements User`
        "class_interface_clause" => true,
        // `use App\Models\User;` / `use ... as Alias;` — the basename filter
        // sorts the import (rewrite) from the alias (skip).
        "namespace_use_clause" => true,
        // The class being declared (`class User { … }`).
        "class_declaration" => is_field_child(parent, node, "name"),
        // `User::method()` — only the scope, not the method.
        "scoped_call_expression" => is_scope_child(parent, node),
        // `User::class` / `User::CONST` — only the class, not the constant.
        "class_constant_access_expression" => is_scope_child(parent, node),
        // `User::$prop`
        "scoped_property_access_expression" => is_scope_child(parent, node),
        // `$x instanceof User`
        "binary_expression" => is_instanceof_right(parent, node),
        _ => false,
    }
}

/// Whether `node` is the `field`-named child of `parent`.
fn is_field_child(parent: Node, node: Node, field: &str) -> bool {
    parent
        .child_by_field_name(field)
        .map(|c| c.id() == node.id())
        .unwrap_or(false)
}

/// Whether `node` is the class/scope side of a `::` access. Prefers the `scope`
/// field; falls back to "first named child" for grammars that don't field it.
fn is_scope_child(parent: Node, node: Node) -> bool {
    if let Some(scope) = parent.child_by_field_name("scope") {
        return scope.id() == node.id();
    }
    let mut cursor = parent.walk();
    let first = parent.named_children(&mut cursor).next();
    first.map(|c| c.id() == node.id()).unwrap_or(false)
}

/// Whether `parent` is an `X instanceof Class` expression and `node` is the
/// `Class` (right) operand.
fn is_instanceof_right(parent: Node, node: Node) -> bool {
    let mut cursor = parent.walk();
    let has_instanceof = parent
        .children(&mut cursor)
        .any(|c| c.kind() == "instanceof");
    if !has_instanceof {
        return false;
    }
    // The class is the right operand; `right` field if present, else the last
    // named child.
    if let Some(right) = parent.child_by_field_name("right") {
        return right.id() == node.id();
    }
    let mut cursor = parent.walk();
    let last = parent.named_children(&mut cursor).last();
    last.map(|c| c.id() == node.id()).unwrap_or(false)
}

/// The file's namespace (`namespace App\Models;`), if declared. Operates on an
/// already-parsed tree.
fn namespace_in(tree: &tree_sitter::Tree, bytes: &[u8]) -> Option<String> {
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "namespace_definition" {
            // The name child holds the namespace path.
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "namespace_name" {
                    return child.utf8_text(bytes).ok().map(str::to_string);
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

/// PHPDoc tags whose value starts with a type expression we should rewrite.
const DOC_TYPE_TAGS: &[&str] = &[
    "@param",
    "@return",
    "@var",
    "@property",
    "@property-read",
    "@property-write",
    "@throws",
];

/// Scan `/** … */` docblocks for class references in `@param`/`@return`/`@var`/…
/// **type** positions only — not prose. For each tag we take the following type
/// token (`User`, `User|null`, `\App\Models\User`, `User[]`) and rewrite
/// basename words in it that resolve to the target. Comments aren't in the AST,
/// so this is a targeted text scan; restricting to the type token keeps prose
/// like `// named User` from matching.
fn docblock_spans_in(
    tree: &tree_sitter::Tree,
    bytes: &[u8],
    aliases: &UseAliases,
    namespace: Option<&str>,
    target_fqcn: &str,
    old_basename: &str,
) -> Vec<(usize, usize)> {
    let resolves_to_target = |word: &str| -> bool {
        let resolved = resolve_class_name(word, aliases);
        let fqcn = if resolved.contains('\\') {
            resolved
        } else {
            match namespace {
                Some(ns) => format!("{ns}\\{resolved}"),
                None => resolved,
            }
        };
        fqcn == target_fqcn
    };

    let mut spans = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "comment" {
            if let Ok(text) = node.utf8_text(bytes) {
                if text.starts_with("/**") {
                    let base = node.start_byte();
                    for (type_start, type_str) in doc_type_tokens(text) {
                        for (woff, word) in word_spans(type_str) {
                            if word == old_basename && resolves_to_target(word) {
                                let at = base + type_start + woff;
                                spans.push((at, at + word.len()));
                            }
                        }
                    }
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    spans
}

/// Find each `@tag <type>` in a docblock and return `(byte offset of the type
/// token within `text`, the type token)`. The type is the whitespace-delimited
/// token immediately after a recognised tag.
fn doc_type_tokens(text: &str) -> Vec<(usize, &str)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    for tag in DOC_TYPE_TAGS {
        let mut from = 0;
        while let Some(rel) = text[from..].find(tag) {
            let tag_at = from + rel;
            from = tag_at + tag.len();
            // The char right after the tag must be whitespace, so `@param`
            // doesn't match inside `@parameters`.
            match bytes.get(from) {
                Some(b) if b.is_ascii_whitespace() => {}
                _ => continue,
            }
            // Skip whitespace to the type token.
            let mut i = from;
            while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
                i += 1;
            }
            let type_start = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i > type_start {
                out.push((type_start, &text[type_start..i]));
            }
        }
    }
    out
}

/// Identifier-like words in a string, with their byte offsets. Splits on any
/// non-identifier character so `User|null` yields `User`, `null`.
fn word_spans(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            out.push((start, &text[start..i]));
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests;
