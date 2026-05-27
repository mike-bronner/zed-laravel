//! Eloquent query builder chain completion.
//!
//! Recognises fluent chains in PHP source, identifies the model or table they're
//! rooted at, and answers the question "what columns or relations are valid at
//! the cursor". Phase 1 implements static method completion for direct chains;
//! the data structures (`BuilderChain`, `ChainContext`) are designed to also
//! support Phase 2's diagnostics and goto-definition-on-column.
//!
//! Three chain modes drive completion:
//!
//! - `EloquentBuilder` — pre-execution Eloquent (`User::where(...)`). `where()`
//!   offers DB columns; `with()` offers relations.
//! - `BaseBuilder` — `DB::table('users')` or post-`toBase()`. `where()` offers
//!   raw schema columns; `with()` returns nothing (no relations on the base
//!   query builder).
//! - `EloquentCollection` — post-execution Eloquent (`User::all()->where(...)`).
//!   `where()` filters a hydrated collection, so accessors and cast names are
//!   valid alongside DB columns.

pub mod chain;
pub mod cursor;
pub mod eloquent_completion;
pub mod extractor;
pub mod methods;
pub mod use_aliases;

pub use chain::*;
pub use cursor::{detect_chain_context_at, position_to_byte_offset};
pub use extractor::extract_chains;
pub use use_aliases::{extract_use_aliases, resolve_class_name, UseAliases};
