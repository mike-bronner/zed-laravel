//! Prove that a class member is *read* somewhere in its inheritance chain —
//! including parent classes and used traits that live in `vendor/`.
//!
//! ## Why this exists
//!
//! The unused-symbol diagnostic flags a code-lensed member with zero project
//! references as "possibly dead code". That's wrong for **framework-read
//! configuration properties**: a model's `public $timestamps = false;` is never
//! referenced by *app* code, but `Illuminate\Database\Eloquent\Concerns\
//! HasTimestamps` reads `$this->timestamps`. The property isn't dead — the
//! framework reads it via inheritance.
//!
//! Rather than maintain a hand-written allowlist of Eloquent's config
//! properties (a heuristic that drifts with each Laravel release), this module
//! *proves* the read deterministically: it walks the class's `extends` parents
//! and `use`d traits — resolving each FQCN (app **or** vendor) to a file — and
//! checks whether any of them reads `$this->member` (or `self::$member` /
//! `static::$member`). If something in the chain reads it, it isn't dead.
//!
//! ## Scope — we do NOT index all of `vendor/`
//!
//! Only the files actually *in the chain* are resolved and parsed. For a real
//! Eloquent model that's `Model` plus its concern traits — roughly two dozen
//! small files — resolved lazily and parsed once. The full `vendor/` tree
//! (often 10k+ files) is never walked.
//!
//! ## Concurrency
//!
//! [`member_read_in_chain`] is pure and **synchronous**: it does filesystem IO
//! and tree-sitter parsing inline. Call it from `spawn_blocking`, never from
//! inside the Salsa actor — a slow walk there would serialize with every other
//! in-flight LSP request.

use std::collections::HashSet;
use std::path::Path;

use tree_sitter::Node;

use crate::class_hierarchy_index::classes_in_file;
use crate::class_locator::find_php_class_file_in_app_or_vendor;
use crate::parser::parse_php;

/// Safety bound on how many classes a single chain walk will resolve+parse. A
/// real Eloquent model's chain (`Model` + its concern traits) is ~25 classes;
/// 256 is generous headroom while still capping a pathological hierarchy (or a
/// resolution cycle the visited-set somehow doesn't catch).
const MAX_CHAIN_VISITS: usize = 256;

/// Whether `member` is read as `$this->member` — or `self::$member` /
/// `static::$member` / `Foo::$member` — in `fqcn`'s own source or in any class
/// or trait reachable by walking its `extends` parents and `use`d traits.
///
/// Each FQCN (app or vendor) is resolved to a file via
/// [`find_php_class_file_in_app_or_vendor`], parsed, and scanned; its parent and
/// traits are then enqueued. The walk is cycle-safe (each FQCN visited once) and
/// bounded by [`MAX_CHAIN_VISITS`].
///
/// Returns `false` for inputs that don't name a resolvable class — an empty
/// FQCN, or a synthetic key like `volt::/path/to/file` (which carries `::`) —
/// without touching the filesystem.
///
/// See the module docs for the concurrency contract (call from
/// `spawn_blocking`).
pub fn member_read_in_chain(root: &Path, fqcn: &str, member: &str) -> bool {
    // Synthetic component keys (`volt::<path>`) and empty names don't name a
    // class. Bail before the resolver does a fruitless `vendor/` basename walk.
    if fqcn.trim().is_empty() || fqcn.contains("::") || member.is_empty() {
        return false;
    }

    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: Vec<String> = vec![normalize(fqcn)];

    while let Some(current) = queue.pop() {
        if visited.len() >= MAX_CHAIN_VISITS {
            break;
        }
        if current.is_empty() || !visited.insert(current.clone()) {
            continue;
        }
        let Some(path) = find_php_class_file_in_app_or_vendor(&current, root) else {
            continue;
        };
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if source_reads_member(&content, member) {
            return true;
        }
        // Enqueue parents + traits (already resolved to FQCNs by the hierarchy
        // extractor). PSR-4 puts one class per file, so taking every node the
        // file declares is correct in practice and harmless otherwise (the
        // visited-set dedups, the bound caps fan-out).
        for node in classes_in_file(&path, &content) {
            if let Some(parent) = node.extends {
                queue.push(normalize(&parent));
            }
            for tr in node.trait_uses {
                queue.push(normalize(&tr));
            }
        }
    }
    false
}

/// Strip a leading `\` and surrounding whitespace so FQCNs compare/dedup
/// consistently (`\Illuminate\…\Model` and `Illuminate\…\Model` are the same).
fn normalize(fqcn: &str) -> String {
    fqcn.trim().trim_start_matches('\\').to_string()
}

/// Whether `source` contains any read of `member` on `$this` (instance) or via a
/// scope (`self::$member` / `static::$member` / `Foo::$member`).
fn source_reads_member(source: &str, member: &str) -> bool {
    let Ok(tree) = parse_php(source) else {
        return false;
    };
    let bytes = source.as_bytes();
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if node_reads_member(n, bytes, member) {
            return true;
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    false
}

/// One node: is it a `$this->member` access or a `::$member` scoped access whose
/// member name equals `member`?
fn node_reads_member(node: Node, bytes: &[u8], member: &str) -> bool {
    match node.kind() {
        "member_access_expression" | "nullsafe_member_access_expression" => {
            let Some(object) = node.child_by_field_name("object") else {
                return false;
            };
            if object.kind() != "variable_name"
                || object.utf8_text(bytes).ok() != Some("$this")
            {
                return false;
            }
            node.child_by_field_name("name")
                .and_then(|n| n.utf8_text(bytes).ok())
                .map(|t| t.trim_start_matches('$') == member)
                .unwrap_or(false)
        }
        // `self::$x` / `static::$x` / `Foo::$x`: the scope is a name/relative
        // scope; the property part is a `variable_name`. Match on the variable.
        "scoped_property_access_expression" => {
            let mut c = node.walk();
            let found = node.children(&mut c).any(|ch| {
                ch.kind() == "variable_name"
                    && ch
                        .utf8_text(bytes)
                        .ok()
                        .map(|t| t.trim_start_matches('$') == member)
                        .unwrap_or(false)
            });
            found
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests;
