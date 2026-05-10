//! Laravel LSP - Language Server Protocol implementation for Laravel development
//!
//! This library provides the core functionality for the Laravel LSP server,
//! including pattern extraction, Salsa incremental computation, and file resolution.

// Core modules
pub mod parser;
pub mod queries;
pub mod config;
pub mod middleware_parser;
pub mod cache_manager;
pub mod validation_rules;
pub mod database;
pub mod model_analyzer;
pub mod route_discovery;

// Salsa 0.25 implementation (incremental computation)
pub mod salsa_impl;

// Re-export commonly used types
pub use config::find_project_root;
pub use queries::{ExtractedPhpPatterns, ExtractedBladePatterns, EchoPhpMatch};
