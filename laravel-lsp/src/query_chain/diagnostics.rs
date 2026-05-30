//! Whole-file validation of Eloquent / DB query-builder chains.
//!
//! Completion answers "what is valid at the cursor"; diagnostics answer the
//! mirror question — "is what's already written valid?". We reuse the exact
//! same resolution pipeline as completion ([`detect_chain_context_at`] plus
//! the async model/relation hops) so the two features can never disagree: if
//! completion would have offered an identifier, diagnostics won't flag it.
//!
//! # Guiding principle: under-warn, never over-warn
//!
//! A false positive (squiggle on valid code) is worse than a false negative
//! (missed typo) — it trains users to ignore the LSP. Every ambiguity resolves
//! toward *staying quiet*:
//!
//! - **Schema not loaded** (no DB connection, table absent) → the legal set is
//!   empty → skip. We only flag when we have a confident, non-empty set to
//!   check against.
//! - **Receiver unresolved** (`$x->where(...)` with no known type) → the
//!   resolver returns `None` → skip.
//! - **Non-identifier literals** — qualified columns (`users.id`), expressions
//!   (`count(*)`), aliases (`name as n`), wildcards (`*`) — are skipped via the
//!   [`is_simple_identifier`] guard. A bare typo like `emial` is what we catch.
//! - **`having`** is not diagnosed: it filters on aggregate aliases where a
//!   bare identifier is routinely not a real column. (`select`/`addSelect` ARE
//!   diagnosed — the identifier guard skips their alias/qualified forms.)
//! - **Array-form args** are diagnosed for list methods: `with(['a', 'b'])`,
//!   `select(['x', 'y'])`, and keyed relations `with(['rel' => $closure])`. But
//!   `where`-family arrays are NOT descended — they hold values / `col => value`
//!   pairs, and validating the value side would false-positive.
//!
//! Raw-SQL methods (`whereRaw`, `selectRaw`, …) never reach here: the method
//! classifier assigns them [`ArgKind::None`], so their links carry no
//! diagnosable arg.
//!
//! # Dynamic `where{Column}` finders
//!
//! Eloquent's magic `whereEmail($v)` / `orWhereName($v)` (and `And`/`Or`
//! composites like `whereFirstNameAndEmail`) carry their column in the *method
//! name*, not an argument. These are validated too — but only after ruling out
//! real builder methods ([`KNOWN_WHERE_METHODS`]) and the model's local
//! `scope{Name}` methods (a `scopeWhereActive` makes `whereActive()` a scope
//! call, not a column probe). Collections are skipped — they have no dynamic
//! `where{Column}`.

use super::chain::*;
use super::cursor::{byte_offset_to_position, chain_context_for_link};
use super::eloquent_completion::{
    enrich_join_parent_tables, resolve_related_model, resolve_table_for_model, snake_pluralize,
    walk_dotted_hops,
};
use crate::class_locator::find_php_class_file;
use crate::database::DatabaseSchemaProvider;
use crate::laravel_introspector::{
    analyze, pascal_to_snake, snake_to_studly, ClassView, ModelMetadata,
};
use std::path::Path;
use std::sync::Arc;
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString, Range};

/// Column methods whose first string arg we deliberately DON'T validate.
/// `having` filters on aggregate *aliases* (`having('total', '>', 5)` after a
/// `selectRaw('count(*) as total')`), where a bare simple identifier is
/// routinely not a real column — validating it would false-positive.
///
/// `select`/`addSelect` are NOT denied: their alias/qualified/expression forms
/// (`'votes as score'`, `'users.id'`, `'count(*)'`) are already skipped by the
/// [`is_simple_identifier`] guard, so only a bare typo like `select('emial')`
/// is flagged — which is exactly what we want.
const COLUMN_DIAG_DENY: &[&str] = &["having"];

/// Diagnostic codes — stable strings the code-action handler keys off to offer
/// the matching quick-fix. Kept here so the producer and the consumer share a
/// single source of truth.
pub const CODE_UNKNOWN_COLUMN: &str = "laravel-lsp.unknown-column";
pub const CODE_UNKNOWN_RELATION: &str = "laravel-lsp.unknown-relation";
pub const CODE_UNKNOWN_TABLE: &str = "laravel-lsp.unknown-table";
/// A bare column that exists on more than one accessible table (issue #24) —
/// the query would be ambiguous at runtime, so the user must qualify it.
pub const CODE_AMBIGUOUS_COLUMN: &str = "laravel-lsp.ambiguous-column";

/// What kind of identifier a link's first string arg names. Derived from the
/// link's `ArgKind`, collapsed to the three things we can validate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiagKind {
    Column,
    Relation,
    Table,
}

