//! Route outline builder for Laravel route files.
//!
//! Produces a hierarchical outline of routes that preserves group nesting,
//! computes combined URIs (a route inside `prefix('/api')` shows `/api/users`,
//! not `/users`), and combines route name prefixes (a route inside
//! `name('api.')` shows `api.users`, not `users`).
//!
//! The AST walk is owned by [`crate::route_chain`]; this module folds the
//! resulting chain tree into the `RouteOutline` hierarchy. Because the walker
//! matches every `Route::*` method by structure, `Route::livewire`,
//! `Route::resource`, `Route::apiResource`, and any future custom route method
//! surface automatically without us maintaining a verb allow-list here.
//!
//! The output is plain data (`RouteOutline`) so it can be memoized through
//! Salsa and converted to `SymbolEntry` by `document_symbols.rs`.

use crate::route_chain::{extract_route_chains, RouteChainNode};

/// One entry in the route outline tree. Leaf entries are route definitions
/// (HTTP verbs, `livewire`, `resource`, etc.); container entries are
/// `Route::group(...)` blocks with their nested routes as children.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RouteOutline {
    /// HTTP verb or Laravel route method, uppercased: GET, POST, RESOURCE,
    /// LIVEWIRE, REDIRECT, VIEW, etc. "GROUP" for container entries.
    pub method: String,
    /// Full URI including any inherited group prefixes. For groups, the
    /// group's accumulated prefix (e.g. "/api/admin" for a nested group).
    pub uri: String,
    /// Full route name including any inherited group name prefixes, if the
    /// route is named. For groups, the accumulated name prefix if any.
    pub name: Option<String>,
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
    /// For group containers, the nested routes/groups in source order.
    /// For leaf routes, always empty.
    pub children: Vec<RouteOutline>,
    /// True for `Route::group(...)` containers.
    pub is_group: bool,
}

/// Parse a route file's content and return the outline tree. Returns an
/// empty vec on parse failure.
pub fn extract_route_outline(content: &str) -> Vec<RouteOutline> {
    extract_route_outline_with_external(content, &[])
}

/// Like [`extract_route_outline`], but prepends an *external-file* group name
/// prefix to every displayed full route name (issue #43).
///
/// A file loaded via `Route::as('admin.')->group(base_path('that.php'))` has
/// its in-file routes resolve to `admin.<name>` at the project level, even
/// though this file's own AST only sees `<name>`. `external_prefixes` carries
/// those inherited prefixes; the *first* non-empty entry is used as the primary
/// display prefix (a single file can only display one outline, so we don't
/// duplicate the tree per prefix). An empty slice — or one containing only `""`
/// — yields byte-identical output to [`extract_route_outline`].
///
/// Only the displayed route *name* gains the prefix; URIs are unaffected
/// (external loads contribute name prefixes, not URI prefixes, in this code
/// path's resolution model).
pub fn extract_route_outline_with_external(
    content: &str,
    external_prefixes: &[String],
) -> Vec<RouteOutline> {
    let primary = external_prefixes
        .iter()
        .find(|p| !p.is_empty())
        .map(String::as_str)
        .unwrap_or("");

    extract_route_chains(content)
        .iter()
        .filter_map(|chain| fold_chain(chain, primary, ""))
        .collect()
}

/// Fold one shared-walker chain node into a `RouteOutline`, accumulating the
/// inherited URI prefix (`uri_prefix`) and the externally-applied name prefix
/// (`ext_name_prefix`, propagated to every nested level).
///
/// Returns `None` for chains that are neither a route definition nor a
/// non-empty group (e.g. a config-only `Route::pattern(...)` chain, or an empty
/// group — empty groups are noise and omitted).
fn fold_chain(
    chain: &RouteChainNode,
    ext_name_prefix: &str,
    uri_prefix: &str,
) -> Option<RouteOutline> {
    if let Some(verb) = chain.verb.as_ref() {
        // Leaf route definition.
        let route_uri = chain.uri.clone().unwrap_or_default();
        let combined_uri = format!("{}{}", uri_prefix, route_uri);
        let combined_name = chain
            .name
            .as_ref()
            .map(|n| format!("{}{}", ext_name_prefix, n.segment));

        return Some(RouteOutline {
            method: verb.to_uppercase(),
            uri: combined_uri,
            name: combined_name,
            start_line: chain.chain_range.start_line,
            start_column: chain.chain_range.start_column,
            end_line: chain.chain_range.end_line,
            end_column: chain.chain_range.end_column,
            children: Vec::new(),
            is_group: false,
        });
    }

    if chain.is_group() {
        // Group container. Apply this group's modifiers to the inherited
        // context, then recurse into the children with the new context.
        let prefix_arg = chain.prefix_arg.clone().unwrap_or_default();
        let name_arg = chain
            .name
            .as_ref()
            .map(|n| n.segment.as_str())
            .unwrap_or("");
        let group_uri = format!("{}{}", uri_prefix, prefix_arg);
        // The displayed group name prefix combines the external prefix with
        // this and all enclosing groups' name segments. We thread the combined
        // string down so children inherit it; the group node itself displays
        // the same combined value.
        let group_name = format!("{}{}", ext_name_prefix, name_arg);

        let children: Vec<RouteOutline> = chain
            .group_children
            .iter()
            // Children inherit the FULL accumulated name prefix (external +
            // this group's segment) and the FULL accumulated URI prefix.
            .filter_map(|c| fold_chain(c, &group_name, &group_uri))
            .collect();

        // Skip empty groups — they're noise.
        if children.is_empty() {
            return None;
        }

        // Position the group entry at the closure expression itself (not the
        // wider chain) so it overlaps the editor's tree-sitter "Closure" symbol
        // at the exact same range — gives the outline UI a chance to dedupe.
        let range = chain.group_closure_range.unwrap_or(chain.chain_range);

        return Some(RouteOutline {
            method: "GROUP".to_string(),
            uri: group_uri,
            name: if group_name.is_empty() {
                None
            } else {
                Some(group_name)
            },
            start_line: range.start_line,
            start_column: range.start_column,
            end_line: range.end_line,
            end_column: range.end_column,
            children,
            is_group: true,
        });
    }

    // Other chains (e.g. `Route::pattern(...)` config calls) are ignored.
    None
}

#[cfg(test)]
mod tests;
