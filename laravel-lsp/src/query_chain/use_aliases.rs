//! Parse `use` statements in PHP source into an alias → fully-qualified-name
//! map.
//!
//! PHP class names are case-insensitive but the alias *name* (the local
//! identifier) is matched as-typed by source. We store keys as-typed.
//! Resolution helpers do case-insensitive lookups so `db` and `DB` both
//! resolve when an import says `use Foo\DB;`.
//!
//! Scope: top-level `use` statements (the `namespace_use_declaration` node).
//! Grouped uses (`use Foo\{Bar, Baz as B};`) are also handled. Function and
//! constant uses (`use function foo;`, `use const FOO;`) are ignored —
//! chains never receive functions or constants as their static scope.

use std::collections::HashMap;
use tree_sitter::{Node, Tree};

/// Map from the local name in source → the fully-qualified class name.
///
/// Examples:
/// - `use Illuminate\Support\Facades\DB;` → `"DB"` → `"Illuminate\Support\Facades\DB"`
/// - `use Illuminate\Support\Facades\DB as Database;` → `"Database"` → `"Illuminate\Support\Facades\DB"`
/// - `use App\Models\{User, Post as P};` → `"User"` → `"App\Models\User"`, `"P"` → `"App\Models\Post"`
pub type UseAliases = HashMap<String, String>;

/// Extract every `use` import in the file. Returns an empty map if no
/// imports exist or the file has parse errors.
pub fn extract_use_aliases(tree: &Tree, source: &str) -> UseAliases {
    let bytes = source.as_bytes();
    let mut aliases: UseAliases = HashMap::new();

    let mut stack: Vec<Node> = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "namespace_use_declaration" {
            collect_from_declaration(node, bytes, &mut aliases);
            // Don't recurse into the declaration — we've handled its children.
            continue;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }

    aliases
}

/// Resolve a class reference as it appears in source to its FQCN, by
/// looking up the leading segment in the alias map. Falls back to the
/// original string when no match — Laravel's global aliases (the
/// `config/app.php` `aliases` array, which makes `\DB` etc. available
/// everywhere) are NOT in PHP's use-statement scope so they end up here.
///
/// Examples:
/// - `Database::table` with `Database → Illuminate\Support\Facades\DB` → `Illuminate\Support\Facades\DB`
/// - `\DB::table` (leading `\`) → `DB` (unchanged after stripping leading `\`)
/// - `DB::table` with no import → `DB` (unchanged; relies on global alias)
/// - `App\Foo::method` (no segment in map) → `App\Foo` (unchanged)
pub fn resolve_class_name(class: &str, aliases: &UseAliases) -> String {
    let stripped = class.trim_start_matches('\\');
    if let Some((head, rest)) = split_first_segment(stripped) {
        // Case-insensitive lookup against the map keys.
        for (alias, fqcn) in aliases {
            if alias.eq_ignore_ascii_case(head) {
                return if rest.is_empty() {
                    fqcn.clone()
                } else {
                    format!("{fqcn}\\{rest}")
                };
            }
        }
    }
    stripped.to_string()
}

/// Walk a `namespace_use_declaration` and add every clause to `aliases`.
///
/// AST shapes (from tree-sitter-php):
///
/// Flat: `use Foo\Bar as Baz;`
/// ```text
/// namespace_use_declaration
///   use
///   namespace_use_clause
///     qualified_name    <- the FQCN
///     as
///     name              <- the alias (only present with `as`)
/// ```
///
/// Grouped: `use Foo\{Bar, Baz as B};`
/// ```text
/// namespace_use_declaration
///   use
///   namespace_name      <- the shared prefix
///   namespace_use_group
///     namespace_use_clause
///       name            <- "Bar" (no alias)
///     namespace_use_clause
///       name            <- "Baz"
///       as
///       name            <- "B"
/// ```
///
/// Function/const: `use function foo;` — the `function` marker is INSIDE
/// the clause, not at declaration level.
fn collect_from_declaration(decl: Node, bytes: &[u8], aliases: &mut UseAliases) {
    // Find the (optional) prefix and (optional) group, scanning direct
    // children of the declaration.
    let mut prefix: Option<String> = None;
    let mut group: Option<Node> = None;
    let mut cursor = decl.walk();
    for child in decl.children(&mut cursor) {
        match child.kind() {
            "namespace_name" => prefix = node_text(child, bytes).map(String::from),
            "namespace_use_group" => group = Some(child),
            _ => {}
        }
    }

    // Clauses live inside the group when present, otherwise directly under
    // the declaration.
    let clauses_parent = group.unwrap_or(decl);
    let mut cursor = clauses_parent.walk();
    for clause in clauses_parent.children(&mut cursor) {
        if clause.kind() == "namespace_use_clause" {
            insert_clause(clause, bytes, prefix.as_deref(), aliases);
        }
    }
}

/// Insert one `namespace_use_clause` into the alias map. `prefix` is the
/// shared prefix from a grouped use, if any.
///
/// Skips `function` / `const` imports — those don't bind classes, so chains
/// would never reference them as static receivers.
fn insert_clause(clause: Node, bytes: &[u8], prefix: Option<&str>, aliases: &mut UseAliases) {
    // Collect children once so we can scan for both the function/const
    // modifier and the class name without re-walking.
    let mut cursor = clause.walk();
    let children: Vec<Node> = clause.children(&mut cursor).collect();

    if children
        .iter()
        .any(|c| matches!(c.kind(), "function" | "const"))
    {
        return;
    }

    // The class name is the first `qualified_name` | `namespace_name` |
    // `name` child. `name` covers the single-identifier case inside grouped
    // imports (`{User, Post}`).
    let name_node = children
        .iter()
        .find(|c| matches!(c.kind(), "qualified_name" | "namespace_name" | "name"))
        .copied();
    let Some(name_node) = name_node else {
        return;
    };
    let Some(name_text) = node_text(name_node, bytes) else {
        return;
    };
    let name_clean = name_text.trim_start_matches('\\');

    let fqcn = match prefix {
        Some(p) => format!("{p}\\{name_clean}"),
        None => name_clean.to_string(),
    };

    // The alias name (if present) is the `name` node that comes AFTER an
    // `as` token among the clause's direct children. Walk in source order
    // so we don't mistake the class-name's `name` (in the grouped case) for
    // an alias.
    let mut alias: Option<String> = None;
    let mut saw_as = false;
    for child in &children {
        if child.kind() == "as" {
            saw_as = true;
            continue;
        }
        if saw_as && child.kind() == "name" {
            alias = node_text(*child, bytes).map(String::from);
            break;
        }
    }

    // No `as` — alias is the last segment of the FQCN.
    let alias = alias.unwrap_or_else(|| fqcn.rsplit('\\').next().unwrap_or(&fqcn).to_string());

    aliases.insert(alias, fqcn);
}

fn split_first_segment(class: &str) -> Option<(&str, &str)> {
    match class.find('\\') {
        Some(i) => Some((&class[..i], &class[i + 1..])),
        None if class.is_empty() => None,
        None => Some((class, "")),
    }
}

fn node_text<'a>(node: Node<'_>, bytes: &'a [u8]) -> Option<&'a str> {
    let start = node.start_byte();
    let end = node.end_byte();
    std::str::from_utf8(bytes.get(start..end)?).ok()
}

#[cfg(test)]
mod tests;
