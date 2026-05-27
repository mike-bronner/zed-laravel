//! Build LSP `CompletionItem`s from a resolved [`ChainContext`].
//!
//! Three column-completion helpers, picked by `BuilderMode`:
//!
//! - [`columns_raw`] — DB schema only. Used by `BaseBuilder` chains
//!   (`DB::table(...)` and post-`toBase()` Eloquent chains). No casts, no
//!   accessors, no relations.
//! - `columns_for_builder` (Phase 4) — DB columns with cast-aware PHP types
//!   in `detail`. Eloquent builder pre-execution.
//! - `columns_for_collection` (Phase 6) — DB columns + accessors + cast
//!   property names. Eloquent collection post-execution.
//!
//! `relations` (Phase 5) — Eloquent-only relation list for `with()` /
//! `whereHas()` / `load()` etc.
//!
//! Phase 3 ships only `columns_raw`. The other functions are stubbed so
//! their wiring sites can compile against the same module surface.

use super::chain::*;
use crate::database::DatabaseSchemaProvider;
use tower_lsp::lsp_types::{CompletionItem, CompletionItemKind};

/// Build column completions for a `BaseBuilder` chain. Reads the schema
/// directly — no model lookup, no casts, no accessors.
///
/// Returns an empty `Vec` when:
/// - The context has no `effective_table` (shouldn't happen for Base chains
///   coming out of the cursor resolver, but guarded for safety)
/// - The database schema isn't introspected yet (cold start, no DB connection,
///   or the table doesn't exist in the introspected schema)
pub async fn columns_raw(ctx: &ChainContext, db: &DatabaseSchemaProvider) -> Vec<CompletionItem> {
    let Some(table) = &ctx.effective_table else {
        return Vec::new();
    };

    let columns = db.get_columns_with_types(table).await;
    columns
        .into_iter()
        .map(|(name, php_type)| {
            CompletionItem {
                label: name.clone(),
                kind: Some(CompletionItemKind::FIELD),
                detail: Some(format!("{php_type} ({table})")),
                // DB columns rank first; later modes will add accessors at
                // sort_text = "2_…" so columns still rank above them.
                sort_text: Some(format!("1_{name}")),
                filter_text: Some(name.clone()),
                insert_text: Some(name),
                ..Default::default()
            }
        })
        .collect()
}
