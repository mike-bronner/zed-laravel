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
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ClosureParam {
    pub name: String,
    pub php_type: Option<String>,
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
    /// For dotted relation paths like `with('posts.author.|')`, the prefix the
    /// user has already typed after the last dot. Completion items filter
    /// against this and `insert_text` is just the current segment.
    pub dotted_prefix: Option<String>,
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
