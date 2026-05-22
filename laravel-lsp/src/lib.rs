//! Laravel LSP - Language Server Protocol implementation for Laravel development
//!
//! This library provides the core functionality for the Laravel LSP server,
//! including pattern extraction, Salsa incremental computation, and file resolution.

// Core modules
pub mod blade_loops;
pub mod blade_php_block;
pub mod cache_manager;
pub mod config;
pub mod database;
pub mod middleware_parser;
pub mod model_analyzer;
pub mod parser;
pub mod php_class;
pub mod queries;
pub mod route_discovery;
pub mod slot_navigation;
pub mod validation_rules;

// Salsa 0.25 implementation (incremental computation)
pub mod salsa_impl;

// Re-export commonly used types
pub use config::find_project_root;
pub use queries::{EchoPhpMatch, ExtractedBladePatterns, ExtractedPhpPatterns};
