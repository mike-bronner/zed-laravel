//! Laravel LSP - Language Server Protocol implementation for Laravel development
//!
//! This library provides the core functionality for the Laravel LSP server,
//! including pattern extraction, Salsa incremental computation, and file resolution.

// Allow complex types crate-wide. Several Salsa caches and internal data
// structures use deeply-nested generic types (e.g. `LruCache<PathBuf, (i32,
// Arc<Vec<(String, String)>>)>`). Extracting type aliases for each is a
// worthwhile follow-up refactor — tracked separately from CI hardening.
#![allow(clippy::type_complexity)]

// Core modules
pub mod blade_directive_tokens;
pub mod blade_embedded_php;
pub mod blade_loops;
pub mod blade_php_block;
pub mod blade_props;
pub mod blade_var_rename;
pub mod cache_manager;
pub mod class_locator;
pub mod class_rename;
pub mod code_lens;
pub mod column_rename;
pub mod command_call_locator;
pub mod command_disk_cache;
pub mod command_index;
pub mod command_signature;
pub mod completion_format;
pub mod component_completion;
pub mod component_declaration_locator;
pub mod composer_autoload;
pub mod config;
pub mod config_key_locator;
pub mod config_lookup;
pub mod database;
pub mod document_symbols;
pub mod env_key_locator;
pub mod file_watcher;
pub mod hover;
pub mod indexing_progress;
pub mod laravel_introspector;
pub mod livewire_config;
pub mod livewire_declaration_locator;
pub mod livewire_resolver;
pub mod livewire_version;
pub mod method_name_completion;
pub mod middleware_binding_locator;
pub mod middleware_parser;
pub mod migration_index;
// model_analyzer was consolidated into `laravel_introspector::model_metadata`.
// Public API is re-exported as `laravel_introspector::ModelMetadata`.
pub mod magic_dependency_index;
pub mod magic_disk_cache;
pub mod naming;
pub mod parser;
pub mod pattern_disk_cache;
pub mod pattern_indexer;
pub mod php_class;
// php_outline was consolidated into `laravel_introspector::walker`.
pub mod queries;
pub mod query_chain;
pub mod references;
pub mod rename;
pub mod route_binding;
pub mod route_chain;
pub mod route_discovery;
pub mod route_name_locator;
pub mod route_outline;
pub mod slot_navigation;
pub mod translation_key_locator;
pub mod translation_lookup;
pub mod validation_rules;
pub mod vendor_translations;
pub mod view_declaration_locator;

// Salsa 0.25 implementation (incremental computation)
pub mod salsa_impl;

// Inverted symbol index for O(1) find-references
pub mod symbol_index;

// Project-wide class-hierarchy + member index (structural code lenses)
pub mod class_hierarchy_index;

// Prove a member is read across the inheritance chain (incl. vendor) — used to
// avoid flagging framework-read config properties (e.g. $timestamps) as unused
pub mod vendor_member_prover;

// Magic-member resolve + classify engine (find-references / lens / hover)
pub mod member_resolver;

// Controller/Volt → Blade view-variable type inference (magic members in Blade)
pub mod view_var_index;

// Re-export commonly used types
pub use config::find_project_root;
pub use queries::{EchoPhpMatch, ExtractedBladePatterns, ExtractedPhpPatterns};
