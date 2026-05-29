//! Build LSP `CompletionItem`s from a resolved [`ChainContext`].
//!
//! Three column-completion entry points, picked by `BuilderMode`, all funnel
//! through the shared [`assemble_columns`] multi-table assembler — they differ
//! only in how the ROOT table and its cast/accessor metadata are resolved:
//!
//! - [`columns_raw`] — `BaseBuilder` chains (`DB::table(...)` / post-`toBase()`).
//!   Schema-only root, no casts, no accessors.
//! - [`columns_for_builder`] — Eloquent builder pre-execution. Root from the
//!   model with cast-aware PHP types.
//! - [`columns_for_collection`] — Eloquent collection post-execution. Adds the
//!   model's accessors to the root's columns.
//!
//! Joined tables (issue #24) are offered as `qualifier.column` in every mode;
//! a `from()` override replaces the root, `fromRaw`/`fromSub` makes it opaque.
//!
//! [`relations`] — Eloquent-only relation list for `with()` / `whereHas()` /
//! `load()` etc.

use super::chain::*;
use crate::class_locator::find_php_class_file;
use crate::completion_format::CompletionDoc;
use crate::database::DatabaseSchemaProvider;
use crate::laravel_introspector::{map_cast_to_php_type, relationship_to_php_type, ModelMetadata};
use std::path::Path;
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
                documentation: Some(
                    CompletionDoc::new()
                        .header(&name)
                        .summary("Database table.")
                        .into_documentation(),
                ),
                sort_text: Some(format!("1_{name}")),
                filter_text: Some(name.clone()),
                insert_text: Some(insert_text),
                ..Default::default()
            }
        })
        .collect()
}

/// Per-table cast/accessor metadata for building the ROOT table's items.
/// Base-builder roots (and `from()`-replaced roots) use the empty default —
/// schema-only, no casts, no accessors. Eloquent roots fill `casts` (column →
/// raw cast name) and, in collection mode, `accessors`.
#[derive(Default)]
struct RootMeta {
    /// column name → raw cast (e.g. `"array"`), mapped to a PHP type at build.
    casts: std::collections::HashMap<String, String>,
    /// (property name, PHP return type) accessor entries — collection mode only.
    accessors: Vec<(String, String)>,
}

/// Build column completions for a `BaseBuilder` chain (`DB::table(...)` or a
/// post-`toBase()` Eloquent chain). Schema-only — no casts, no accessors.
///
/// Joined tables (issue #24) are offered as `qualifier.column`; `from()`
/// replaces the root; `fromRaw`/`fromSub` make it opaque. See
/// [`assemble_columns`] for the full multi-table behavior.
pub async fn columns_raw(
    ctx: &ChainContext,
    db: &DatabaseSchemaProvider,
    wrap_with_quote: Option<char>,
) -> Vec<CompletionItem> {
    let root = base_root_table(ctx);
    assemble_columns(
        db,
        wrap_with_quote,
        ctx.dotted_prefix.as_deref(),
        root.as_ref(),
        &RootMeta::default(),
        &ctx.joined_tables,
    )
    .await
}

/// Resolve the root (FROM) table for a base-builder chain, honoring any
/// `from*()` override. `None` means no bare root columns should be offered
/// (an opaque `fromRaw`/`fromSub`, or no resolvable table at all).
fn base_root_table(ctx: &ChainContext) -> Option<AccessibleTable> {
    match &ctx.from_clause {
        FromClause::Replace(table) => Some(table.clone()),
        FromClause::Opaque => None,
        FromClause::Inherit => ctx.effective_table.clone().map(AccessibleTable::bare),
    }
}