/// Validate every diagnosable chain in `chains` against the resolved schema,
/// returning one [`Diagnostic`] per unknown column / relation / table.
///
/// `content` is the file source the chains' byte spans index into (used to
/// turn byte spans into LSP `Range`s). `severity` is the configured severity
/// for these diagnostics — the caller skips the call entirely when the feature
/// is turned off.
pub async fn chain_diagnostics(
    chains: &[Arc<BuilderChain>],
    db: &DatabaseSchemaProvider,
    project_root: &Path,
    content: &str,
    severity: DiagnosticSeverity,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();

    for chain in chains {
        for (idx, link) in chain.links.iter().enumerate() {
            // Dynamic `where{Column}` / `orWhere{Column}` finders carry their
            // column in the METHOD NAME, not a string arg — handle them first.
            // A finder link has no diagnosable string arg, so on a hit we move
            // straight to the next link.
            if let Some(diag) = dynamic_where_diagnostic(
                chains,
                chain,
                idx,
                link,
                db,
                project_root,
                content,
                severity,
            )
            .await
            {
                out.push(diag);
                continue;
            }

            // Which kind of identifier (if any) does this link's first string
            // arg name? `ArgKind::None` links (terminators, raw SQL, …) and
            // alias-defining column methods are skipped here.
            let kind = match link.arg {
                ArgKind::Column => {
                    if COLUMN_DIAG_DENY.contains(&link.method.as_str()) {
                        continue;
                    }
                    DiagKind::Column
                }
                ArgKind::Relation | ArgKind::ClosureCarrier => DiagKind::Relation,
                ArgKind::Table => DiagKind::Table,
                ArgKind::None => continue,
            };

            // The literal(s) to validate. Most methods validate a single
            // leading string; list methods (`select`, `with(['a','b'])`, …)
            // validate every array element too. `where`-family arrays are
            // values/`col => value` pairs, so they're never descended (the
            // value side would false-positive).
            let targets = collect_targets(link, kind);
            if targets.is_empty() {
                continue;
            }

            // Resolve the context for THIS link by index — receiver type plus
            // the effects of preceding links. This lints each method call on
            // its own; it does NOT depend on the chain being "finished" (a
            // terminator like `->get()`), nor on cursor/completion semantics.
            let Some(base_ctx) = chain_context_for_link(chains, chain, idx) else {
                continue;
            };
            let Some(base_ctx) = finalize_context(base_ctx, project_root).await else {
                continue;
            };

            match kind {
                // Columns: validate against the per-table column sets of every
                // accessible table, so we can tell unknown (on none) from
                // ambiguous (on more than one, issue #24) apart.
                DiagKind::Column => {
                    let Some((tables, extras)) =
                        accessible_column_sets(&base_ctx, db, project_root).await
                    else {
                        continue; // resolution gap / schema not loaded / virtual — stay quiet
                    };
                    for (value, span) in targets {
                        if !is_simple_identifier(&value) {
                            continue; // qualified / expression / aliased / wildcard
                        }
                        let hits: Vec<&str> = tables
                            .iter()
                            .filter(|(_, cols)| cols.iter().any(|c| c == &value))
                            .map(|(name, _)| name.as_str())
                            .collect();
                        match hits.len() {
                            // On exactly one table — unambiguous, valid.
                            1 => {}
                            // On more than one — ambiguous; must be qualified.
                            n if n > 1 => out.push(ambiguous_column_diagnostic(
                                &value, &hits, span, content, severity,
                            )),
                            // On none — valid only if it's a model accessor;
                            // otherwise an unknown column.
                            _ => {
                                if extras.iter().any(|e| e == &value) {
                                    continue;
                                }
                                let union: Vec<String> = tables
                                    .iter()
                                    .flat_map(|(_, cols)| cols.iter().cloned())
                                    .chain(extras.iter().cloned())
                                    .collect();
                                let subject = tables
                                    .first()
                                    .map(|(name, _)| name.clone())
                                    .unwrap_or_default();
                                let suggestion = best_suggestion(&value, &union);
                                out.push(make_diagnostic(
                                    DiagKind::Column,
                                    &value,
                                    &subject,
                                    suggestion,
                                    span,
                                    &value,
                                    content,
                                    severity,
                                ));
                            }
                        }
                    }
                }
                // Tables resolve a single legal set across all targets.
                DiagKind::Table => {
                    let Some((legal, subject)) =
                        legal_names(DiagKind::Table, &base_ctx, db, project_root).await
                    else {
                        continue; // resolution gap / schema not loaded — stay quiet
                    };
                    for (value, span) in targets {
                        if !is_simple_identifier(&value) {
                            continue;
                        }
                        if legal.iter().any(|name| name == &value) {
                            continue;
                        }
                        let suggestion = best_suggestion(&value, &legal);
                        out.push(make_diagnostic(
                            DiagKind::Table,
                            &value,
                            &subject,
                            suggestion,
                            span,
                            &value,
                            content,
                            severity,
                        ));
                    }
                }
                // Relations can be dotted per element (`with(['posts.author',
                // 'comments'])`), so the legal set is resolved per target.
                DiagKind::Relation => {
                    for (value, span) in targets {
                        let needle = value.rsplit('.').next().unwrap_or(&value);
                        if !is_simple_identifier(needle) {
                            continue;
                        }
                        let mut ctx = base_ctx.clone();
                        ctx.dotted_prefix = value.rfind('.').map(|i| value[..i].to_string());
                        let Some((legal, subject)) =
                            legal_names(DiagKind::Relation, &ctx, db, project_root).await
                        else {
                            continue;
                        };
                        if legal.iter().any(|name| name == needle) {
                            continue;
                        }
                        let suggestion = best_suggestion(needle, &legal);
                        out.push(make_diagnostic(
                            DiagKind::Relation,
                            needle,
                            &subject,
                            suggestion,
                            span,
                            &value,
                            content,
                            severity,
                        ));
                    }
                }
            }
        }
    }

    out
}

