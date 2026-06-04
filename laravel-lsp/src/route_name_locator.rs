//! Locate `->name('xxx')` (and the `->as('xxx')` alias) declaration sites
//! inside a Laravel route file via tree-sitter — column-accurate positions
//! suitable for building a rename `WorkspaceEdit`.
//!
//! The companion to `route_outline.rs`: that module extracts the *name string*
//! to label outline entries, this one extracts the *position of the name
//! string's content* so we can rewrite it in place. Both share the AST walk in
//! [`crate::route_chain`]; the divergence is that we need column ranges rather
//! than concatenated strings.
//!
//! Group prefixes are followed honestly: a route inside `Route::name('api.')`
//! whose own `->name('users')` resolves to declared name `api.users`, but the
//! *physical source position to rewrite* is just the `users` portion. Renaming
//! `api.users` to `api.accounts` therefore mutates only `users` → `accounts`
//! in source — the prefix lives in a separate declaration that the user can
//! rename independently if desired.

use crate::route_chain::{extract_route_chains, RouteChainNode};

/// One declaration site that contributes to a full route name. The
/// `name_arg_line/start_column/end_column` identifies the physical source
/// position of the string literal content (between the quotes) — the precise
/// range a rename `TextEdit` should replace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteNameDeclaration {
    /// Combined full route name including any inherited group `name(...)`
    /// prefixes. This is what the user sees when they search for a route.
    pub full_name: String,
    /// The literal source-text content of this declaration's own
    /// `->name(...)` / `->as(...)` argument (no quotes, no prefix).
    pub local_segment: String,
    /// Row of the string content. 0-based.
    pub line: u32,
    /// Column of the first character of the string content (after the opening
    /// quote). 0-based.
    pub start_column: u32,
    /// Column one past the last character of the string content (before the
    /// closing quote). 0-based.
    pub end_column: u32,
}

impl RouteNameDeclaration {
    /// Given the new fully-qualified route name the user typed (e.g.
    /// `admin.dashboard`), return the text to write at THIS declaration's
    /// source position — which spans only `local_segment`, not the whole
    /// dotted name.
    ///
    /// Because `full_name` is built as `inherited_prefix + local_segment`
    /// (see [`fold_chain`]), the prefix is recovered by stripping the segment
    /// back off. We then drop that same prefix from the new name so only this
    /// declaration's own segment is rewritten — the inherited
    /// `Route::name('admin.')` group prefix lives at a separate declaration
    /// the user renames independently.
    ///
    /// This deliberately differs from the config/translation rename, which
    /// take `new_key.rsplit('.').next()` (last dot segment). Route leaf
    /// segments can themselves be dotted (`->name('users.index')`), so a
    /// last-dot split would corrupt them; suffix-based prefix stripping
    /// preserves the full local segment.
    ///
    /// Fallback: if the new name doesn't carry the inherited prefix (the user
    /// rewrote the group portion — something a leaf rename can't express
    /// coherently), the new name is returned verbatim as a best effort.
    pub fn rewritten_segment<'a>(&self, new_full_name: &'a str) -> &'a str {
        let inherited_prefix = self
            .full_name
            .strip_suffix(&self.local_segment)
            .unwrap_or("");
        new_full_name
            .strip_prefix(inherited_prefix)
            .unwrap_or(new_full_name)
    }
}

/// Walk a route file and return every `->name(...)` / `->as(...)` declaration
/// in source order, regardless of whether they sit on leaf routes or on
/// `Route::group(...)` containers.
pub fn extract_route_name_declarations(content: &str) -> Vec<RouteNameDeclaration> {
    let mut output = Vec::new();
    for chain in extract_route_chains(content) {
        fold_chain(&chain, "", &mut output);
    }
    output
}

/// All declarations whose `full_name` equals `target`. Useful for rename:
/// returns every place that contributes to the same fully-qualified route
/// name (a route inside multiple nested `name('a.')` `name('b.')` groups has
/// one leaf and two group declarations).
pub fn find_declarations_named(content: &str, target: &str) -> Vec<RouteNameDeclaration> {
    find_declarations_named_with_external(content, target, &["".to_string()])
}

/// `find_declarations_named`, but also matching declarations whose name only
/// equals `target` once an *external-file* group prefix is prepended (issue
/// #43). A file loaded via `Route::as('admin.')->group(base_path('that.php'))`
/// has its in-file `->name('x')` resolve to `admin.x` at the project level,
/// even though this file's own AST only sees `x`.
///
/// For each declaration `d` and each `external_prefix` in `external_prefixes`,
/// the declaration matches when `external_prefix + d.full_name == target`. On a
/// match, the returned declaration's `full_name` is rewritten to the resolved
/// project-level name (`external_prefix + raw`) so that
/// [`RouteNameDeclaration::rewritten_segment`] strips the *combined* prefix and
/// still rewrites only the same leaf segment in source.
///
/// `external_prefixes` should always include `""` (the file is scanned directly
/// too); an empty slice is treated as `[""]`.
pub fn find_declarations_named_with_external(
    content: &str,
    target: &str,
    external_prefixes: &[String],
) -> Vec<RouteNameDeclaration> {
    // Normalize to a non-empty set that always contains the empty prefix.
    let mut effective: Vec<&str> = vec![""];
    for p in external_prefixes {
        if !p.is_empty() && !effective.contains(&p.as_str()) {
            effective.push(p.as_str());
        }
    }

    let mut out = Vec::new();
    for d in extract_route_name_declarations(content) {
        for ext in &effective {
            if ext.is_empty() {
                if d.full_name == target {
                    out.push(d.clone());
                }
            } else if format!("{}{}", ext, d.full_name) == target {
                // Re-anchor `full_name` to the resolved project-level name so
                // `rewritten_segment` strips the combined (external + group)
                // prefix and rewrites only this declaration's own leaf.
                let mut resolved = d.clone();
                resolved.full_name = format!("{}{}", ext, d.full_name);
                out.push(resolved);
            }
        }
    }
    out
}

/// Fold one shared-walker chain node (and its group children) into route-name
/// declarations, accumulating the enclosing group `name(...)` prefix.
///
/// A declaration is emitted for any chain that carries a `->name`/`->as`
/// argument — whether it's a leaf route or a group container (the group's own
/// `->name('admin.')` is itself a renameable declaration). The group's segment
/// then extends the prefix for its children.
fn fold_chain(chain: &RouteChainNode, name_prefix: &str, output: &mut Vec<RouteNameDeclaration>) {
    if let Some(name) = chain.name.as_ref() {
        output.push(RouteNameDeclaration {
            full_name: format!("{}{}", name_prefix, name.segment),
            local_segment: name.segment.clone(),
            line: name.line,
            start_column: name.start_column,
            end_column: name.end_column,
        });
    }

    if chain.is_group() {
        let extra = chain
            .name
            .as_ref()
            .map(|n| n.segment.as_str())
            .unwrap_or("");
        let child_prefix = format!("{}{}", name_prefix, extra);
        for child in &chain.group_children {
            fold_chain(child, &child_prefix, output);
        }
    }
}

#[cfg(test)]
mod tests;