/// Given a chain's accessible tables and a column literal under the cursor,
/// produce the ordered `(real_table, column)` candidates to probe for
/// goto-definition (issue #24).
///
/// - **Qualified** (`orders.status`, `o.status`): resolve the qualifier
///   (alias or table name) through `accessible` to its real table; if nothing
///   matches, fall back to treating the qualifier as a literal table name
///   (covers schema-qualified or untracked tables). One candidate.
/// - **Bare** (`status`): one candidate per accessible table, in order (root
///   first), so the lookup returns the first table that defines the column.
///
/// Pure (no I/O) so the resolution order is unit-testable; the caller probes
/// each candidate against the migration index and takes the first hit.
pub fn goto_column_candidates(
    accessible: &[AccessibleTable],
    value: &str,
) -> Vec<(String, String)> {
    match value.rsplit_once('.') {
        Some((qualifier, column)) => {
            let table = accessible
                .iter()
                .find(|t| t.qualifier() == qualifier)
                .map(|t| t.table.clone())
                .unwrap_or_else(|| qualifier.to_string());
            vec![(table, column.to_string())]
        }
        None => accessible
            .iter()
            .map(|t| (t.table.clone(), value.to_string()))
            .collect(),
    }
}

/// Resolve the root table + cast/accessor metadata for an Eloquent chain,
/// honoring any `from*()` override (issue #24):
/// - `from('admins')` redirects to a plain schema-only table — the model→table
///   mapping no longer applies, so casts/accessors are dropped.
/// - `fromRaw(...)` / `fromSub(...)` (Opaque) leave no root (joined tables
///   still resolve in [`assemble_columns`]).
/// - Otherwise the root is the model's table (from `$table` or the
///   snake-pluralize convention) with its casts, plus accessors when
///   `want_accessors` (collection mode).
///
/// Returns `(None, default)` on any model-resolution failure — the caller then
/// offers only joined-table columns (or nothing), logging the cause at INFO.
async fn resolve_eloquent_root(
    ctx: &ChainContext,
    project_root: &Path,
    want_accessors: bool,
) -> (Option<AccessibleTable>, RootMeta) {
    match &ctx.from_clause {
        FromClause::Replace(table) => (Some(table.clone()), RootMeta::default()),
        FromClause::Opaque => (None, RootMeta::default()),
        FromClause::Inherit => {
            let Some(class) = &ctx.effective_model else {
                info!("🔗 resolve_eloquent_root: ctx.effective_model is None");
                return (None, RootMeta::default());
            };
            let Some(path) = find_php_class_file(class, project_root) else {
                info!(
                    "🔗 resolve_eloquent_root: no PHP file found for class {:?} under {:?}",
                    class, project_root
                );
                return (None, RootMeta::default());
            };
            // Walk `extends` chains so a child inherits its parent's
            // `$table`/casts/accessors. Runs the sync walker on a blocking
            // thread so the LSP runtime stays responsive on slow disks.
            let path_for_blocking = path.clone();
            let root_for_blocking = project_root.to_path_buf();
            let metadata = match tokio::task::spawn_blocking(move || {
                ModelMetadata::from_file_with_inheritance(&path_for_blocking, &root_for_blocking)
            })
            .await
            {
                Ok(Some(m)) => m,
                Ok(None) => {
                    info!(
                        "🔗 resolve_eloquent_root: failed to read/parse {:?} (or no inheritable parent)",
                        path
                    );
                    return (None, RootMeta::default());
                }
                Err(err) => {
                    info!("🔗 resolve_eloquent_root: blocking task panicked: {}", err);
                    return (None, RootMeta::default());
                }
            };
            let simple_class = class.rsplit('\\').next().unwrap_or(class);
            let table = metadata
                .table_name
                .clone()
                .unwrap_or_else(|| snake_pluralize(simple_class));
            let accessors = if want_accessors {
                metadata
                    .accessors
                    .iter()
                    .map(|a| {
                        (
                            a.property_name.clone(),
                            a.return_type.clone().unwrap_or_else(|| "mixed".to_string()),
                        )
                    })
                    .collect()
            } else {
                Vec::new()
            };
            (
                Some(AccessibleTable::bare(table)),
                RootMeta {
                    casts: metadata.casts,
                    accessors,
                },
            )
        }
    }
}