/// Apply the async resolution hops completion does before dispatch: a relation
/// closure hop (`whereHas('rel', fn ($q) => $q->where('|'))` resolves `$q` to
/// the related model) and post-`toBase()` table resolution. Returns `None`
/// when a required hop can't be resolved — the caller then stays quiet.
async fn finalize_context(mut ctx: ChainContext, root: &Path) -> Option<ChainContext> {
    if let Some(rel) = ctx.closure_relation_hop.take() {
        let parent = ctx.effective_model.clone()?;
        let related = resolve_related_model(&parent, &rel, root).await?;
        ctx.effective_model = Some(related);
    }
    if ctx.mode == BuilderMode::BaseBuilder && ctx.effective_table.is_none() {
        if let Some(model) = ctx.effective_model.clone() {
            ctx.effective_table = resolve_table_for_model(&model, root).await;
        }
    }
    // Join-closure parent table (issue #24): fold an Eloquent parent's table
    // into the accessible set so a bare parent column inside the closure isn't
    // false-flagged.
    enrich_join_parent_tables(&mut ctx, root).await;
    Some(ctx)
}

/// Build the set of legal names for `kind` in `ctx`, plus the human-readable
/// subject for the diagnostic message. Returns `None` whenever the set can't be
/// computed confidently (no table/model, schema not loaded, model has no
/// relations) — every such case means "don't flag".
async fn legal_names(
    kind: DiagKind,
    ctx: &ChainContext,
    db: &DatabaseSchemaProvider,
    root: &Path,
) -> Option<(Vec<String>, String)> {
    match kind {
        DiagKind::Table => {
            let tables = db.get_tables().await;
            if tables.is_empty() {
                return None; // schema not introspected — stay quiet
            }
            Some((tables, String::new()))
        }
        // Columns are validated via `accessible_column_sets` (it needs
        // per-table sets to distinguish ambiguous from unknown), never here.
        DiagKind::Column => unreachable!("columns use accessible_column_sets"),
        DiagKind::Relation => {
            // Relations only exist on Eloquent builders/collections. A relation
            // method on a `DB::table()` base builder is user error, but flagging
            // it is out of scope — stay quiet.
            if ctx.mode == BuilderMode::BaseBuilder {
                return None;
            }
            let starting = ctx.effective_model.as_deref()?;
            // Walk any dotted prefix to the final model whose relations we list.
            let model = match ctx.dotted_prefix.as_deref() {
                Some(prefix) => walk_dotted_hops(starting, prefix, root).await?,
                None => starting.to_string(),
            };
            let meta = load_metadata(&model, root).await?;
            let names: Vec<String> = meta
                .relationships
                .into_iter()
                .map(|rel| rel.method_name)
                .collect();
            if names.is_empty() {
                // No relations extracted — can't confidently flag a typo.
                return None;
            }
            Some((names, model))
        }
    }
}

