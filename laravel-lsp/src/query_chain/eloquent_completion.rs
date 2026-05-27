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
use tower_lsp::lsp_types::{CompletionItem, CompletionItemKind, CompletionItemLabelDetails};
use tracing::info;

/// Build table-name completions for the cursor inside `DB::table('|')` or
/// right after `DB::table(|`. Reads `DatabaseSchema::get_tables()` and
/// returns one item per table.
///
/// `wrap_with_quote` controls `insert_text` formatting:
/// - `None` — the source already has quotes around the cursor (user typed
///   `'` and we're inside it). Insert bare.
/// - `Some(q)` — the source has no quotes (user just typed `(`). Insert
///   wrapped: `q + name + q`.
pub async fn tables(
    db: &DatabaseSchemaProvider,
    wrap_with_quote: Option<char>,
) -> Vec<CompletionItem> {
    db.get_tables()
        .await
        .into_iter()
        .map(|name| {
            let insert_text = match wrap_with_quote {
                Some(q) => format!("{q}{name}{q}"),
                None => name.clone(),
            };
            CompletionItem {
                label: name.clone(),
                kind: Some(CompletionItemKind::CLASS),
                // Single muted "table" badge to the right of the name. Mirrors
                // the column-item shape so the popup stays visually consistent
                // when switching between table-name and column-name positions.
                label_details: Some(CompletionItemLabelDetails {
                    detail: None,
                    description: Some("table".to_string()),
                }),
                detail: Some("table".to_string()),
                sort_text: Some(format!("1_{name}")),
                filter_text: Some(name),
                insert_text: Some(insert_text),
                ..Default::default()
            }
        })
        .collect()
}

/// Build column completions for a `BaseBuilder` chain. Reads the schema
/// directly — no model lookup, no casts, no accessors.
///
/// Returns an empty `Vec` when:
/// - The context has no `effective_table` (shouldn't happen for Base chains
///   coming out of the cursor resolver, but guarded for safety)
/// - The database schema isn't introspected yet (cold start, no DB connection,
///   or the table doesn't exist in the introspected schema)
pub async fn columns_raw(
    ctx: &ChainContext,
    db: &DatabaseSchemaProvider,
    wrap_with_quote: Option<char>,
) -> Vec<CompletionItem> {
    let Some(table) = &ctx.effective_table else {
        info!("🔗 columns_raw: ctx.effective_table is None — returning 0 items");
        return Vec::new();
    };

    let columns = db.get_columns_with_types(table).await;
    if columns.is_empty() {
        info!(
            "🔗 columns_raw: get_columns_with_types({:?}) returned 0 columns \
             (schema cache may not have this table, or DB not yet warmed)",
            table
        );
    }
    columns
        .into_iter()
        .map(|(name, php_type)| {
            let insert_text = match wrap_with_quote {
                Some(q) => format!("{q}{name}{q}"),
                None => name.clone(),
            };
            CompletionItem {
                label: name.clone(),
                kind: Some(CompletionItemKind::FIELD),
                // Use `label_details` (LSP 3.17) so the type renders right
                // next to the column name (e.g., "email   string") and the
                // source table renders as a dimmer suffix on the right
                // ("from users"). Editors that support it (Zed, VS Code)
                // render each piece distinctly; older clients fall back to
                // `detail` below, which we still set as a single-string
                // approximation.
                label_details: Some(CompletionItemLabelDetails {
                    detail: Some(format!("  {php_type}")),
                    description: Some(table.clone()),
                }),
                detail: Some(format!("{php_type} ({table})")),
                // DB columns rank first; later modes will add accessors at
                // sort_text = "2_…" so columns still rank above them.
                sort_text: Some(format!("1_{name}")),
                filter_text: Some(name),
                insert_text: Some(insert_text),
                ..Default::default()
            }
        })
        .collect()
}