/// Assemble multi-table column completions, shared by every builder mode.
///
/// - **No `dotted_prefix`**: bare columns for the root table, plus
///   `qualifier.column` for every accessible table (root + joined) once joins
///   make qualification meaningful — so a join-free query stays bare-only.
///   Root accessors (collection mode) are appended bare.
/// - **`dotted_prefix = Some(qual)`** (`where('orders.|')`): narrow to the one
///   accessible table whose qualifier (alias or name) matches `qual`; columns
///   insert bare because the `qualifier.` is already typed.
///
/// `root_meta` carries cast/accessor info for the root; joined tables are
/// always schema-only (there's no model behind them).
async fn assemble_columns(
    db: &DatabaseSchemaProvider,
    wrap_with_quote: Option<char>,
    dotted_prefix: Option<&str>,
    root: Option<&AccessibleTable>,
    root_meta: &RootMeta,
    joined: &[AccessibleTable],
) -> Vec<CompletionItem> {
    // Narrowing: the user typed `qualifier.` — resolve it to one table.
    if let Some(qualifier) = dotted_prefix {
        if let Some(r) = root {
            if r.qualifier() == qualifier {
                return build_table_items(db, wrap_with_quote, r, root_meta, ColEmit::Narrow).await;
            }
        }
        if let Some(jt) = joined.iter().find(|t| t.qualifier() == qualifier) {
            return build_table_items(
                db,
                wrap_with_quote,
                jt,
                &RootMeta::default(),
                ColEmit::Narrow,
            )
            .await;
        }
        info!(
            "🔗 assemble_columns: qualifier {:?} matches no accessible table — returning 0 items",
            qualifier
        );
        return Vec::new();
    }

    let has_joins = !joined.is_empty();
    let mut items = Vec::new();
    if let Some(r) = root {
        let emit = if has_joins {
            ColEmit::BareAndQualified
        } else {
            ColEmit::BareOnly
        };
        items.extend(build_table_items(db, wrap_with_quote, r, root_meta, emit).await);
    }
    for jt in joined {
        items.extend(
            build_table_items(
                db,
                wrap_with_quote,
                jt,
                &RootMeta::default(),
                ColEmit::QualifiedOnly,
            )
            .await,
        );
    }
    if items.is_empty() {
        info!("🔗 assemble_columns: no accessible tables/columns resolved — returning 0 items");
    }
    items
}

/// Which item variants [`build_table_items`] emits for one table.
#[derive(Clone, Copy)]
enum ColEmit {
    /// Bare columns only — the narrowed `qualifier.|` case (the qualifier is
    /// already typed). Accessors are NOT included (they're not table-qualified).
    Narrow,
    /// Bare columns + accessors — the root of a join-free query.
    BareOnly,
    /// Bare + `qualifier.column` columns + accessors — the root when joins are
    /// present.
    BareAndQualified,
    /// `qualifier.column` only, no accessors — a joined table.
    QualifiedOnly,
}

/// Fetch one table's columns and emit completion items per `emit`, applying
/// `meta`'s casts (and accessors, when listing the root fully).
async fn build_table_items(
    db: &DatabaseSchemaProvider,
    wrap_with_quote: Option<char>,
    table: &AccessibleTable,
    meta: &RootMeta,
    emit: ColEmit,
) -> Vec<CompletionItem> {
    let columns = db.get_columns_with_types(&table.table).await;
    if columns.is_empty() {
        info!(
            "🔗 build_table_items: get_columns_with_types({:?}) returned 0 columns \
             (schema cache may not have this table, or DB not yet warmed)",
            table.table
        );
    }
    let mut items = Vec::new();
    for (name, sql_php_type) in &columns {
        // Cast override: if the model declares a cast for this column, its PHP
        // type wins (a JSON column with `'options' => 'array'` shows `array`).
        let (php_type, has_cast) = match meta.casts.get(name) {
            Some(cast) => (map_cast_to_php_type(cast), true),
            None => (sql_php_type.clone(), false),
        };
        match emit {
            ColEmit::Narrow | ColEmit::BareOnly => {
                items.push(col_item(
                    name,
                    &php_type,
                    &table.table,
                    None,
                    has_cast,
                    wrap_with_quote,
                    1,
                ));
            }
            ColEmit::QualifiedOnly => {
                items.push(col_item(
                    name,
                    &php_type,
                    &table.table,
                    Some(table.qualifier()),
                    has_cast,
                    wrap_with_quote,
                    2,
                ));
            }
            ColEmit::BareAndQualified => {
                items.push(col_item(
                    name,
                    &php_type,
                    &table.table,
                    None,
                    has_cast,
                    wrap_with_quote,
                    1,
                ));
                items.push(col_item(
                    name,
                    &php_type,
                    &table.table,
                    Some(table.qualifier()),
                    has_cast,
                    wrap_with_quote,
                    2,
                ));
            }
        }
    }
    // Accessors only when listing the root fully — never narrowed (not
    // table-qualified) and never for joined tables (no model behind them).
    if matches!(emit, ColEmit::BareOnly | ColEmit::BareAndQualified) {
        for (name, php_type) in &meta.accessors {
            items.push(accessor_item(name, php_type, wrap_with_quote));
        }
    }
    items
}