/// Per-table DB column sets for every table a chain can reference, plus
/// root-bound extras (model accessors). Used for ambiguity-aware column
/// validation: a bare column on >1 table is ambiguous, on exactly 1 is valid,
/// on 0 (and not an accessor) is unknown (issue #24).
///
/// Returns `None` — stay quiet — when nothing resolves confidently: no
/// accessible table has columns, the schema isn't loaded, or a virtual
/// (subquery) table is in scope (its best-effort columns shouldn't drive
/// ambiguity). Each entry is `(table_name, db_column_names)`; the root entry's
/// columns include the model's cast keys (which are columns of the root).
async fn accessible_column_sets(
    ctx: &ChainContext,
    db: &DatabaseSchemaProvider,
    root: &Path,
) -> Option<(Vec<(String, Vec<String>)>, Vec<String>)> {
    // Virtual (subquery) tables carry best-effort SELECT columns — don't let
    // them drive ambiguity/unknown decisions; stay quiet entirely.
    let has_virtual = matches!(&ctx.from_clause, FromClause::Replace(t) if t.virtual_columns.is_some())
        || ctx
            .joined_tables
            .iter()
            .any(|t| t.virtual_columns.is_some());
    if has_virtual {
        return None;
    }

    // Resolve the root (FROM) table plus, for Eloquent chains, the model's
    // casts (columns whose type is overridden) and accessors. Mirrors
    // `eloquent_completion::resolve_eloquent_root`. The root is an
    // `AccessibleTable` so its DB lookup uses the real name while the reported
    // identifier (and qualify-fix) uses the qualifier (alias if aliased).
    let (root_at, casts, accessors): (Option<AccessibleTable>, Vec<String>, Vec<String>) =
        match &ctx.from_clause {
            FromClause::Replace(at) => (Some(at.clone()), Vec::new(), Vec::new()),
            FromClause::Opaque => (None, Vec::new(), Vec::new()),
            FromClause::Inherit => match ctx.mode {
                BuilderMode::BaseBuilder => (
                    ctx.effective_table.clone().map(AccessibleTable::bare),
                    Vec::new(),
                    Vec::new(),
                ),
                BuilderMode::EloquentBuilder | BuilderMode::EloquentCollection => {
                    let model = ctx.effective_model.as_deref()?;
                    let meta = load_metadata(model, root).await?;
                    let simple = model.rsplit('\\').next().unwrap_or(model);
                    let table = meta
                        .table_name
                        .clone()
                        .unwrap_or_else(|| snake_pluralize(simple));
                    let casts: Vec<String> = meta.casts.keys().cloned().collect();
                    let accessors: Vec<String> = if ctx.mode == BuilderMode::EloquentCollection {
                        meta.accessors
                            .iter()
                            .map(|a| a.property_name.clone())
                            .collect()
                    } else {
                        Vec::new()
                    };
                    (Some(AccessibleTable::bare(table)), casts, accessors)
                }
            },
        };

    // Each entry's identifier is the qualifier the user references (alias if
    // present), used for the ambiguity message and the qualify quick-fix; the
    // DB lookup uses the real table name.
    let mut tables: Vec<(String, Vec<String>)> = Vec::new();
    if let Some(at) = root_at {
        let mut cols: Vec<String> = db
            .get_columns_with_types(&at.table)
            .await
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        if !cols.is_empty() {
            cols.extend(casts); // cast keys are columns of the root table
            tables.push((at.qualifier().to_string(), cols));
        }
    }
    for jt in &ctx.joined_tables {
        let cols: Vec<String> = db
            .get_columns_with_types(&jt.table)
            .await
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        if !cols.is_empty() {
            tables.push((jt.qualifier().to_string(), cols));
        }
    }

    if tables.is_empty() {
        return None; // no accessible table resolved any columns — can't validate
    }
    Some((tables, accessors))
}

/// Read + parse a model class to [`ModelMetadata`], walking its `extends`
/// chain. Mirrors the blocking-pool pattern the completion helpers use so the
/// LSP runtime stays responsive on slow disks / deep inheritance.
async fn load_metadata(class: &str, root: &Path) -> Option<ModelMetadata> {
    let path = find_php_class_file(class, root)?;
    let path_c = path.clone();
    let root_c = root.to_path_buf();
    tokio::task::spawn_blocking(move || ModelMetadata::from_file_with_inheritance(&path_c, &root_c))
        .await
        .ok()
        .flatten()
}

/// Read + parse a model class to a [`ClassView`] (inheritance- and
/// trait-aware). Used by the dynamic-where path, which needs the model's local
/// `scopes` (to avoid flagging a scope call) alongside its table.
async fn load_class_view(class: &str, root: &Path) -> Option<ClassView> {
    let path = find_php_class_file(class, root)?;
    let path_c = path.clone();
    let root_c = root.to_path_buf();
    tokio::task::spawn_blocking(move || analyze(&path_c, &root_c))
        .await
        .ok()
        .flatten()
}

// ---- Dynamic `where{Column}` finders --------------------------------------

