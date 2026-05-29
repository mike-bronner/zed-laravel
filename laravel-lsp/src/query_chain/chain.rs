//! Types describing an Eloquent / DB query builder chain extracted from PHP source.
//!
//! A `BuilderChain` is the precomputed shape of one fluent chain
//! (e.g. `User::where(...)->with(...)->get()`). It lives in the Salsa pattern
//! cache alongside other extracted patterns, so chain extraction happens in the
//! same tree-sitter pass as everything else and consumers (completion today,
//! diagnostics and goto-def in Phase 2) read it without re-parsing.

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct BuilderChain {
    pub receiver: ChainReceiver,
    pub span_byte_range: (usize, usize),
    pub links: Vec<ChainLink>,
    /// Closure-scope binding when this chain's `$var` receiver is bound
    /// by an enclosing relation closure — `whereHas('rel', fn ($q) => …)`
    /// or `with(['rel' => fn ($q) => …])`. The cursor resolver finds the
    /// parent chain (by span containment), looks up the relation on its
    /// effective model, and uses the related model as this chain's
    /// effective model. `None` for chains that aren't inside a
    /// recognized closure context.
    #[serde(default)]
    pub closure_scope: Option<ClosureScopeBinding>,
}

/// Records that this chain's receiver `$var` is bound by an outer
/// closure-carrying method. Two flavors:
///
/// - `RelationHop`: the closure receives a builder for a *related*
///   model, like `whereHas('posts', fn ($q) => …)` or `with(['rel' =>
///   fn ($q) => …])`. The cursor resolver walks one relation hop on the
///   parent chain's model to get the actual class.
/// - `SameModel`: the closure receives the *same* builder as the outer
///   chain, like `where(function ($q) { $q->where(…)->orWhere(…); })` or
///   `when($cond, fn ($q) => $q->where(…))`. The cursor resolver
///   inherits the parent chain's effective model directly — no relation
///   hop needed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ClosureScopeBinding {
    /// The closure parameter name — must match the chain's
    /// `InstanceVar::var` for the binding to apply. Captured so
    /// `whereHas('posts', function ($outer) use ($inner) { … })` doesn't
    /// accidentally bind a different variable.
    pub param_var: String,
    pub kind: ClosureScopeKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ClosureScopeKind {
    /// `whereHas('posts', closure)` / `with(['rel' => closure])` — bind
    /// to the related model's builder.
    RelationHop { relation_name: String },
    /// `where(closure)` / `when($cond, closure)` / `having(closure)` /
    /// `tap(closure)` etc. — bind to the same model as the outer chain.
    SameModel,
    /// `join('orders', fn ($join) => …)` — `$join` is a `JoinClause` builder
    /// rooted at the joined table (issue #24). `table_ref` is the raw
    /// first-arg string (`"orders"`, `"orders as o"`, `"mydb.orders"`); the
    /// cursor resolver parses it to an [`AccessibleTable`] and models it as a
    /// `from()` override so column completion inside the closure resolves
    /// against that table (alias included).
    JoinTable { table_ref: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ChainReceiver {
    Eloquent(EloquentReceiver),
    DbTable {
        table: String,
        name_byte_range: (usize, usize),
    },
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum EloquentReceiver {
    /// `User::query()`, `User::where(...)`, etc. — any static call against a
    /// class that resolves to an Eloquent model.
    StaticModel(String),
    /// `$user->newQuery()` or any chain rooted at an instance variable. The
    /// `php_type` is filled by [`crate::query_chain`]'s `var_type` resolver
    /// (docblock + typed function param scan); when `None`, the receiver
    /// cannot be resolved and completion silently returns nothing.
    InstanceVar {
        var: String,
        php_type: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ChainLink {
    pub method: String,
    /// What kind of argument this method expects at its first interesting
    /// position — drives completion when the cursor is inside this link.
    pub arg: ArgKind,
    /// What this link does to the walker's chain context — drives mode flips
    /// and chain termination as the walker processes prior links to arrive
    /// at the cursor.
    pub effect: ChainEffect,
    pub span_byte_range: (usize, usize),
    pub args: Vec<ChainArg>,
}

/// What kind of argument the cursor's link expects. Used at the cursor only —
/// for prior links, the walker reads `effect` instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ArgKind {
    /// `where`, `orderBy`, `select`, `pluck`, `sortBy`, … — first string arg
    /// is a column name. Which sources count as "a column" depends on the
    /// current `BuilderMode` at this point in the chain.
    Column,
    /// `with`, `load`, `withCount`, … — first string arg is a relation name
    /// on the current effective model. Eloquent-only.
    Relation,
    /// `whereHas`, `whereDoesntHave`, `withCount`, … — first arg names a
    /// relation, second arg is a closure whose builder parameter is bound
    /// to the related model. The cursor can be inside either the relation
    /// name (treated as `Relation`) or the closure body (handled by closure
    /// scope tracking).
    ClosureCarrier,
    /// `DB::table('|')` — first string arg names a database table. Set by
    /// the extractor after receiver detection (the method name alone doesn't
    /// reveal this; we need to know the class is `DB`).
    Table,
    /// This link doesn't expose a completable argument we recognise — used
    /// for terminators with no relevant string args, mode-flippers, and
    /// transparent transformers.
    None,
}

/// What the walker does when it processes this link en route to the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ChainEffect {
    /// No change to context. Includes `Column` / `Relation` methods that
    /// don't terminate (`where`, `with`, …) and the explicit transparent
    /// transformers (`clone`, `tap`, `when`, `unless`).
    None,
    /// `toBase`, `getQuery` — switches `EloquentBuilder` → `BaseBuilder`.
    FlipToBase,
    /// `get`, `pluck`, `cursor`, `paginate`, … — switches `EloquentBuilder`
    /// → `EloquentCollection`. After execution, `where()` filters in memory
    /// and accessors become valid arguments. Note that `pluck` is both
    /// `ArgKind::Column` (cursor inside expects a column) and `ChainEffect::
    /// FlipToCollection` (subsequent links operate on a Collection).
    FlipToCollection,
    /// `first`, `find`, `count`, `update`, … — ends the chain entirely. No
    /// further completion for chained calls; the result is a Model, scalar,
    /// or void.
    Terminate,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ChainArg {
    StringLit {
        value: String,
        quote: char,
        span_byte_range: (usize, usize),
    },
    Closure {
        params: Vec<ClosureParam>,
        body_byte_range: (usize, usize),
    },
    /// Array literal: `['posts', 'comments']`, `[$var, 'col']`, etc.
    /// `elements` recursively classifies each entry (string literals are
    /// kept as `StringLit` so the cursor resolver can walk them; values
    /// we don't model become `Other`). The span covers the entire array
    /// expression including the brackets.
    Array {
        elements: Vec<ChainArg>,
        span_byte_range: (usize, usize),
    },
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ClosureParam {
    pub name: String,
    pub php_type: Option<String>,
}

/// A table reachable for column references within a chain — either the root
/// (the receiver, or a `from()` replacement) or one made available by a
/// join. Carries the real schema name (used to fetch columns) separately
/// from the alias the user types, so `join('users as u', …)` offers `u.*`
/// while looking columns up under `users`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessibleTable {
    /// Real table name as it appears in the database. May be schema-qualified
    /// (`mydb.orders`) for cross-database joins; column lookup passes it
    /// through to the schema provider, which resolves it best-effort.
    pub table: String,
    /// Alias the user references columns by — `u` from `users as u`, or a
    /// subquery alias. `None` for a plain `join('orders', …)`, where the
    /// qualifier IS the table name.
    pub alias: Option<String>,
}

impl AccessibleTable {
    /// Construct an unaliased accessible table from a bare name.
    pub fn bare(table: impl Into<String>) -> Self {
        Self {
            table: table.into(),
            alias: None,
        }
    }

    /// The qualifier the user types before a column: the alias if present,
    /// else the table name. For a schema-qualified table with no alias this
    /// is the full `mydb.orders`.
    pub fn qualifier(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.table)
    }
}

/// Parse a Laravel table reference string into an [`AccessibleTable`].
///
/// Handles the three shapes that appear in `join()` / `from()` arguments:
/// - `'orders'` → table `orders`, no alias
/// - `'mydb.orders'` → schema-qualified table `mydb.orders`, no alias
/// - `'users as u'` / `'users AS u'` → table `users`, alias `u`
/// - `'users u'` → implicit alias (MySQL-style), table `users`, alias `u`
///
/// The `as` keyword is matched case-insensitively. The table portion is kept
/// verbatim (dots and all) so schema qualifiers survive to the column lookup.
pub fn parse_table_ref(raw: &str) -> AccessibleTable {
    let trimmed = raw.trim();
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    match tokens.as_slice() {
        // `table as alias`
        [table, kw, alias] if kw.eq_ignore_ascii_case("as") => AccessibleTable {
            table: (*table).to_string(),
            alias: Some((*alias).to_string()),
        },
        // `table alias` (implicit alias)
        [table, alias] => AccessibleTable {
            table: (*table).to_string(),
            alias: Some((*alias).to_string()),
        },
        // `table` (no alias) — also the fallback for any unexpected shape,
        // where keeping the whole trimmed string as the table name is the
        // least-surprising behavior.
        _ => AccessibleTable::bare(trimmed.to_string()),
    }
}

/// How the chain's root (FROM) table is determined, after accounting for any
/// receiver-replacing `from*()` call in the chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromClause {
    /// No `from*()` override — the root is the receiver's table (`DB::table`)
    /// or the model's table (Eloquent).
    Inherit,
    /// `from('admins')` / `from('admins as a')` replaced the root with a
    /// concrete table. Columns come from the schema only (no model casts —
    /// the model→table mapping no longer applies once `from()` redirects it).
    Replace(AccessibleTable),
    /// `fromRaw(...)` (and, until Phase 4, `fromSub(...)`) made the root an
    /// opaque expression — no bare root columns can be offered, though any
    /// joined tables remain valid.
    Opaque,
}

/// Completion-time context produced fresh from a `BuilderChain` plus a cursor
/// position. Not stored — recomputed on each completion request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainContext {
    pub mode: BuilderMode,
    /// The DB table to query for columns. `None` only when completion cannot
    /// fire (unresolved receiver, unknown chain).
    pub effective_table: Option<String>,
    /// The current Eloquent model class. `None` for pure `DbTable` chains;
    /// `Some` for Eloquent chains, including post-`toBase()` (where the model
    /// is still known even though the mode is `BaseBuilder`).
    pub effective_model: Option<String>,
    /// What the link at the cursor expects — `Column` or `Relation` drive
    /// completion; `ClosureCarrier` is handled by closure-scope descent;
    /// `None` means no completion.
    pub expecting: ArgKind,
    /// Everything the user has typed before the last dot of the literal under
    /// the cursor. Two meanings depending on `expecting`:
    /// - **Relation** (`with('posts.author.|')`) — the relation segments to
    ///   walk; the final model's relations are then offered.
    /// - **Column** (`where('orders.|')`) — the table qualifier (alias or
    ///   table name) the column belongs to; completion narrows to that one
    ///   table's columns.
    pub dotted_prefix: Option<String>,
    /// Tables made accessible by `join()`-family calls, in source order.
    /// Global to the chain — Laravel compiles the whole chain into one SQL
    /// query regardless of textual order, so a join anywhere in the chain
    /// makes its table referenceable everywhere. Columns from these are
    /// offered as `qualifier.column`.
    ///
    /// Inside a join closure (`join('orders', fn ($join) => …)`) this also
    /// carries the *parent* query's accessible tables that resolve
    /// synchronously (a `DB::table()` / `from()` root and the parent's other
    /// joins), so both sides of an ON clause complete.
    pub joined_tables: Vec<AccessibleTable>,
    /// Whether a `from*()` call replaced or obscured the chain's root table.
    /// `Inherit` for the common case (no `from()` override).
    pub from_clause: FromClause,
    /// Inside a join closure whose *parent* query is rooted at an Eloquent
    /// model (`User::query()->join('orders', fn ($join) => …)`), the parent
    /// model's FQCN. Its table needs async model→table resolution, so the
    /// cursor resolver can't add it to `joined_tables` synchronously —
    /// consumers resolve it and fold it into the accessible set (mirrors how
    /// `closure_relation_hop` defers a relation hop). `None` otherwise.
    pub join_parent_model: Option<String>,
    /// Set when the chain is inside a recognized relation closure
    /// (`whereHas('posts', fn ($q) => $q->where('|'))` or
    /// `with(['posts' => fn ($q) => $q->where('|')])`). When `Some(rel)`,
    /// `effective_model` is the *parent* chain's model and the handler
    /// must resolve `rel` against it (one relation hop) to get the
    /// actual model to complete against.
    pub closure_relation_hop: Option<String>,
    /// The quote character the user is typing inside (`'` or `"`). Used so the
    /// completion item doesn't double up quotes when inserting.
    pub quote: char,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum BuilderMode {
    /// Pre-execution Eloquent: full property/relation/cast support.
    EloquentBuilder,
    /// `DB::table()` or any Eloquent chain after `->toBase()` / `->getQuery()`.
    /// Schema columns only; no relations, no accessors, no casts.
    BaseBuilder,
    /// Post-execution Eloquent (after `->get()`, `->pluck()`, `->all()`, …).
    /// Filtering happens in memory on a hydrated Collection, so accessors and
    /// cast names join DB columns as valid arguments. Relations remain valid
    /// via `load()`.
    EloquentCollection,
}

#[cfg(test)]
mod tests;
