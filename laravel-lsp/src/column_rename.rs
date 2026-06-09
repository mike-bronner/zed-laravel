//! Column rename engine (M8) — renaming a DB column project-wide.
//!
//! Builds on the magic-member rename (M7). A column has no single declaring
//! method (it lives in the database, surfaced as a model attribute), so its
//! rename touches four distinct site classes:
//!
//! 1. **String-literal column args in query chains** — `where('email', …)`,
//!    `orderBy('email')`, `select('email')`, `pluck('email')`, … The literal
//!    is rewritten *only when the chain resolves to the target table* (the
//!    table-match decision is async — model→table resolution — so it lives in
//!    the LSP integration layer; this module surfaces the candidate sites plus
//!    the owning chain index so the caller can filter).
//! 2. **Property-form usages** — `$user->email`, `{{ $user->email }}`. These
//!    are already in the magic-member inverted index; the integration layer
//!    rewrites them via `find_references(MagicMember { … })`.
//! 3. **Model array entries** — the `'email'` string in the declaring model's
//!    `$fillable` / `$casts` / `$hidden` / `$guarded` / `$dates` arrays.
//! 4. **A generated migration** — a `Schema::table(…, renameColumn(old, new))`
//!    file emitted as a `CreateFile` + content insert in the `WorkspaceEdit`.
//!
//! Everything in this module is **pure and synchronously testable**: it parses
//! a `&str`, walks the tree, and returns 0-based positions (`ColumnArgSite`)
//! pointing at the *column name inside the quotes* — never the quotes
//! themselves, and never a table qualifier. The async table-resolution and the
//! `WorkspaceEdit` assembly live in `main.rs`.

use crate::query_chain::{ArgKind, BuilderChain, ChainArg, ChainLink};

/// A single source position to rewrite during a column rename, 0-based.
///
/// Points at the *column name text inside the string quotes*. For a qualified
/// literal `'users.email'`, `start_column`/`end_column` bracket only the
/// `email` segment — the `users.` qualifier is preserved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnArgSite {
    pub line: u32,
    pub start_column: u32,
    pub end_column: u32,
}

/// A column-arg literal found in a query chain, paired with the index of the
/// chain that owns it (into the `Vec<BuilderChain>` returned by
/// [`crate::query_chain::extract_chains`]). The integration layer resolves
/// each chain's accessible tables once and keeps only the sites whose chain
/// matches the target table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainColumnLiteral {
    /// Index into the extracted chains slice — identifies the owning chain so
    /// the caller resolves its table exactly once.
    pub chain_index: usize,
    pub site: ColumnArgSite,
    /// The table qualifier when the literal was written qualified
    /// (`'users.email'` → `Some("users")`); `None` for a bare `'email'`. The
    /// integration layer uses this to decide table-match: a qualified literal
    /// matches only when its qualifier resolves to the target table, while a
    /// bare literal matches whenever the target table is accessible to the
    /// chain.
    pub qualifier: Option<String>,
}

/// Column methods where **every** string-literal argument is a column name:
/// `select('id', 'email')`, `groupBy('a', 'b')`, etc. For these we rewrite all
/// matching args. `whereColumn('a', '=', 'b')` also belongs here — both the
/// first and third args are columns.
const MULTI_COLUMN_METHODS: &[&str] = &[
    "select",
    "addSelect",
    "groupBy",
    "orderBy",
    "orderByDesc",
    "whereColumn",
];

/// Decide, for a column-method link, which of its string-literal args are
/// column references. Conservative by design: for the common filter methods
/// (`where`, `having`, `pluck`, …) only the **first** string arg is a column —
/// later args are operators/values/bindings and must never be touched, so a
/// `where('status', '=', 'email')` data value is left alone. For the
/// [`MULTI_COLUMN_METHODS`] every string arg is a column.
fn column_arg_is_relevant(method: &str, string_arg_ordinal: usize) -> bool {
    if MULTI_COLUMN_METHODS.contains(&method) {
        true
    } else {
        string_arg_ordinal == 0
    }
}

