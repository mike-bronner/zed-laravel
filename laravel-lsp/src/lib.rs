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
pub mod blade_embedded_php;
pub mod blade_loops;
pub mod blade_php_block;
pub mod blade_props;
pub mod cache_manager;
pub mod class_locator;
pub mod component_declaration_locator;
pub mod config;
pub mod config_key_locator;
pub mod config_lookup;
pub mod database;
pub mod document_symbols;
pub mod env_key_locator;
pub mod file_watcher;
pub mod hover;
pub mod indexing_progress;
pub mod livewire_config;
pub mod livewire_declaration_locator;
pub mod livewire_resolver;
pub mod livewire_version;
pub mod middleware_binding_locator;
pub mod middleware_parser;
pub mod model_analyzer;
pub mod naming;
pub mod parser;
pub mod pattern_disk_cache;
pub mod pattern_indexer;
pub mod php_class;
pub mod php_outline;
pub mod queries;
pub mod query_chain;
pub mod references;
pub mod rename;
pub mod route_binding;
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

// Re-export commonly used types
pub use config::find_project_root;
pub use queries::{EchoPhpMatch, ExtractedBladePatterns, ExtractedPhpPatterns};
