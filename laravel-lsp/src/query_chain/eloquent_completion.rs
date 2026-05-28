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
                documentation: Some(
                    CompletionDoc::new()
                        .header(format!("{}.{}", table, name))
                        .summary(format!("Database column of type `{}`.", php_type))
                        .into_documentation(),
                ),
                // DB columns rank first; later modes will add accessors at
                // sort_text = "2_…" so columns still rank above them.
                sort_text: Some(format!("1_{name}")),
                filter_text: Some(name.clone()),
                insert_text: Some(insert_text),
                ..Default::default()
            }
        })
        .collect()
}

/// Build column completions for an `EloquentBuilder` chain — a chain rooted
/// at a static call on a model class (`User::where('|')`, `User::query()->
/// where('|')`, `User::firstWhere('|')`, etc.).
///
/// Two extra hops compared to [`columns_raw`]:
/// 1. Resolve the class FQCN in `ctx.effective_model` to a model file path
///    (uses [`find_php_class_file`]) and parse it to a [`ModelMetadata`].
/// 2. Determine the table: prefer the model's `$table` property, else
///    snake-pluralize the class basename (Laravel convention).
///
/// Once the table is known, fetch DB columns and apply cast-aware PHP types
/// — if the model has a cast for a column, the cast's PHP type wins over
/// the raw SQL → PHP mapping. The cast is also surfaced in `label_details`
/// so the user can tell at a glance which columns are auto-cast.
///
/// Returns an empty `Vec` for any of the failure modes (model not found,
/// not actually an Eloquent model, table missing from DB schema, etc.) and
/// logs the cause at INFO so the LSP log is the source of truth for "why
/// did completion produce nothing here?"
pub async fn columns_for_builder(
    ctx: &ChainContext,
    db: &DatabaseSchemaProvider,
    wrap_with_quote: Option<char>,
    project_root: &Path,
) -> Vec<CompletionItem> {
    let Some(class) = &ctx.effective_model else {
        info!("🔗 columns_for_builder: ctx.effective_model is None — returning 0 items");
        return Vec::new();
    };

    // Resolve class FQCN → file path.
    let Some(path) = find_php_class_file(class, project_root) else {
        info!(
            "🔗 columns_for_builder: no PHP file found for class {:?} under {:?}",
            class, project_root
        );
        return Vec::new();
    };

    // Read + parse to ModelMetadata, walking `extends` chains so the
    // child inherits its parent's `$table` / casts / accessors /
    // relationships when it doesn't declare them itself. Runs the sync
    // walker on a blocking thread so the LSP runtime stays responsive
    // even for deep inheritance trees on slow disks.
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
                "🔗 columns_for_builder: failed to read/parse {:?} (or no inheritable parent found)",
                path
            );
            return Vec::new();
        }
        Err(err) => {
            info!("🔗 columns_for_builder: blocking task panicked: {}", err);
            return Vec::new();
        }
    };

    // Resolve the table: prefer `$table`, fall back to Laravel's
    // snake_case + pluralize convention on the class basename. Naive plural
    // rules — covers >95% of real models. Custom-pluralized models declare
    // `$table` explicitly.
    let simple_class = class.rsplit('\\').next().unwrap_or(class);
    let table = metadata
        .table_name
        .clone()
        .unwrap_or_else(|| snake_pluralize(simple_class));

    let columns = db.get_columns_with_types(&table).await;
    if columns.is_empty() {
        info!(
            "🔗 columns_for_builder: get_columns_with_types({:?}) returned 0 columns \
             (model class {:?}, resolved file {:?}, table derived from {})",
            table,
            class,
            path,
            if metadata.table_name.is_some() {
                "$table property"
            } else {
                "snake_pluralize convention"
            }
        );
    }

    columns
        .into_iter()
        .map(|(name, sql_php_type)| {
            // Cast override: if the model declares a cast for this column,
            // its PHP type wins. Example: a JSON column with
            // `'options' => 'array'` cast surfaces as `array`, not `string`.
            let (php_type, has_cast) = match metadata.casts.get(&name) {
                Some(cast) => (map_cast_to_php_type(cast), true),
                None => (sql_php_type, false),
            };
            let insert_text = match wrap_with_quote {
                Some(q) => format!("{q}{name}{q}"),
                None => name.clone(),
            };
            // Annotate cast-overridden types so the user can tell at a
            // glance which columns are model-cast vs raw DB-typed.
            let detail_suffix = if has_cast { " · cast" } else { "" };
            let summary = if has_cast {
                format!(
                    "Database column of type `{}` (overridden by model cast).",
                    php_type
                )
            } else {
                format!("Database column of type `{}`.", php_type)
            };
            CompletionItem {
                label: name.clone(),
                kind: Some(CompletionItemKind::FIELD),
                label_details: Some(CompletionItemLabelDetails {
                    detail: Some(format!("  {php_type}{detail_suffix}")),
                    description: Some(table.clone()),
                }),
                detail: Some(format!("{php_type}{detail_suffix} ({table})")),
                documentation: Some(
                    CompletionDoc::new()
                        .header(format!("{}.{}", table, name))
                        .summary(summary)
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
/// after them via sort_text ordering. Accessors are kinded as `PROPERTY`
/// to visually distinguish them from `FIELD` DB columns.
pub async fn columns_for_collection(
    ctx: &ChainContext,
    db: &DatabaseSchemaProvider,
    wrap_with_quote: Option<char>,
    project_root: &Path,
) -> Vec<CompletionItem> {
    // Start with DB columns + cast-aware types — same set the builder
    // mode returns. Collection adds to this; doesn't replace.
    let mut items = columns_for_builder(ctx, db, wrap_with_quote, project_root).await;

    // Add accessors. Read metadata again (cheap: OS cache + tokio
    // blocking pool); we don't plumb metadata through columns_for_builder's
    // return since the helper's signature is shaped for the pre-execution
    // case where accessors don't apply.
    let Some(class) = &ctx.effective_model else {
        return items;
    };
    let Some(path) = find_php_class_file(class, project_root) else {
        return items;
    };
    let path_clone = path.clone();
    let root_clone = project_root.to_path_buf();
    let metadata = match tokio::task::spawn_blocking(move || {
        ModelMetadata::from_file_with_inheritance(&path_clone, &root_clone)
    })
    .await
    {
        Ok(Some(m)) => m,
        _ => return items,
    };

    for accessor in metadata.accessors {
        let name = accessor.property_name;
        let php_type = accessor.return_type.unwrap_or_else(|| "mixed".to_string());
        let insert_text = match wrap_with_quote {
            Some(q) => format!("{q}{name}{q}"),
            None => name.clone(),
        };
        items.push(CompletionItem {
            label: name.clone(),
            // PROPERTY kind — semantically "computed property of the
            // hydrated model", distinct from FIELD (DB column).
            kind: Some(CompletionItemKind::PROPERTY),
            label_details: Some(CompletionItemLabelDetails {
                detail: Some(format!("  {php_type}")),
                description: Some("accessor".to_string()),
            }),
            detail: Some(format!("{php_type} (accessor)")),
            documentation: Some(
                CompletionDoc::new()
                    .header(&name)
                    .summary(format!("Model accessor returning `{}`.", php_type))
                    .into_documentation(),
            ),
            // Sort AFTER DB columns (which use "1_…"). When the popup
            // is fuzzy-filtered the explicit DB columns rank first.
            sort_text: Some(format!("2_{name}")),
            filter_text: Some(name.clone()),
            insert_text: Some(insert_text),
            ..Default::default()
        });
    }

    items
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
fn snake_pluralize(class_basename: &str) -> String {
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