/// Locate every query-chain column-arg literal in `source` whose column name
/// equals `old_column`, paired with the owning chain index.
///
/// Parses `source` as PHP, extracts the builder chains, and for each link with
/// [`ArgKind::Column`] inspects its string-literal args (per
/// [`column_arg_is_relevant`]). A literal matches when its column name — the
/// last dot-segment, so `'users.email'` matches `email` — equals `old_column`.
///
/// The returned site brackets only the matching column-name text inside the
/// quotes; a qualified literal keeps its `qualifier.` prefix intact.
///
/// Returns an empty `Vec` (never `None`) when the source has no matching
/// chains, so callers can chain it without an `Option`. Parse failure is
/// likewise an empty `Vec` — a column rename should degrade to "fewer sites",
/// never an error.
pub fn chain_column_literals(source: &str, old_column: &str) -> Vec<ChainColumnLiteral> {
    let Ok(tree) = crate::parser::parse_php(source) else {
        return Vec::new();
    };
    let chains = crate::query_chain::extract_chains(&tree, source);
    let mut out = Vec::new();
    for (chain_index, chain) in chains.iter().enumerate() {
        collect_chain_sites(chain, chain_index, source, old_column, &mut out);
    }
    out
}

/// Walk one chain's column-method links and push matching literal sites.
fn collect_chain_sites(
    chain: &BuilderChain,
    chain_index: usize,
    source: &str,
    old_column: &str,
    out: &mut Vec<ChainColumnLiteral>,
) {
    for link in &chain.links {
        if link.arg != ArgKind::Column {
            continue;
        }
        collect_link_sites(link, chain_index, source, old_column, out);
    }
}

/// Push the matching column-name sites from a single column-method link.
/// `string_arg_ordinal` counts only string-literal args (so closures/`Other`
/// args between strings don't shift the ordinal a human would expect).
fn collect_link_sites(
    link: &ChainLink,
    chain_index: usize,
    source: &str,
    old_column: &str,
    out: &mut Vec<ChainColumnLiteral>,
) {
    let mut string_arg_ordinal = 0usize;
    for arg in &link.args {
        let ChainArg::StringLit {
            value,
            span_byte_range,
            ..
        } = arg
        else {
            continue;
        };
        let relevant = column_arg_is_relevant(&link.method, string_arg_ordinal);
        string_arg_ordinal += 1;
        if !relevant {
            continue;
        }
        if let Some((site, qualifier)) =
            column_site_in_literal(source, value, span_byte_range.0, old_column)
        {
            out.push(ChainColumnLiteral {
                chain_index,
                site,
                qualifier,
            });
        }
    }
}

/// Given a string literal's decoded `value`, the byte offset of its opening
/// quote (`literal_start`), and the `old_column` to match, return the
/// [`ColumnArgSite`] bracketing the column-name text — or `None` if the
/// literal's column name isn't `old_column`.
///
/// The literal's content begins one byte after the opening quote (single- and
/// double-quoted column names never contain escapes in practice). For a
/// qualified value `users.email`, only the trailing `email` segment is
/// bracketed, so the `users.` qualifier survives the rewrite.
fn column_site_in_literal(
    source: &str,
    value: &str,
    literal_start: usize,
    old_column: &str,
) -> Option<(ColumnArgSite, Option<String>)> {
    // The column name is the last dot-segment (`users.email` → `email`); a
    // bare `email` has no dot and uses the whole value. The text before the
    // last dot is the qualifier the chain references the column by.
    let (segment_offset, column_name, qualifier) = match value.rfind('.') {
        Some(dot) => (dot + 1, &value[dot + 1..], Some(value[..dot].to_string())),
        None => (0, value, None),
    };
    if column_name != old_column {
        return None;
    }
    // Content starts 1 byte past the opening quote; the matched segment starts
    // `segment_offset` bytes into the content.
    let name_start_byte = literal_start + 1 + segment_offset;
    let name_end_byte = name_start_byte + column_name.len();
    let start = byte_to_line_col(source, name_start_byte)?;
    let end = byte_to_line_col(source, name_end_byte)?;
    // A column literal never spans lines; if it somehow does, skip it.
    if start.0 != end.0 {
        return None;
    }
    Some((
        ColumnArgSite {
            line: start.0,
            start_column: start.1,
            end_column: end.1,
        },
        qualifier,
    ))
}