/// Build a column completion item.
///
/// - `qualifier = None` → bare item (`label`/`insert` = `name`).
/// - `qualifier = Some(q)` → qualified item (`label`/`insert` = `q.name`).
///
/// `has_cast` annotates the type with `· cast` when a model cast overrode the
/// raw SQL type. `sort_rank` groups items (1 = bare, 2 = qualified) so bare
/// columns surface first.
fn col_item(
    name: &str,
    php_type: &str,
    source_table: &str,
    qualifier: Option<&str>,
    has_cast: bool,
    wrap_with_quote: Option<char>,
    sort_rank: u8,
) -> CompletionItem {
    let text = match qualifier {
        Some(q) => format!("{q}.{name}"),
        None => name.to_string(),
    };
    let insert_text = match wrap_with_quote {
        Some(quote) => format!("{quote}{text}{quote}"),
        None => text.clone(),
    };
    let detail_suffix = if has_cast { " · cast" } else { "" };
    let summary = if has_cast {
        format!("Database column of type `{php_type}` (overridden by model cast).")
    } else {
        format!("Database column of type `{php_type}`.")
    };
    CompletionItem {
        label: text.clone(),
        kind: Some(CompletionItemKind::FIELD),
        // Use `label_details` (LSP 3.17) so the type renders right next to the
        // column name and the source table renders as a dimmer suffix on the
        // right. Older clients fall back to `detail`.
        label_details: Some(CompletionItemLabelDetails {
            detail: Some(format!("  {php_type}{detail_suffix}")),
            description: Some(source_table.to_string()),
        }),
        detail: Some(format!("{php_type}{detail_suffix} ({source_table})")),
        documentation: Some(
            CompletionDoc::new()
                .header(format!("{source_table}.{name}"))
                .summary(summary)
                .into_documentation(),
        ),
        sort_text: Some(format!("{sort_rank}_{text}")),
        filter_text: Some(text.clone()),
        insert_text: Some(insert_text),
        ..Default::default()
    }
}

/// Build a model-accessor completion item (collection mode). Accessors are
/// in-memory computed properties, so they're kinded `PROPERTY` and ranked
/// after DB columns (`2_`).
fn accessor_item(name: &str, php_type: &str, wrap_with_quote: Option<char>) -> CompletionItem {
    let insert_text = match wrap_with_quote {
        Some(quote) => format!("{quote}{name}{quote}"),
        None => name.to_string(),
    };
    CompletionItem {
        label: name.to_string(),
        kind: Some(CompletionItemKind::PROPERTY),
        label_details: Some(CompletionItemLabelDetails {
            detail: Some(format!("  {php_type}")),
            description: Some("accessor".to_string()),
        }),
        detail: Some(format!("{php_type} (accessor)")),
        documentation: Some(
            CompletionDoc::new()
                .header(name)
                .summary(format!("Model accessor returning `{php_type}`."))
                .into_documentation(),
        ),
        sort_text: Some(format!("2_{name}")),
        filter_text: Some(name.to_string()),
        insert_text: Some(insert_text),
        ..Default::default()
    }
}