/// Real `where*` builder methods — NOT dynamic finders. A `where`/`orWhere`-
/// prefixed method that isn't in this set (and isn't a local scope) is treated
/// as a dynamic `where{Column}` finder. Kept deliberately broad: a real method
/// mistaken for a finder would be a false positive, which we never want.
const KNOWN_WHERE_METHODS: &[&str] = &[
    "where",
    "orWhere",
    "whereIn",
    "orWhereIn",
    "whereNotIn",
    "orWhereNotIn",
    "whereNull",
    "orWhereNull",
    "whereNotNull",
    "orWhereNotNull",
    "whereBetween",
    "orWhereBetween",
    "whereNotBetween",
    "orWhereNotBetween",
    "whereBetweenColumns",
    "whereNotBetweenColumns",
    "whereColumn",
    "orWhereColumn",
    "whereDate",
    "orWhereDate",
    "whereMonth",
    "orWhereMonth",
    "whereDay",
    "orWhereDay",
    "whereYear",
    "orWhereYear",
    "whereTime",
    "orWhereTime",
    "whereExists",
    "orWhereExists",
    "whereNotExists",
    "orWhereNotExists",
    "whereHas",
    "orWhereHas",
    "whereDoesntHave",
    "orWhereDoesntHave",
    "whereHasMorph",
    "orWhereHasMorph",
    "whereDoesntHaveMorph",
    "whereRaw",
    "orWhereRaw",
    "whereJsonContains",
    "orWhereJsonContains",
    "whereJsonDoesntContain",
    "whereJsonContainsKey",
    "whereJsonDoesntContainKey",
    "whereJsonLength",
    "orWhereJsonLength",
    "whereFullText",
    "orWhereFullText",
    "whereBelongsTo",
    "orWhereBelongsTo",
    "whereRelation",
    "orWhereRelation",
    "whereMorphRelation",
    "orWhereMorphRelation",
    "whereKey",
    "whereKeyNot",
    "whereNot",
    "orWhereNot",
    "whereInRaw",
    "orWhereInRaw",
    "whereIntegerInRaw",
    "whereIntegerNotInRaw",
    "whereNotInRaw",
    "orWhereNotInRaw",
    "whereMorphedTo",
    "orWhereMorphedTo",
    "whereDescendantOf",
    "whereAncestorOf",
    "whereAll",
    "orWhereAll",
    "whereAny",
    "orWhereAny",
    "whereNone",
    "orWhereNone",
    "wherePivot",
    "wherePivotIn",
    "wherePivotNotIn",
    "wherePivotBetween",
    "wherePivotNotBetween",
    "wherePivotNull",
    "wherePivotNotNull",
];

/// If `method` is a dynamic `where{Column}` / `orWhere{Column}` finder, return
/// `(prefix, finder)` where `prefix` is `"where"` or `"orWhere"` and `finder`
/// is the studly column portion. Returns `None` for real builder methods and
/// for `where`/`orWhere` themselves (no studly suffix).
fn dynamic_where_finder(method: &str) -> Option<(&'static str, &str)> {
    if KNOWN_WHERE_METHODS.contains(&method) {
        return None;
    }
    // Try the longer prefix first so `orWhereName` strips to `Name`, not
    // `eName` via a `where` mismatch (it doesn't start with `where` anyway,
    // but explicit ordering keeps the intent clear).
    for prefix in ["orWhere", "where"] {
        if let Some(rest) = method.strip_prefix(prefix) {
            // A real finder has an uppercase studly column right after the
            // prefix (`whereEmail`). `whereabouts` (lowercase) isn't a finder.
            if rest
                .chars()
                .next()
                .map(|c| c.is_ascii_uppercase())
                .unwrap_or(false)
            {
                return Some((prefix, rest));
            }
        }
    }
    None
}

/// Split a dynamic finder's studly portion into column segments, mirroring
/// Laravel's `Builder::dynamicWhere` (`preg_split('/(And|Or)(?=[A-Z])/')`):
/// split on `And`/`Or` only when immediately followed by an uppercase letter,
/// so `FirstNameAndEmail` → `["FirstName", "Email"]` while `Brand` stays whole
/// (lowercase `and`) — and a leading separator never splits.
fn split_dynamic_segments(finder: &str) -> Vec<&str> {
    let bytes = finder.as_bytes();
    let mut segments = Vec::new();
    let mut seg_start = 0usize;
    let mut i = 0usize;
    while i < finder.len() {
        let kw_len = if finder[i..].starts_with("And") {
            Some(3)
        } else if finder[i..].starts_with("Or") {
            Some(2)
        } else {
            None
        };
        if let Some(klen) = kw_len {
            let next_is_upper = bytes
                .get(i + klen)
                .map(u8::is_ascii_uppercase)
                .unwrap_or(false);
            if i > seg_start && next_is_upper {
                segments.push(&finder[seg_start..i]);
                seg_start = i + klen;
                i = seg_start;
                continue;
            }
        }
        i += 1;
    }
    if seg_start < finder.len() {
        segments.push(&finder[seg_start..]);
    }
    segments
}