/// Translate a byte offset into 0-based `(line, column)`, counting columns in
/// Unicode code points (consistent with the rest of the LSP's position math).
/// `None` if the offset is past the end of `source`.
fn byte_to_line_col(source: &str, byte_offset: usize) -> Option<(u32, u32)> {
    if byte_offset > source.len() {
        return None;
    }
    let mut line: u32 = 0;
    let mut line_start = 0usize;
    for (idx, b) in source.bytes().enumerate() {
        if idx >= byte_offset {
            break;
        }
        if b == b'\n' {
            line += 1;
            line_start = idx + 1;
        }
    }
    let column = source
        .get(line_start..byte_offset)
        .map(|slice| slice.chars().count() as u32)
        .unwrap_or(0);
    Some((line, column))
}

/// Model array properties whose string entries are column names — rewritten in
/// the declaring model file when the column is renamed.
const MODEL_COLUMN_ARRAYS: &[&str] = &["fillable", "guarded", "hidden", "dates"];

/// Locate the `'<old_column>'` string entries in the declaring model's
/// column-name arrays (`$fillable`, `$guarded`, `$hidden`, `$dates`) **and**
/// the *keys* of its `$casts` array, returning a [`ColumnArgSite`] for each
/// (bracketing the name inside the quotes).
///
/// `$casts` is special: it's an associative array (`'email' => 'string'`) where
/// only the **key** is a column — the value is a cast type and must not be
/// touched. The other arrays are flat lists of column-name strings.
///
/// Pure: parses `source`, walks property declarations, matches by property
/// name. First class in the file wins (PSR-4 = one class per file).
pub fn model_array_sites(source: &str, old_column: &str) -> Vec<ColumnArgSite> {
    let Ok(tree) = crate::parser::parse_php(source) else {
        return Vec::new();
    };
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "property_declaration" {
            collect_property_array_sites(node, bytes, source, old_column, &mut out);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    // The stack walk visits nodes in reverse document order; sort so callers
    // (and tests) see sites top-to-bottom, left-to-right.
    out.sort_by_key(|s| (s.line, s.start_column));
    out
}

/// Inspect a `property_declaration`; if it's one of the recognised column
/// arrays, push matching string-entry sites (flat list) or `$casts` *key*
/// sites (associative).
fn collect_property_array_sites(
    node: tree_sitter::Node,
    bytes: &[u8],
    source: &str,
    old_column: &str,
    out: &mut Vec<ColumnArgSite>,
) {
    // A property_declaration holds one-or-more `property_element` children,
    // each with a `variable_name` and optional `= <array>` initializer.
    let mut cursor = node.walk();
    for element in node.children(&mut cursor) {
        if element.kind() != "property_element" {
            continue;
        }
        let Some(name) = property_element_name(element, bytes) else {
            continue;
        };
        let is_casts = name == "casts";
        if !is_casts && !MODEL_COLUMN_ARRAYS.contains(&name.as_str()) {
            continue;
        }
        let Some(array) = find_array_creation(element) else {
            continue;
        };
        if is_casts {
            collect_casts_key_sites(array, bytes, source, old_column, out);
        } else {
            collect_flat_array_sites(array, bytes, source, old_column, out);
        }
    }
}

/// Read a `property_element`'s name (the `$variable_name` without the `$`).
fn property_element_name(element: tree_sitter::Node, bytes: &[u8]) -> Option<String> {
    let mut cursor = element.walk();
    for child in element.children(&mut cursor) {
        if child.kind() == "variable_name" {
            let text = child.utf8_text(bytes).ok()?;
            return Some(text.trim_start_matches('$').to_string());
        }
    }
    None
}

/// Find the `array_creation_expression` initializer under a `property_element`.
// The borrow checker forbids the iterator-`find` form here: the returned
// `Node` outlives the `cursor` temporary the iterator borrows. An explicit
// loop returns the node by value before `cursor` drops.
#[allow(clippy::manual_find)]
fn find_array_creation(element: tree_sitter::Node) -> Option<tree_sitter::Node> {
    let mut cursor = element.walk();
    for child in element.children(&mut cursor) {
        if child.kind() == "array_creation_expression" {
            return Some(child);
        }
    }
    None
}

/// Flat array (`$fillable = ['email', …]`): each `array_element_initializer`
/// holds a single string — push a site for every one matching `old_column`.
fn collect_flat_array_sites(
    array: tree_sitter::Node,
    bytes: &[u8],
    source: &str,
    old_column: &str,
    out: &mut Vec<ColumnArgSite>,
) {
    let mut cursor = array.walk();
    for element in array.children(&mut cursor) {
        if element.kind() != "array_element_initializer" {
            continue;
        }
        // A flat entry has exactly one named child: the value string.
        if let Some(string_node) = element.named_child(0) {
            if let Some(site) = string_literal_site(string_node, bytes, source, old_column) {
                out.push(site);
            }
        }
    }
}

/// Associative `$casts` array (`['email' => 'string']`): only the **key**
/// (first named child of each `array_element_initializer`) is a column name.
fn collect_casts_key_sites(
    array: tree_sitter::Node,
    bytes: &[u8],
    source: &str,
    old_column: &str,
    out: &mut Vec<ColumnArgSite>,
) {
    let mut cursor = array.walk();
    for element in array.children(&mut cursor) {
        if element.kind() != "array_element_initializer" {
            continue;
        }
        // Associative entry: `key => value` → two named children; the key is
        // the first. A flat entry (no `=>`) has one — skip it for $casts.
        if element.named_child_count() < 2 {
            continue;
        }
        if let Some(key_node) = element.named_child(0) {
            if let Some(site) = string_literal_site(key_node, bytes, source, old_column) {
                out.push(site);
            }
        }
    }
}

/// If `node` is a `string`/`encapsed_string` whose decoded content equals
/// `old_column`, return the site bracketing the content inside the quotes.
fn string_literal_site(
    node: tree_sitter::Node,
    bytes: &[u8],
    source: &str,
    old_column: &str,
) -> Option<ColumnArgSite> {
    if !matches!(node.kind(), "string" | "encapsed_string") {
        return None;
    }
    // The `string_content` child is the unquoted text and carries its own
    // position — use it directly so quotes and escapes are handled by the
    // parser, not by hand.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "string_content" {
            let text = child.utf8_text(bytes).ok()?;
            if text != old_column {
                return None;
            }
            let s = child.start_position();
            let e = child.end_position();
            // Column names are single-line; bail if not.
            if s.row != e.row {
                return None;
            }
            return Some(ColumnArgSite {
                line: s.row as u32,
                start_column: s.column as u32,
                end_column: e.column as u32,
            });
        }
    }
    let _ = source; // reserved for future escape-aware handling
    None
}

