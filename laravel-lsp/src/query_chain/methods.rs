//! Classification tables for Eloquent / DB builder methods.
//!
//! Every method name we recognise falls into one of the `MethodKind` variants
//! defined in [`super::chain`]. The categorisation here drives both link
//! classification during extraction and the dispatch logic inside the chain
//! walker: a `ModeFlipToBase` flips the mode, a `CollectionTerminator` flips
//! to `EloquentCollection`, a `ChainTerminator` ends the chain entirely.
//!
//! The lists are small (~30 entries each) and the lookup is a linear scan
//! over `&'static [&'static str]` slices. That's faster than a `HashMap`
//! at this size and saves an allocation per call.

use super::chain::{ArgKind, ChainEffect};

/// Methods whose first string argument is a column name. Used both as static
/// entry points (`Model::where(...)`) and as chained links (`->where(...)`).
/// The same method name can also appear in `EloquentCollection` mode — the
/// completion source changes (DB cols + accessors + cast names) but the
/// classification stays the same.
pub const COLUMN_METHODS: &[&str] = &[
    "where",
    "orWhere",
    "whereIn",
    "whereNotIn",
    "whereNull",
    "whereNotNull",
    "whereBetween",
    "whereNotBetween",
    "whereDate",
    "whereMonth",
    "whereYear",
    "whereTime",
    "whereDay",
    "whereColumn",
    "orderBy",
    "orderByDesc",
    "latest",
    "oldest",
    "groupBy",
    "having",
    "havingRaw",
    "select",
    "addSelect",
    "pluck",
    "firstWhere",
    "value",
    "increment",
    "decrement",
    // Collection-shape methods that also accept column names as the first arg.
    "sortBy",
    "sortByDesc",
    "keyBy",
    "unique",
    "uniqueStrict",
];

/// Methods whose first string argument is a relation name. Eloquent-only —
/// these don't exist on the base query builder.
pub const RELATION_METHODS: &[&str] = &[
    "with",
    "without",
    "load",
    "loadMissing",
    "loadCount",
    "loadSum",
    "loadAvg",
    "loadMin",
    "loadMax",
    "has",
    "whereHas",
    "doesntHave",
    "whereDoesntHave",
    "orWhereHas",
    "orWhereDoesntHave",
    "withCount",
    "withSum",
    "withAvg",
    "withMin",
    "withMax",
    "withExists",
];

/// Methods whose second argument is a closure. Inside the closure body, the
/// builder parameter is bound to the model resolved from the first argument
/// (the relation name). Subset of `RELATION_METHODS`.
pub const CLOSURE_CARRIERS: &[&str] = &[
    "whereHas",
    "doesntHave",
    "whereDoesntHave",
    "orWhereHas",
    "orWhereDoesntHave",
    "withCount",
    "withSum",
    "withAvg",
    "withMin",
    "withMax",
];

/// Methods that flip the chain from Eloquent → base query builder. After
/// these, the chain is operating on `Illuminate\Database\Query\Builder`, so
/// relation methods would error at runtime — completion returns empty for
/// them.
pub const MODE_FLIP_TO_BASE: &[&str] = &["toBase", "getQuery"];

/// Methods that flip the chain from `EloquentBuilder` to `EloquentCollection`.
/// After these, the chain is operating on a hydrated `Collection`, so
/// accessors and cast names join DB columns as valid `where` arguments.
///
/// Note: `all()` is a Collection-returning static starter (`User::all()`) but
/// only appears as the chain entry point; handled at the receiver level rather
/// than as a mid-chain terminator.
pub const COLLECTION_TERMINATORS: &[&str] = &[
    "get",
    "pluck",
    "cursor",
    "lazy",
    "lazyById",
    "paginate",
    "simplePaginate",
    "cursorPaginate",
];

/// Methods that end the chain. Subsequent calls are operating on a single
/// Model, a scalar, or `void` — none of which is a builder, so completion
/// stops paying attention.
pub const CHAIN_TERMINATORS: &[&str] = &[
    // Single-model terminators
    "first",
    "firstOrFail",
    "firstOr",
    "find",
    "findOrFail",
    "findOrNew",
    "findMany",
    "sole",
    // Scalar terminators
    "count",
    "exists",
    "doesntExist",
    "sum",
    "avg",
    "average",
    "min",
    "max",
    "median",
    "mode",
    // Mutation terminators
    "update",
    "delete",
    "insert",
    "insertOrIgnore",
    "upsert",
    "forceCreate",
    "forceDelete",
    "save",
    "push",
    "truncate",
    "restore",
    // Iteration patterns — take callbacks, don't chain meaningfully
    "chunk",
    "chunkById",
    "each",
    "eachById",
    // Debug terminators — stop completion here for simplicity
    "dd",
    "dump",
    "ddRawSql",
    "dumpRawSql",
];