/// Validate a dynamic `where{Column}` finder link, returning a diagnostic for
/// the first segment that doesn't resolve to a real column. Returns `None` for
/// non-finders, local scopes, collection chains, unresolved receivers, and a
/// cold/absent schema — every ambiguity stays quiet.
#[allow(clippy::too_many_arguments)]
async fn dynamic_where_diagnostic(
    chains: &[Arc<BuilderChain>],
    chain: &BuilderChain,
    link_idx: usize,
    link: &ChainLink,
    db: &DatabaseSchemaProvider,
    root: &Path,
    content: &str,
    severity: DiagnosticSeverity,
) -> Option<Diagnostic> {
    let (prefix, finder) = dynamic_where_finder(&link.method)?;

    let ctx = chain_context_for_link(chains, chain, link_idx)?;
    let ctx = finalize_context(ctx, root).await?;

    // Resolve the table to validate against. For Eloquent we also load the
    // model's scopes: a scope whose callable name equals the method (e.g.
    // `scopeWhereActive` → `whereActive`) means this is a scope call, not a
    // dynamic column finder — stay quiet.
    let table = match ctx.mode {
        BuilderMode::BaseBuilder => ctx.effective_table.clone()?,
        // Collections don't have dynamic `where{Column}` — `whereEmail` on a
        // hydrated collection is just an undefined method, not a column probe.
        BuilderMode::EloquentCollection => return None,
        BuilderMode::EloquentBuilder => {
            let model = ctx.effective_model.as_deref()?;
            let view = load_class_view(model, root).await?;
            if view.scopes.iter().any(|scope| scope.name == link.method) {
                return None; // it's a local scope, not a dynamic finder
            }
            let simple = model.rsplit('\\').next().unwrap_or(model);
            view.table_name
                .clone()
                .unwrap_or_else(|| snake_pluralize(simple))
        }
    };

    let columns: Vec<String> = db
        .get_columns_with_types(&table)
        .await
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    if columns.is_empty() {
        return None; // schema not loaded — stay quiet
    }

    for segment in split_dynamic_segments(finder) {
        let column = pascal_to_snake(segment);
        if !is_simple_identifier(&column) {
            return None; // defensive — give up rather than risk a bad squiggle
        }
        if !columns.iter().any(|c| c == &column) {
            let suggestion = best_suggestion(&column, &columns);
            return Some(make_dynamic_where_diagnostic(
                &link.method,
                prefix,
                &column,
                &table,
                suggestion,
                link.span_byte_range,
                content,
                severity,
            ));
        }
    }
    None
}

/// Build the diagnostic for a dynamic-where finder. The squiggle covers the
/// studly column portion of the method name (`Emaaail` in `whereEmaaail`), and
/// the suggestion is the corrected *method* name (`whereEmail`).
#[allow(clippy::too_many_arguments)]
fn make_dynamic_where_diagnostic(
    method: &str,
    prefix: &str,
    column: &str,
    table: &str,
    suggestion: Option<String>,
    link_span: (usize, usize),
    content: &str,
    severity: DiagnosticSeverity,
) -> Diagnostic {
    // Locate the method name within the link's span to target the squiggle.
    // (Same dynamic finder called twice in one chain is the only ambiguity;
    // `find` lands on the first, still a correctly-named token.)
    let span_text = content.get(link_span.0..link_span.1).unwrap_or("");
    let method_rel = span_text.find(method).unwrap_or(0);
    let method_start = link_span.0 + method_rel;
    let studly_start = method_start + prefix.len();
    let method_end = method_start + method.len();
    let range = Range {
        start: byte_offset_to_position(content, studly_start),
        end: byte_offset_to_position(content, method_end),
    };

    let mut message =
        format!("Column \"{column}\" does not exist on table \"{table}\" (dynamic `{method}`).");
    let fixed_method = suggestion
        .as_ref()
        .map(|s| format!("{prefix}{}", snake_to_studly(s)));
    if let Some(ref fixed) = fixed_method {
        message.push_str(&format!(" Did you mean `{fixed}`?"));
    }

    // For the rename quick-fix: `range` covers the studly column portion of
    // the method name, so the replacement is the studly form of the suggested
    // column (`email` → `Email`), which turns `whereEmaaaail` into `whereEmail`.
    // `replacementLabel` shows the whole corrected method in the action title.
    let replacement = suggestion.as_ref().map(|s| snake_to_studly(s));
    let data = serde_json::json!({
        "kind": "column",
        "name": column,
        "dynamic": true,
        "method": method,
        "prefix": prefix,
        "suggestion": suggestion,
        "suggestedMethod": fixed_method,
        "replacement": replacement,
        "replacementLabel": fixed_method,
        "table": table,
    });

    Diagnostic {
        range,
        severity: Some(severity),
        code: Some(NumberOrString::String(CODE_UNKNOWN_COLUMN.to_string())),
        source: Some("laravel-lsp".to_string()),
        message,
        data: Some(data),
        ..Default::default()
    }
}

