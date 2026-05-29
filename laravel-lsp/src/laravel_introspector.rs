//! Laravel-aware PHP class introspector — the LSP's one and only walker
//! for understanding what a class or file means to Laravel.
//!
//! Every consumer that asks "is this an Eloquent model?", "what scopes
//! does it have?", "what methods does Builder expose at `Model::|`?",
//! "what casts apply?", etc. goes through this module. There is no
//! parallel walker elsewhere.
//!
//! ## Sub-modules
//!
//! - [`walker`] — low-level PHP class parsing via tree-sitter. Returns
//!   raw structural info (classes, methods, properties, attributes,
//!   trait imports). No Laravel knowledge.
//! - [`chain`] — the Laravel-aware layer. Walks inheritance and trait
//!   composition, classifies the class kind (Model / Eloquent\Builder /
//!   Query\Builder / Other), and pre-computes every Laravel surface
//!   (scopes, accessors, relationships, casts, table, `__callStatic`
//!   surface). Exposes [`chain::ClassView`] (re-exported from the
//!   module root) and [`chain::analyze`] / [`chain::analyze_content`].
//! - [`model_metadata`] — public `ModelMetadata` API. A thin adapter
//!   that converts a [`chain::ClassView`] into the historical
//!   `ModelMetadata` shape callers across the LSP already use. Also
//!   houses a few file-level utilities ([`extract_namespace`],
//!   [`extract_use_aliases`], [`resolve_to_fqcn`], [`pascal_to_snake`],
//!   [`parse_cast_array`]) and the cast/relationship type-mapping
//!   helpers.
//! - [`builder_index`] — public `BuilderMethodIndex` API. Assembles the
//!   `Model::|` callstatic surface from three [`chain::ClassView`]s
//!   (Eloquent\Builder, Query\Builder, Model).
//!
//! The sub-files are an internal organisation; consumers depend on
//! `crate::laravel_introspector::*` only and don't reach past the
//! re-exports below.

pub mod builder_index;
pub mod chain;
pub mod model_metadata;
pub mod walker;

// ---- Re-exports — the canonical paths consumers should use ----

// Core analysis API.
pub use chain::{
    analyze, analyze_content, extends_eloquent_model, AccessorInfo, BuilderMethod, ClassView,
    ColumnInfo, ColumnSource, LaravelClassKind, RelationshipInfo, ResolvedMember, ScopeInfo,
    ScopeStyle, AUTHENTICATABLE_TRAIT_FQCN, ELOQUENT_BUILDER_FQCN, ELOQUENT_MODEL_FQCN,
    FOUNDATION_AUTH_USER_FQCN, QUERY_BUILDER_FQCN, SCOPE_ATTRIBUTE_FQCN, SOFT_DELETES_FQCN,
};

// Model metadata API + utilities. The `*Info` types come from `chain`;
// `ModelMetadata` carries its own `AccessorInfo`/`RelationshipInfo` for
// backward compatibility with call sites that depended on
// `ModelMetadata`-flavoured shapes, but they're identical to the
// canonical ones in `chain`.
pub use model_metadata::{
    extract_namespace, extract_use_aliases, map_cast_to_php_type, parse_array_keys_public,
    parse_cast_array, parse_string_array, pascal_to_snake, relationship_to_php_type,
    resolve_to_fqcn, snake_to_studly, ModelMetadata,
};
// Compatibility re-exports of the historical `AccessorInfo` /
// `RelationshipInfo` shapes that some call sites import from this
// module specifically. These are structurally identical to the ones
// in `chain` — the model_metadata module keeps its own copy because
// it's the public API surface that existed before the consolidation.
pub use model_metadata::{AccessorInfo as ModelAccessorInfo, RelationshipInfo as ModelRelationshipInfo};

// Builder method index API.
pub use builder_index::{
    parse_builder_methods, BuilderMethodIndex, ELOQUENT_BUILDER_REL_PATH,
    ELOQUENT_MODEL_REL_PATH, ParsedMethod, QUERY_BUILDER_REL_PATH,
};

// Low-level PHP structure types — surfaced for the few call sites
// that work directly with the parsed AST shape. Most consumers don't
// need these.
pub use walker::{
    extract_php_structure, PhpFileStructure, PhpFunctionInfo, PhpMethodInfo, PhpParameter,
    PhpPropertyInfo, PhpStructure, PhpStructureKind, PhpVisibility,
};