/// Methods that don't affect chain context — pass through unchanged. They
/// return the same builder type with the same effective model.
pub const TRANSPARENT: &[&str] = &[
    "clone",
    "cloneWithout",
    "cloneWithoutBindings",
    "tap",
    "when",
    "unless",
];

/// Static methods on an Eloquent model class that start a chain. This list
/// disambiguates Eloquent static calls from unrelated static calls like
/// `Carbon::now()` — the receiver detection still needs to confirm the class
/// actually extends `Illuminate\Database\Eloquent\Model`, but if the static
/// method isn't on this list (or the model's local scopes), it's not a chain
/// starter.
pub const ELOQUENT_STATIC_STARTERS: &[&str] = &[
    "query",
    "newQuery",
    "on",
    "onWriteConnection",
    "where",
    "whereIn",
    "whereNotIn",
    "whereNull",
    "whereNotNull",
    "whereBetween",
    "find",
    "findOrFail",
    "findOrNew",
    "findMany",
    "first",
    "firstWhere",
    "firstOrFail",
    "firstOrCreate",
    "firstOrNew",
    "firstOr",
    "all",
    "get",
    "cursor",
    "with",
    "without",
    "has",
    "whereHas",
    "doesntHave",
    "whereDoesntHave",
    "withCount",
    "withSum",
    "withAvg",
    "withMin",
    "withMax",
    "create",
    "make",
    "forceCreate",
    "insert",
    "insertOrIgnore",
    "upsert",
    "count",
    "exists",
    "doesntExist",
    "sum",
    "avg",
    "min",
    "max",
    "latest",
    "oldest",
    "orderBy",
    "groupBy",
    "having",
    "select",
    "addSelect",
    "pluck",
    "value",
    "chunk",
    "chunkById",
    "each",
    "paginate",
    "simplePaginate",
    "cursorPaginate",
    "inRandomOrder",
    "limit",
    "take",
    "skip",
    "offset",
    "forPage",
    "withTrashed",
    "onlyTrashed",
    "withoutTrashed",
];

/// What kind of argument a method expects at its first interesting position.
/// Used at the cursor's link only — the walker uses `chain_effect` for prior
/// links.
///
/// Precedence handles methods that appear in multiple lists:
/// - `ClosureCarrier` wins over `Relation` (drives closure scope descent)
/// - `Relation` wins over `Column` (no current overlap, but reserved)
/// - `Column` wins over `None` (methods like `pluck` are `Column` for cursor
///   purposes even though they also terminate the chain — termination is a
///   `ChainEffect`, orthogonal to `ArgKind`)
pub fn arg_kind(name: &str) -> ArgKind {
    if CLOSURE_CARRIERS.contains(&name) {
        return ArgKind::ClosureCarrier;
    }
    if RELATION_METHODS.contains(&name) {
        return ArgKind::Relation;
    }
    if COLUMN_METHODS.contains(&name) {
        return ArgKind::Column;
    }
    ArgKind::None
}

/// What this method does to the walker's chain context as it processes prior
/// links to reach the cursor. Orthogonal to `arg_kind` — `pluck` is both
/// `ArgKind::Column` AND `ChainEffect::FlipToCollection`.
pub fn chain_effect(name: &str) -> ChainEffect {
    if CHAIN_TERMINATORS.contains(&name) {
        return ChainEffect::Terminate;
    }
    if COLLECTION_TERMINATORS.contains(&name) {
        return ChainEffect::FlipToCollection;
    }
    if MODE_FLIP_TO_BASE.contains(&name) {
        return ChainEffect::FlipToBase;
    }
    ChainEffect::None
}

/// Whether this method, when called statically on an Eloquent model class,
/// starts a query chain. Used by receiver detection — a static call that
/// isn't on this list isn't treated as a chain starter (the call might be
/// `Carbon::now()` or similar).
pub fn is_eloquent_static_starter(name: &str) -> bool {
    ELOQUENT_STATIC_STARTERS.contains(&name)
}

#[cfg(test)]
mod tests;