/// Build column completions for an `EloquentBuilder` chain — a chain rooted
/// at a static call on a model class (`User::where('|')`, `User::query()->
/// where('|')`, `User::firstWhere('|')`, etc.).
///
/// The root table comes from the model (`$table` or the snake-pluralize
/// convention) with cast-aware PHP types — a column's model cast wins over the
/// raw SQL→PHP mapping and is annotated `· cast`. Joined tables (issue #24)
/// are offered as `qualifier.column` (schema-only), and a `from()` override
/// redirects the root; see [`resolve_eloquent_root`] and [`assemble_columns`].
///
/// Returns an empty `Vec` for failure modes (model not found, table missing
/// from the DB schema, …) when there are no joined tables to fall back on;
/// the cause is logged at INFO.
pub async fn columns_for_builder(
    ctx: &ChainContext,
    db: &DatabaseSchemaProvider,
    wrap_with_quote: Option<char>,
    project_root: &Path,
) -> Vec<CompletionItem> {
    let (root, meta) = resolve_eloquent_root(ctx, project_root, false).await;
    assemble_columns(
        db,
        wrap_with_quote,
        ctx.dotted_prefix.as_deref(),
        root.as_ref(),
        &meta,
        &ctx.joined_tables,
    )
    .await
}

/// Build relation completions for an `EloquentBuilder` chain — the first
/// string arg of methods like `with('|')`, `whereHas('|', closure)`,
/// `load('|')`, `withCount('|')`, etc.
///
/// Reads the model's `ModelMetadata::relationships` (already extracted by
/// the existing model analyzer — same source the property completion path
/// uses) and surfaces one item per relationship method, with a
/// Laravel-aware return type like `HasMany<Post>` or `BelongsTo<User>` in
/// the popup detail. Items use `CompletionItemKind::REFERENCE` so the
/// icon matches what users expect for "method that returns something
/// related."
///
/// Returns an empty `Vec` when the model file isn't found or the model
/// has no relationships. Same failure-mode pattern as
/// [`columns_for_builder`]: log INFO and yield empty so the LSP log is
/// the source of truth.
///
/// Base-builder chains (`DB::table(...)`) never reach here — the handler
/// dispatch returns empty for `(BaseBuilder, Relation)`. That's
/// load-bearing: Query Builder doesn't have relation methods, so a `with`
/// on a `DB::table()` chain is user error and we shouldn't pretend
/// otherwise by listing relations.
pub async fn relations(
    ctx: &ChainContext,
    wrap_with_quote: Option<char>,
    project_root: &Path,
) -> Vec<CompletionItem> {
    let Some(starting_class) = &ctx.effective_model else {
        info!("🔗 relations: ctx.effective_model is None — returning 0 items");
        return Vec::new();
    };

    // Phase 7: if the user is typing a dotted relation path like
    // `with('posts.author.|')`, walk each hop on the starting model to
    // arrive at the final model whose relations we'll list. The cursor
    // resolver populates `dotted_prefix` with everything before the last
    // `.`; the editor handles fuzzy-filtering the part after.
    let class = if let Some(prefix) = ctx.dotted_prefix.as_deref() {
        match walk_dotted_hops(starting_class, prefix, project_root).await {
            Some(resolved) => {
                if resolved != *starting_class {
                    info!(
                        "🔗 relations: dotted-path hop {:?} on {:?} → {:?}",
                        prefix, starting_class, resolved
                    );
                }
                resolved
            }
            None => {
                info!(
                    "🔗 relations: dotted-path hop {:?} failed on {:?} \
                     (segment doesn't resolve to a known relation)",
                    prefix, starting_class
                );
                return Vec::new();
            }
        }
    } else {
        starting_class.clone()
    };

    let Some(path) = find_php_class_file(&class, project_root) else {
        info!(
            "🔗 relations: no PHP file found for class {:?} under {:?}",
            class, project_root
        );
        return Vec::new();
    };

    // Walk `extends` chain so child classes pick up relationships
    // declared on their parents.
    let path_for_blocking = path.clone();
    let root_for_blocking = project_root.to_path_buf();
    let metadata = match tokio::task::spawn_blocking(move || {
        ModelMetadata::from_file_with_inheritance(&path_for_blocking, &root_for_blocking)
    })
    .await
    {
        Ok(Some(m)) => m,
        Ok(None) => {
            info!("🔗 relations: failed to read/parse {:?}", path);
            return Vec::new();
        }
        Err(err) => {
            info!("🔗 relations: blocking task panicked: {}", err);
            return Vec::new();
        }
    };

    if metadata.relationships.is_empty() {
        info!(
            "🔗 relations: model {:?} (file {:?}) has no relationships extracted",
            class, path
        );
    }

    metadata
        .relationships
        .into_iter()
        .map(|rel| {
            let php_type =
                relationship_to_php_type(&rel.relationship_type, rel.related_model.as_deref());
            let name = rel.method_name;
            let insert_text = match wrap_with_quote {
                Some(q) => format!("{q}{name}{q}"),
                None => name.clone(),
            };
            let summary = match rel.related_model.as_deref() {
                Some(related) => format!(
                    "Eloquent `{}` relationship to `{}`.",
                    rel.relationship_type, related
                ),
                None => format!("Eloquent `{}` relationship.", rel.relationship_type),
            };
            CompletionItem {
                label: name.clone(),
                // REFERENCE icon — semantically "this points to another
                // entity," which matches relationships better than FIELD
                // (column) or CLASS (table).
                kind: Some(CompletionItemKind::REFERENCE),
                label_details: Some(CompletionItemLabelDetails {
                    detail: Some(format!("  {php_type}")),
                    description: Some(rel.relationship_type.clone()),
                }),
                detail: Some(format!("{php_type} ({})", rel.relationship_type)),
                documentation: Some(
                    CompletionDoc::new()
                        .header(&name)
                        .summary(summary)
                        .into_documentation(),
                ),
                // Same `1_` prefix as columns so when both are surfaced
                // (different ArgKind branches in the handler, so they
                // shouldn't collide in practice) they rank together.
                sort_text: Some(format!("1_{name}")),
                filter_text: Some(name.clone()),
                insert_text: Some(insert_text),
                ..Default::default()
            }
        })
        .collect()
}

