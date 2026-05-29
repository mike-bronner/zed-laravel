//! Eager, project-wide index of where columns and tables are *defined* in
//! `database/migrations/*.php`. Powers goto-definition on query-chain literals:
//! a column literal jumps to its `$table->string('col')` line, a table literal
//! (`DB::table('users')`) to its `Schema::create('users'` line.
//!
//! Built once at init and refreshed when migration files change (mirrors the
//! route index). Parsing is tree-sitter, not regex — migrations nest closures
//! and span lines, and we need accurate positions for the jump target.
//!
//! Only column *definitions* are indexed: `Schema::create`/`Schema::table`
//! closures, member calls whose method is a Blueprint column type
//! ([`COLUMN_DEF_METHODS`]). Index/foreign/`dropColumn` calls reference columns
//! but don't define them, so they're skipped — we never jump to the wrong line.
//! First definition wins (the `create` migration over later `table` tweaks).

use crate::parser::parse_php;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tree_sitter::Node;
use walkdir::WalkDir;

/// A definition site inside a migration file. Positions are 0-based and point
/// at the *content* of the relevant string literal (inside the quotes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationSite {
    pub file: PathBuf,
    pub line: u32,
    pub start_char: u32,
    pub end_char: u32,
}

/// Resolved column/table definition sites across all project migrations.
#[derive(Debug, Clone, Default)]
pub struct MigrationIndex {
    /// `(table, column)` → where the column is defined.
    columns: HashMap<(String, String), MigrationSite>,
    /// `table` → where the table is created (`Schema::create`).
    tables: HashMap<String, MigrationSite>,
}

impl MigrationIndex {
    /// Definition site for `table`.`column`, if a migration defines it.
    pub fn column(&self, table: &str, column: &str) -> Option<&MigrationSite> {
        self.columns.get(&(table.to_string(), column.to_string()))
    }

    /// Creation site for `table`, if a migration creates it.
    pub fn table(&self, table: &str) -> Option<&MigrationSite> {
        self.tables.get(table)
    }

    pub fn column_count(&self) -> usize {
        self.columns.len()
    }

    pub fn table_count(&self) -> usize {
        self.tables.len()
    }
}

/// Blueprint methods that *define* a column, taking the column name as their
/// first string argument. Deliberately an allowlist: index/unique/foreign and
/// drop*/rename* reference columns without defining them, and excluding them
/// keeps goto from landing on the wrong line. `morphs`/`nullableMorphs` are
/// omitted — their arg names a relation, not a single literal column.
const COLUMN_DEF_METHODS: &[&str] = &[
    "bigIncrements",
    "bigInteger",
    "binary",
    "boolean",
    "char",
    "date",
    "dateTime",
    "dateTimeTz",
    "decimal",
    "double",
    "enum",
    "float",
    "foreignId",
    "foreignUlid",
    "foreignUuid",
    "geography",
    "geometry",
    "increments",
    "integer",
    "ipAddress",
    "json",
    "jsonb",
    "longText",
    "macAddress",
    "mediumIncrements",
    "mediumInteger",
    "mediumText",
    "set",
    "smallIncrements",
    "smallInteger",
    "string",
    "text",
    "time",
    "timeTz",
    "timestamp",
    "timestampTz",
    "tinyIncrements",
    "tinyInteger",
    "tinyText",
    "ulid",
    "unsignedBigInteger",
    "unsignedDecimal",
    "unsignedInteger",
    "unsignedMediumInteger",
    "unsignedSmallInteger",
    "unsignedTinyInteger",
    "uuid",
    "year",
];

/// Build the index by parsing every `*.php` under `<root>/database/migrations`.
/// Missing directory yields an empty index (non-Laravel project / no DB).
pub fn build_migration_index(root: &Path) -> MigrationIndex {
    let mut index = MigrationIndex::default();
    let dir = root.join("database").join("migrations");
    if !dir.exists() {
        return index;
    }
    for entry in WalkDir::new(&dir)
        .max_depth(4)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "php") {
            if let Ok(content) = std::fs::read_to_string(path) {
                index_migration_file(&mut index, path, &content);
            }
        }
    }
    index
}