/// Column methods that take a *list* of columns rather than a single column
/// followed by operator/value args. Their positional string args AND array
/// elements are all columns. `where`-family methods are excluded: their arrays
/// are values or `col => value` pairs, and their 2nd+ args are operators/values.
const COLUMN_LIST_METHODS: &[&str] = &["select", "addSelect", "groupBy"];

/// The literal(s) to validate for a link, as `(value, span)` (span includes
/// quotes). Method-aware so we never validate a `where(...)` value or a
/// `whereHas(...)` operator:
///
/// - **Table** / **where-family column**: the first top-level string only.
/// - **Column list methods** (`select`/`addSelect`/`groupBy`): every top-level
///   string AND every string element of array args.
/// - **Relation**: the first argument only (a string, or the string elements of
///   an array like `with(['a', 'b'])` / `with(['rel' => $closure])`) — never the
///   2nd+ args, which are closures/operators.
fn collect_targets(link: &ChainLink, kind: DiagKind) -> Vec<(String, (usize, usize))> {
    match kind {
        DiagKind::Table => first_string_arg(link).into_iter().collect(),
        DiagKind::Relation => relation_targets(link),
        DiagKind::Column => {
            if COLUMN_LIST_METHODS.contains(&link.method.as_str()) {
                column_list_targets(link)
            } else {
                first_string_arg(link).into_iter().collect()
            }
        }
    }
}

/// Relation targets: the first arg only. A `StringLit` → that one; an `Array`
/// → its string elements (plain `'rel'` entries and keyed `'rel' => …` keys).
fn relation_targets(link: &ChainLink) -> Vec<(String, (usize, usize))> {
    match link.args.first() {
        Some(ChainArg::StringLit {
            value,
            span_byte_range,
            ..
        }) => vec![(value.clone(), *span_byte_range)],
        Some(arr @ ChainArg::Array { .. }) => array_string_elements(arr),
        _ => Vec::new(),
    }
}

/// Column-list targets: every top-level string arg plus every string element of
/// any array arg (`select('a', 'b')`, `select(['a', 'b'])`).
fn column_list_targets(link: &ChainLink) -> Vec<(String, (usize, usize))> {
    let mut out = Vec::new();
    for arg in &link.args {
        match arg {
            ChainArg::StringLit {
                value,
                span_byte_range,
                ..
            } => out.push((value.clone(), *span_byte_range)),
            ChainArg::Array { .. } => out.extend(array_string_elements(arg)),
            _ => {}
        }
    }
    out
}