/// Phase 7 helper: walk a dotted relation path, returning the model
/// class at the FINAL hop. Each segment must resolve as a relation on
/// the previous segment's model.
///
/// Examples:
/// - `walk_dotted_hops("User", "posts", root)` → `Some("Post")` (one hop)
/// - `walk_dotted_hops("User", "posts.author", root)` → `Some("Author")`
/// - `walk_dotted_hops("User", "posts.author.profile", root)` → `Some("Profile")`
/// - Any segment that fails to resolve → `None`
///
/// Returns the starting model unchanged if `dotted_prefix` is empty.
pub async fn walk_dotted_hops(
    starting_model: &str,
    dotted_prefix: &str,
    project_root: &Path,
) -> Option<String> {
    if dotted_prefix.is_empty() {
        return Some(starting_model.to_string());
    }
    let mut current = starting_model.to_string();
    for segment in dotted_prefix.split('.') {
        if segment.is_empty() {
            // Empty segment means the user typed two consecutive dots
            // (e.g. `with('posts..|')`). Treat as a no-op — keep the
            // current model, don't try to resolve "".
            continue;
        }
        current = resolve_related_model(&current, segment, project_root).await?;
    }
    Some(current)
}

/// Phase 8 helper: walk one relation hop. Given a parent class name and
/// a relation name, find the parent's model file, read its
/// `ModelMetadata::relationships`, and return the related model class.
///
/// Used when the cursor is inside a relation closure like
/// `OAuthClient::with(['tokens' => fn ($q) => $q->where('|')])` — we know
/// the parent model (`OAuthClient`) and the relation name (`tokens`), and
/// need the related model (e.g. `OAuthToken`) so column/relation
/// completion runs against the correct class.
///
/// Returns `None` when the parent file can't be located, the
/// relation isn't defined on the parent, or the relation has no
/// resolvable `related_model` (e.g. a polymorphic `morphTo`).
pub async fn resolve_related_model(
    parent_class: &str,
    relation_name: &str,
    project_root: &Path,
) -> Option<String> {
    let path = find_php_class_file(parent_class, project_root)?;
    // Inheritance-aware: a relationship declared on the parent class
    // should be findable when the chain is rooted at a child class.
    let path_clone = path.clone();
    let root_clone = project_root.to_path_buf();
    let metadata = tokio::task::spawn_blocking(move || {
        ModelMetadata::from_file_with_inheritance(&path_clone, &root_clone)
    })
    .await
    .ok()
    .flatten()?;
    let rel = metadata
        .relationships
        .into_iter()
        .find(|r| r.method_name == relation_name)?;
    rel.related_model
}