/// Index a single migration file's `Schema::create`/`Schema::table` blocks.
/// Exposed for unit tests; `build_migration_index` is the production entry.
pub fn index_migration_file(index: &mut MigrationIndex, path: &Path, content: &str) {
    let Ok(tree) = parse_php(content) else {
        return;
    };
    let bytes = content.as_bytes();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "scoped_call_expression" {
            if let Some((table, table_node, method)) = schema_call(node, bytes) {
                if method == "create" {
                    index
                        .tables
                        .entry(table.clone())
                        .or_insert_with(|| site_from_string_node(path, table_node));
                }
                if let Some(args) = node.child_by_field_name("arguments") {
                    collect_columns(index, path, &table, args, bytes);
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
}

/// If `node` is a `Schema::create('t', …)` / `Schema::table('t', …)` call,
/// return `(table, table_name_string_node, method)`.
fn schema_call<'a>(node: Node<'a>, bytes: &[u8]) -> Option<(String, Node<'a>, &'static str)> {
    let scope = node.child_by_field_name("scope")?;
    let scope_text = scope.utf8_text(bytes).ok()?;
    // PHP class names are case-insensitive; match the `Schema` facade by its
    // basename so `\Illuminate\Support\Facades\Schema` also matches.
    let basename = scope_text.rsplit('\\').next().unwrap_or(scope_text);
    if !basename.eq_ignore_ascii_case("Schema") {
        return None;
    }
    let name = node.child_by_field_name("name")?.utf8_text(bytes).ok()?;
    let method = match name {
        "create" => "create",
        "table" => "table",
        _ => return None,
    };
    let args = node.child_by_field_name("arguments")?;
    let (table, table_node) = first_string_arg(args, bytes)?;
    Some((table, table_node, method))
}

/// Walk an arguments subtree for `$table->method('col')` column definitions,
/// mapping each to `(table, col)`. First definition wins.
fn collect_columns(index: &mut MigrationIndex, path: &Path, table: &str, args: Node, bytes: &[u8]) {
    let mut stack = vec![args];
    while let Some(node) = stack.pop() {
        if node.kind() == "member_call_expression" {
            if let Some(name) = node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(bytes).ok())
            {
                if COLUMN_DEF_METHODS.contains(&name) {
                    if let Some(call_args) = node.child_by_field_name("arguments") {
                        if let Some((column, col_node)) = first_string_arg(call_args, bytes) {
                            index
                                .columns
                                .entry((table.to_string(), column))
                                .or_insert_with(|| site_from_string_node(path, col_node));
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
}

/// First string-literal argument of an `arguments` node, as `(value, node)`.
fn first_string_arg<'a>(args: Node<'a>, bytes: &[u8]) -> Option<(String, Node<'a>)> {
    let mut cursor = args.walk();
    for child in args.children(&mut cursor) {
        if child.kind() != "argument" {
            continue;
        }
        if let Some(inner) = child.named_child(0) {
            if inner.kind() == "string" {
                if let Some(value) = string_literal_value(inner, bytes) {
                    return Some((value, inner));
                }
            }
        }
    }
    None
}

/// Decode a tree-sitter `string` node to its unquoted content.
fn string_literal_value(node: Node, bytes: &[u8]) -> Option<String> {
    let raw = node.utf8_text(bytes).ok()?;
    let trimmed = raw
        .strip_prefix(['\'', '"'])
        .and_then(|s| s.strip_suffix(['\'', '"']))
        .unwrap_or(raw);
    Some(trimmed.to_string())
}

/// Build a `MigrationSite` pointing at the *content* of a string literal node
/// (inside the quotes). Assumes a single-line, single-byte-quoted literal —
/// always true for table/column names.
fn site_from_string_node(path: &Path, node: Node) -> MigrationSite {
    let start = node.start_position();
    let end = node.end_position();
    // Inside the quotes: +1 past the opener, -1 before the closer.
    let (start_char, end_char) = if start.row == end.row && end.column > start.column + 1 {
        (start.column as u32 + 1, end.column as u32 - 1)
    } else {
        (start.column as u32, end.column as u32)
    };
    MigrationSite {
        file: path.to_path_buf(),
        line: start.row as u32,
        start_char,
        end_char,
    }
}

#[cfg(test)]
mod tests;