/// The top-level string elements of an `Array` arg, as `(value, span)`.
fn array_string_elements(arg: &ChainArg) -> Vec<(String, (usize, usize))> {
    match arg {
        ChainArg::Array { elements, .. } => elements
            .iter()
            .filter_map(|e| match e {
                ChainArg::StringLit {
                    value,
                    span_byte_range,
                    ..
                } => Some((value.clone(), *span_byte_range)),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// The first top-level [`ChainArg::StringLit`] of a link, as `(value, span)`.
/// Span includes the surrounding quotes (as the extractor records it). Array
/// args and 2nd+ positional args are intentionally ignored.
fn first_string_arg(link: &ChainLink) -> Option<(String, (usize, usize))> {
    link.args.iter().find_map(|arg| match arg {
        ChainArg::StringLit {
            value,
            span_byte_range,
            ..
        } => Some((value.clone(), *span_byte_range)),
        _ => None,
    })
}

/// A bare PHP identifier: `[A-Za-z_][A-Za-z0-9_]*`. Rejects qualified names
/// (`users.id`), expressions (`count(*)`), aliases (`x as y`), and wildcards
/// (`*`) — none of which we validate.
fn is_simple_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Pick the closest candidate to `needle` as a "did you mean" suggestion.
/// Accepts a candidate within Levenshtein distance 2; failing that, the closest
/// candidate that shares a ≥3-character case-insensitive prefix. Returns `None`
/// when nothing is close enough — better no suggestion than a misleading one.
fn best_suggestion(needle: &str, candidates: &[String]) -> Option<String> {
    let needle_lower = needle.to_ascii_lowercase();
    let mut best_edit: Option<(usize, &str)> = None;
    let mut best_prefix: Option<(usize, &str)> = None;

    for candidate in candidates {
        let cand_lower = candidate.to_ascii_lowercase();
        let dist = levenshtein(&needle_lower, &cand_lower);
        if dist <= 2 {
            match best_edit {
                Some((d, _)) if d <= dist => {}
                _ => best_edit = Some((dist, candidate)),
            }
        }
        if common_prefix_len(&needle_lower, &cand_lower) >= 3 {
            match best_prefix {
                Some((d, _)) if d <= dist => {}
                _ => best_prefix = Some((dist, candidate)),
            }
        }
    }

    best_edit.or(best_prefix).map(|(_, c)| c.to_string())
}

/// Length of the shared leading run of two strings, in characters.
fn common_prefix_len(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

/// Classic two-row Levenshtein edit distance. Small inputs (identifier names),
/// so the allocation-light two-row form is plenty — no need for a crate.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr: Vec<usize> = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (prev[j + 1] + 1) // deletion
                .min(curr[j] + 1) // insertion
                .min(prev[j] + cost); // substitution
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// LSP `Range` covering just the `needle` at the end of a string literal's
/// content. The literal span includes its quotes (single-byte ASCII), so the
/// closing quote is one byte before the span end; the needle is the trailing
/// `needle.len()` content bytes (for dotted relations, the tail segment).
fn needle_range(lit_span: (usize, usize), needle: &str, content: &str) -> Range {
    let content_end = lit_span.1.saturating_sub(1);
    let needle_start = content_end.saturating_sub(needle.len());
    Range {
        start: byte_offset_to_position(content, needle_start),
        end: byte_offset_to_position(content, content_end),
    }
}

/// Build the `laravel-lsp.ambiguous-column` diagnostic for a bare column that
/// exists on more than one accessible table (issue #24). `tables` are the
/// tables that define it; the message suggests qualifying with the first.
fn ambiguous_column_diagnostic(
    needle: &str,
    tables: &[&str],
    lit_span: (usize, usize),
    content: &str,
    severity: DiagnosticSeverity,
) -> Diagnostic {
    let range = needle_range(lit_span, needle, content);
    let table_list = tables.join("`, `");
    let qualify_hint = tables
        .first()
        .map(|t| format!(" Qualify it, e.g. `{t}.{needle}`."))
        .unwrap_or_default();
    let message =
        format!("Column \"{needle}\" is ambiguous — it exists on `{table_list}`.{qualify_hint}");
    let data = serde_json::json!({
        "kind": "ambiguous-column",
        "name": needle,
        "tables": tables,
    });
    Diagnostic {
        range,
        severity: Some(severity),
        code: Some(NumberOrString::String(CODE_AMBIGUOUS_COLUMN.to_string())),
        source: Some("laravel-lsp".to_string()),
        message,
        data: Some(data),
        ..Default::default()
    }
}

/// Build the LSP `Diagnostic` for one unknown identifier. The squiggle is
/// narrowed to the offending segment (e.g. just `authorr` in `posts.authorr`).
/// A structured `data` payload carries everything the code-action handler needs
/// without re-parsing the message string.
#[allow(clippy::too_many_arguments)]
fn make_diagnostic(
    kind: DiagKind,
    needle: &str,
    subject: &str,
    suggestion: Option<String>,
    lit_span: (usize, usize),
    full_value: &str,
    content: &str,
    severity: DiagnosticSeverity,
) -> Diagnostic {
    let range = needle_range(lit_span, needle, content);

    let (code, data_kind) = match kind {
        DiagKind::Column => (CODE_UNKNOWN_COLUMN, "column"),
        DiagKind::Relation => (CODE_UNKNOWN_RELATION, "relation"),
        DiagKind::Table => (CODE_UNKNOWN_TABLE, "table"),
    };

    // `subject` is the raw table name (Column), the model FQCN (Relation), or
    // unused (Table).
    let mut message = match kind {
        DiagKind::Table => format!("Table \"{needle}\" does not exist."),
        DiagKind::Column => format!("Column \"{needle}\" does not exist on table \"{subject}\"."),
        DiagKind::Relation => format!("Relation \"{needle}\" does not exist on {subject}."),
    };
    if let Some(ref s) = suggestion {
        message.push_str(&format!(" Did you mean \"{s}\"?"));
    }

    // `replacement` is the exact text the rename quick-fix puts in `range`
    // (identical to the suggestion for these non-dynamic cases). `table` is
    // present only for columns — it drives the create-migration action.
    let data = serde_json::json!({
        "kind": data_kind,
        "name": needle,
        "value": full_value,
        "suggestion": suggestion,
        "replacement": suggestion,
        "replacementLabel": suggestion,
        "table": if kind == DiagKind::Column { Some(subject) } else { None },
    });

    Diagnostic {
        range,
        severity: Some(severity),
        code: Some(NumberOrString::String(code.to_string())),
        source: Some("laravel-lsp".to_string()),
        message,
        data: Some(data),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests;