/// Phase 6 helper: resolve a model class to its table name.
///
/// Used for the post-`->toBase()` case where the chain has flipped to
/// `BuilderMode::BaseBuilder` but `effective_table` is still `None` —
/// we know the model, we just haven't asked which table it points at.
///
/// Reads `ModelMetadata` (with inheritance walking, so subclasses of a
/// vendor model inherit the parent's `$table`), then either returns the
/// explicit `$table` or falls back to snake_pluralize of the class
/// basename.
pub async fn resolve_table_for_model(class: &str, project_root: &Path) -> Option<String> {
    let path = find_php_class_file(class, project_root)?;
    let path_clone = path.clone();
    let root_clone = project_root.to_path_buf();
    let metadata = tokio::task::spawn_blocking(move || {
        ModelMetadata::from_file_with_inheritance(&path_clone, &root_clone)
    })
    .await
    .ok()
    .flatten()?;
    let simple_class = class.rsplit('\\').next().unwrap_or(class);
    Some(
        metadata
            .table_name
            .unwrap_or_else(|| snake_pluralize(simple_class)),
    )
}

/// Phase 6: column completion for an `EloquentCollection` chain — a
/// chain that's been executed via `->get()` / `->all()` / `->pluck()` /
/// `->cursor()` etc. After execution, the result is a hydrated
/// `Collection<Model>` and Collection's `where()` filters in MEMORY
/// against model property access — not SQL. That means accessors
/// (`getFullNameAttribute`, `Attribute::make(get:)`) are valid `where()`
/// args even though they don't exist as DB columns.
///
/// Returns DB columns (with cast-aware types) FIRST, then accessor items
/// after them via sort_text ordering (`1_` vs `2_`). Joined-table columns
/// (issue #24) are offered as `qualifier.column`, same as builder mode.
pub async fn columns_for_collection(
    ctx: &ChainContext,
    db: &DatabaseSchemaProvider,
    wrap_with_quote: Option<char>,
    project_root: &Path,
) -> Vec<CompletionItem> {
    // `want_accessors = true`: the root's accessors join its DB columns in the
    // assembled set (they're valid in-memory `where()` args post-execution).
    let (root, meta) = resolve_eloquent_root(ctx, project_root, true).await;
    assemble_columns(
        db,
        wrap_with_quote,
        ctx.dotted_prefix.as_deref(),
        root.as_ref(),
        &meta,
        &ctx.joined_tables,
    )
    .await
}

/// Convert a PascalCase class basename to Laravel's default table name:
/// snake_case + naive pluralization. Models with non-standard pluralization
/// (people, octopi, child → children) declare `$table` explicitly; this
/// fallback only covers the convention case.
///
/// Examples:
///   `User` → `users`
///   `BlogPost` → `blog_posts`
///   `Category` → `categories`
///   `Address` → `addresses`
pub fn snake_pluralize(class_basename: &str) -> String {
    // PascalCase → snake_case.
    let mut snake = String::with_capacity(class_basename.len() + 4);
    for (i, c) in class_basename.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            snake.push('_');
        }
        snake.extend(c.to_lowercase());
    }
    // Naive English pluralization rules — match Laravel's
    // `Str::plural` behavior for the common cases.
    if snake.ends_with('y')
        && !snake
            .chars()
            .nth_back(1)
            .map(|c| "aeiou".contains(c))
            .unwrap_or(false)
    {
        // consonant + y → ies (category → categories, but NOT day → daies)
        snake.pop();
        snake.push_str("ies");
    } else if snake.ends_with('s')
        || snake.ends_with('x')
        || snake.ends_with('z')
        || snake.ends_with("ch")
        || snake.ends_with("sh")
    {
        snake.push_str("es");
    } else {
        snake.push('s');
    }
    snake
}

#[cfg(test)]
mod tests;