// ── Migration generation ──────────────────────────────────────────────────

/// The migration filename (no directory) for a column rename:
/// `<timestamp>_rename_<old>_to_<new>_in_<table>_table.php`. `timestamp` is the
/// `YYYY_MM_DD_HHMMSS` prefix from
/// [`crate::query_chain::code_actions::format_migration_timestamp`].
pub fn rename_migration_filename(
    timestamp: &str,
    old_column: &str,
    new_column: &str,
    table: &str,
) -> String {
    format!("{timestamp}_rename_{old_column}_to_{new_column}_in_{table}_table.php")
}

/// Render the migration body for renaming `old_column` → `new_column` on
/// `table`. `up()` renames forward, `down()` reverses it — symmetric so the
/// migration is reversible. Self-contained (doesn't read a stub): a
/// `renameColumn` body is a single call, so a fixed anonymous-class template
/// matches modern Laravel output without stub substitution.
pub fn rename_migration_content(table: &str, old_column: &str, new_column: &str) -> String {
    format!(
        r#"<?php

use Illuminate\Database\Migrations\Migration;
use Illuminate\Database\Schema\Blueprint;
use Illuminate\Support\Facades\Schema;

return new class extends Migration
{{
    /**
     * Run the migrations.
     */
    public function up(): void
    {{
        Schema::table('{table}', function (Blueprint $table) {{
            $table->renameColumn('{old_column}', '{new_column}');
        }});
    }}

    /**
     * Reverse the migrations.
     */
    public function down(): void
    {{
        Schema::table('{table}', function (Blueprint $table) {{
            $table->renameColumn('{new_column}', '{old_column}');
        }});
    }}
}};
"#
    )
}

#[cfg(test)]
mod tests;
