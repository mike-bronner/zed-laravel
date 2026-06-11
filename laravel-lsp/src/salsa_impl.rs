//! Salsa 0.25 incremental computation database for Laravel LSP
//!
//! This module provides proper incremental computation using the Salsa framework.
//! It replaces the custom "Salsa-inspired" implementation in salsa_db.rs.
//!
//! # Actor Pattern for Async Integration
//!
//! Since Salsa's `Storage` type is not `Send+Sync`, we use an actor pattern to
//! run Salsa operations on a dedicated thread. The `SalsaActor` owns the database
//! and processes requests via channels.
#![allow(dead_code)]

use lru::LruCache;
use salsa::Setter;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot};
use tracing::debug;

use crate::config::kebab_to_pascal_case;
use crate::middleware_parser::middleware_base_alias;
use crate::parser::{language_php, parse_php};
use crate::queries::extract_all_php_patterns;

// ============================================================================
// Database Definition
// ============================================================================

/// The Salsa database trait for Laravel LSP
#[salsa::db]
pub trait Db: salsa::Database {}

/// The concrete database implementation
#[salsa::db]
#[derive(Default, Clone)]
pub struct LaravelDatabase {
    storage: salsa::Storage<Self>,
}

#[salsa::db]
impl salsa::Database for LaravelDatabase {}

#[salsa::db]
impl Db for LaravelDatabase {}

// ============================================================================
// Input Types - Source data provided to the system
// ============================================================================

/// Represents a source file in the workspace
#[salsa::input]
pub struct SourceFile {
    /// The file path
    #[returns(ref)]
    pub path: PathBuf,

    /// The document version from LSP
    pub version: i32,

    /// The file content
    #[returns(ref)]
    pub text: String,
}

/// Represents a configuration file (composer.json, config/*.php)
#[salsa::input]
pub struct ConfigFile {
    /// The file path
    #[returns(ref)]
    pub path: PathBuf,

    /// Version incremented when file changes
    pub version: i32,

    /// The file content
    #[returns(ref)]
    pub text: String,
}

/// Represents the project's registered files for reference finding
/// Files are grouped by their source directory type
#[salsa::input]
pub struct ProjectFiles {
    /// Version incremented when file list changes
    pub version: i32,

    /// PHP files in app/Http/Controllers
    #[returns(ref)]
    pub controller_files: Vec<PathBuf>,

    /// Blade files in view paths
    #[returns(ref)]
    pub view_files: Vec<PathBuf>,

    /// PHP files in app/Livewire
    #[returns(ref)]
    pub livewire_files: Vec<PathBuf>,

    /// PHP files in routes/
    #[returns(ref)]
    pub route_files: Vec<PathBuf>,
}

/// Represents a service provider file with priority
#[salsa::input]
pub struct ServiceProviderFile {
    /// The file path
    #[returns(ref)]
    pub path: PathBuf,

    /// Version incremented when file changes
    pub version: i32,

    /// The file content
    #[returns(ref)]
    pub text: String,

    /// Priority: 0=framework, 1=package, 2=app
    pub priority: u8,
}

/// Represents an environment file (.env, .env.local, .env.example)
#[salsa::input]
pub struct EnvFile {
    /// The file path
    #[returns(ref)]
    pub path: PathBuf,

    /// Version incremented when file changes
    pub version: i32,

    /// The file content
    #[returns(ref)]
    pub text: String,

    /// Priority: 0=.env.example, 1=.env.local, 2=.env (highest)
    pub priority: u8,
}

// ============================================================================
// Interned Types - Deduplicated strings
// ============================================================================

/// Interned string for view names (e.g., "users.profile")
#[salsa::interned]
pub struct ViewName<'db> {
    #[returns(ref)]
    pub name: String,
}

/// Interned string for component names (e.g., "button")
#[salsa::interned]
pub struct ComponentName<'db> {
    #[returns(ref)]
    pub name: String,
}

/// Interned string for directive names (e.g., "extends")
#[salsa::interned]
pub struct DirectiveName<'db> {
    #[returns(ref)]
    pub name: String,
}

/// Interned string for env variable names (e.g., "APP_DEBUG")
#[salsa::interned]
pub struct EnvVarName<'db> {
    #[returns(ref)]
    pub name: String,
}

/// Interned string for config keys (e.g., "app.name")
#[salsa::interned]
pub struct ConfigKey<'db> {
    #[returns(ref)]
    pub key: String,
}

/// Interned string for middleware names (e.g., "auth", "throttle:60,1")
#[salsa::interned]
pub struct MiddlewareName<'db> {
    #[returns(ref)]
    pub name: String,
}

/// Interned string for translation keys (e.g., "messages.welcome")
#[salsa::interned]
pub struct TranslationKey<'db> {
    #[returns(ref)]
    pub key: String,
}

/// Interned string for asset paths (e.g., "css/app.css")
#[salsa::interned]
pub struct AssetPath<'db> {
    #[returns(ref)]
    pub path: String,
}

/// Interned string for binding names (e.g., "auth", "App\\Contracts\\PaymentGateway")
#[salsa::interned]
pub struct BindingName<'db> {
    #[returns(ref)]
    pub name: String,
}

#[salsa::interned]
pub struct RouteName<'db> {
    #[returns(ref)]
    pub name: String,
}

#[salsa::interned]
pub struct UrlPath<'db> {
    #[returns(ref)]
    pub path: String,
}

#[salsa::interned]
pub struct ActionName<'db> {
    #[returns(ref)]
    pub action: String,
}

/// Interned string for package view namespace (e.g., "courier", "mail")
#[salsa::interned]
pub struct PackageNamespace<'db> {
    #[returns(ref)]
    pub namespace: String,
}

// ============================================================================
// Tracked Types - Computed/derived values
// ============================================================================

/// A parsed view reference found in code
#[salsa::tracked]
pub struct ViewReference<'db> {
    pub name: ViewName<'db>,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
    pub is_route_view: bool,
}

/// A parsed component reference found in code
#[salsa::tracked]
pub struct ComponentReference<'db> {
    pub name: ComponentName<'db>,
    pub tag_name: ComponentName<'db>,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// A parsed directive reference found in code
#[salsa::tracked]
pub struct DirectiveReference<'db> {
    pub name: DirectiveName<'db>,
    #[returns(ref)]
    pub arguments: Option<String>,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
    /// Column of first character INSIDE the quoted string (after opening quote)
    pub string_column: u32,
    /// Column one past the last character INSIDE the quoted string (before closing quote)
    pub string_end_column: u32,
}

/// A parsed env reference found in code
#[salsa::tracked]
pub struct EnvReference<'db> {
    pub name: EnvVarName<'db>,
    pub has_fallback: bool,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// A parsed config reference found in code
#[salsa::tracked]
pub struct ConfigReference<'db> {
    pub key: ConfigKey<'db>,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Interned string for Livewire component names
#[salsa::interned]
pub struct LivewireName<'db> {
    #[returns(ref)]
    pub name: String,
}

/// A parsed Livewire component reference found in code
#[salsa::tracked]
pub struct LivewireReference<'db> {
    pub name: LivewireName<'db>,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// A parsed middleware reference found in code
#[salsa::tracked]
pub struct MiddlewareReference<'db> {
    pub name: MiddlewareName<'db>,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// A parsed translation reference found in code
#[salsa::tracked]
pub struct TranslationReference<'db> {
    pub key: TranslationKey<'db>,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Asset helper type - mirrors queries::AssetHelperType
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum AssetHelperType {
    Asset,
    PublicPath,
    BasePath,
    AppPath,
    StoragePath,
    DatabasePath,
    LangPath,
    ConfigPath,
    ResourcePath,
    Mix,
    ViteAsset,
}

/// A parsed asset reference found in code
#[salsa::tracked]
pub struct AssetReference<'db> {
    pub path: AssetPath<'db>,
    pub helper_type: AssetHelperType,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// A parsed binding reference found in code
#[salsa::tracked]
pub struct BindingReference<'db> {
    pub name: BindingName<'db>,
    pub is_class_reference: bool,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// A parsed route() call found in code
#[salsa::tracked]
pub struct RouteReference<'db> {
    pub name: RouteName<'db>,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// A parsed url() call found in code
#[salsa::tracked]
pub struct UrlReference<'db> {
    pub path: UrlPath<'db>,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// A parsed action() call found in code
#[salsa::tracked]
pub struct ActionReference<'db> {
    pub action: ActionName<'db>,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// All patterns found in a file
/// Note: route_refs, url_refs, action_refs are parsed separately to keep field count under 12
/// (Salsa's tuple-based Hash impl has a 12-element limit)
#[salsa::tracked]
pub struct ParsedPatterns<'db> {
    pub file: SourceFile,
    #[returns(ref)]
    pub views: Vec<ViewReference<'db>>,
    #[returns(ref)]
    pub components: Vec<ComponentReference<'db>>,
    #[returns(ref)]
    pub directives: Vec<DirectiveReference<'db>>,
    #[returns(ref)]
    pub env_refs: Vec<EnvReference<'db>>,
    #[returns(ref)]
    pub config_refs: Vec<ConfigReference<'db>>,
    #[returns(ref)]
    pub livewire_refs: Vec<LivewireReference<'db>>,
    #[returns(ref)]
    pub middleware_refs: Vec<MiddlewareReference<'db>>,
    #[returns(ref)]
    pub translation_refs: Vec<TranslationReference<'db>>,
    #[returns(ref)]
    pub asset_refs: Vec<AssetReference<'db>>,
    #[returns(ref)]
    pub binding_refs: Vec<BindingReference<'db>>,
}

/// Parsed Laravel configuration (from composer.json, config/view.php, etc.)
#[salsa::tracked]
pub struct LaravelConfigRef<'db> {
    /// Project root path
    #[returns(ref)]
    pub root: PathBuf,

    /// View paths configured in config/view.php
    #[returns(ref)]
    pub view_paths: Vec<PathBuf>,

    /// Component paths with optional namespace prefix
    #[returns(ref)]
    pub component_paths: Vec<(String, PathBuf)>,

    /// Livewire component path (if Livewire is installed)
    #[returns(ref)]
    pub livewire_path: Option<PathBuf>,

    /// Whether Livewire is installed (detected from composer.json)
    pub has_livewire: bool,
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Extract a string value from directive arguments like ('welcome') or ("welcome")
///
/// Returns (string_value, start_offset, end_offset) if found
fn extract_string_from_args(args: &str) -> Option<(String, usize, usize)> {
    // Find the first quote character (single or double)
    let chars: Vec<char> = args.chars().collect();
    let mut i = 0;

    // Skip until we find a quote
    while i < chars.len() {
        if chars[i] == '\'' || chars[i] == '"' {
            break;
        }
        i += 1;
    }

    if i >= chars.len() {
        return None;
    }

    let quote_char = chars[i];
    let start_pos = i + 1; // Position after opening quote
    i += 1;

    // Find the closing quote
    let mut content = String::new();
    while i < chars.len() && chars[i] != quote_char {
        content.push(chars[i]);
        i += 1;
    }

    if content.is_empty() {
        return None;
    }

    let end_pos = i; // Position of closing quote

    Some((content, start_pos, end_pos))
}

/// Extract translation key from PHP content inside {{ ... }} Blade echo statements
///
/// Handles common translation functions:
/// - __("Welcome to our app")
/// - __('messages.welcome')
/// - trans("messages.welcome")
/// - trans_choice("messages.items", $count)
/// - @lang("messages.welcome")
///
/// Returns (translation_key, start_offset, end_offset) if found
pub fn extract_translation_from_echo(php_content: &str) -> Option<(String, usize, usize)> {
    use regex::Regex;

    // Match translation function calls: __(), trans(), trans_choice()
    // We need separate patterns for single and double quotes since regex crate doesn't support backreferences
    static TRANS_REGEX_SINGLE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r#"(?:__|trans|trans_choice)\s*\(\s*'([^']+)'"#).unwrap()
    });
    static TRANS_REGEX_DOUBLE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r#"(?:__|trans|trans_choice)\s*\(\s*"([^"]+)""#).unwrap()
    });

    // Try single quotes first
    if let Some(captures) = TRANS_REGEX_SINGLE.captures(php_content) {
        let key_match = captures.get(1)?;
        let trans_key = key_match.as_str().to_string();
        let start_offset = key_match.start();
        let end_offset = key_match.end();
        return Some((trans_key, start_offset, end_offset));
    }

    // Try double quotes
    if let Some(captures) = TRANS_REGEX_DOUBLE.captures(php_content) {
        let key_match = captures.get(1)?;
        let trans_key = key_match.as_str().to_string();
        let start_offset = key_match.start();
        let end_offset = key_match.end();
        return Some((trans_key, start_offset, end_offset));
    }

    None
}

/// Parse @vite directive arguments and extract individual file paths with their positions
///
/// Handles both formats:
/// - @vite('resources/css/app.css')
/// - @vite(['resources/css/app.css', 'resources/js/app.js'])
///
/// Returns Vec of (path, line, column, end_column) for each file path
pub fn parse_vite_directive_assets(
    args: &str,
    directive_row: usize,
    directive_col: usize,
    directive_len: usize,
) -> Vec<(String, u32, u32, u32)> {
    let mut results = Vec::new();

    // The args from tree-sitter typically include the parentheses content
    // e.g., "(['resources/css/app.css', 'resources/js/app.js'])"
    let args = args.trim();

    // Track position within the arguments string
    let mut pos = 0;
    let chars: Vec<char> = args.chars().collect();

    while pos < chars.len() {
        // Find the start of a quoted string
        let quote_char = match chars[pos] {
            '\'' | '"' => chars[pos],
            _ => {
                pos += 1;
                continue;
            }
        };

        let quote_start = pos;
        pos += 1; // Move past opening quote

        // Find the end of the quoted string
        let mut path_chars = Vec::new();
        while pos < chars.len() && chars[pos] != quote_char {
            path_chars.push(chars[pos]);
            pos += 1;
        }

        if pos < chars.len() {
            let path: String = path_chars.into_iter().collect();
            // Calculate column positions for the path content (excluding quotes)
            // directive_col is where @ starts
            // directive_len is length of @vite (5)
            // quote_start is position of opening quote within args (which includes the paren)
            // +1 to skip the opening quote itself and point to the path content
            // +1 more because LSP columns are 0-based but we need to account for the @ symbol position
            let col = (directive_col + directive_len + quote_start + 2) as u32;
            // Empty entries (`@vite('')`) are kept, not dropped: Laravel can't
            // resolve them and throws at build time, so they must be flagged. A
            // zero-width range wouldn't render a squiggle, so span the two quote
            // characters around the (absent) content instead.
            let (col, end_col) = if path.is_empty() {
                (col.saturating_sub(1), col + 1)
            } else {
                (col, col + path.len() as u32) // Just the path, no quotes
            };

            results.push((path, directive_row as u32, col, end_col));
            pos += 1; // Move past closing quote
        }
    }

    results
}

// ============================================================================
// Query Functions - The actual computation
// ============================================================================

/// Parse a source file and extract all Laravel patterns
/// This is automatically memoized by Salsa
///
/// Uses single-pass extraction for performance:
/// - One query compilation (cached globally)
/// - One tree traversal per language (PHP/Blade)
/// - All patterns extracted in O(n) instead of O(n×k)
#[salsa::tracked]
pub fn parse_file_patterns<'db>(db: &'db dyn Db, file: SourceFile) -> ParsedPatterns<'db> {
    use crate::parser::{language_blade, language_php, parse_blade, parse_php};
    use crate::queries::{
        extract_all_blade_patterns, extract_all_php_patterns,
        AssetHelperType as QueryAssetHelperType,
    };

    let text = file.text(db);
    let path = file.path(db);
    let is_blade = path.to_string_lossy().ends_with(".blade.php");

    let mut views = Vec::new();
    let mut components = Vec::new();
    let mut directives = Vec::new();
    let mut env_refs = Vec::new();
    let mut config_refs = Vec::new();
    let mut livewire_refs = Vec::new();
    let mut middleware_refs = Vec::new();
    let mut translation_refs = Vec::new();
    let mut asset_refs = Vec::new();
    let mut binding_refs = Vec::new();

    // Parse Blade files - single pass extraction
    if is_blade {
        if let Ok(tree) = parse_blade(text) {
            let lang = language_blade();

            if let Ok(blade_patterns) = extract_all_blade_patterns(&tree, text, &lang) {
                // Process components
                for comp in blade_patterns.components {
                    let name = ComponentName::new(db, comp.component_name.to_string());
                    let tag = ComponentName::new(db, comp.tag_name.to_string());
                    components.push(ComponentReference::new(
                        db,
                        name,
                        tag,
                        comp.row as u32,
                        comp.column as u32,
                        comp.end_column as u32,
                    ));
                }

                // Process Livewire components
                for lw in blade_patterns.livewire {
                    let name = LivewireName::new(db, lw.component_name.to_string());
                    livewire_refs.push(LivewireReference::new(
                        db,
                        name,
                        lw.row as u32,
                        lw.column as u32,
                        lw.end_column as u32,
                    ));
                }

                // Process directives
                for dir in blade_patterns.directives {
                    // Handle @vite specially - extract individual asset paths
                    if dir.directive_name == "vite" {
                        if let Some(args) = dir.arguments {
                            let vite_assets = parse_vite_directive_assets(
                                args,
                                dir.row,
                                dir.column,
                                dir.directive_name.len() + 1,
                            );
                            for (path, line, col, end_col) in vite_assets {
                                let asset_path = AssetPath::new(db, path);
                                asset_refs.push(AssetReference::new(
                                    db,
                                    asset_path,
                                    AssetHelperType::ViteAsset,
                                    line,
                                    col,
                                    end_col,
                                ));
                            }
                        }
                        continue; // Don't add @vite as a directive
                    }

                    // Handle @lang specially - extract as translation reference
                    if dir.directive_name == "lang" {
                        if let Some(args) = dir.arguments {
                            // Extract the translation key from the arguments
                            // Args look like: ('welcome') or ("welcome")
                            if let Some((trans_key, start_offset, end_offset)) =
                                extract_string_from_args(args)
                            {
                                let key = TranslationKey::new(db, trans_key);
                                // Calculate column positions: directive_column + @lang + offset into args
                                // @lang is 5 chars, plus 1 for @
                                let base_col = dir.column + 6; // position after @lang
                                let col = base_col + start_offset;
                                let end_col = base_col + end_offset;
                                debug!(
                                    "📍 @lang translation: key='{}' row={} col={}-{} (args={:?})",
                                    key.key(db),
                                    dir.row,
                                    col,
                                    end_col,
                                    args
                                );
                                translation_refs.push(TranslationReference::new(
                                    db,
                                    key,
                                    dir.row as u32,
                                    col as u32,
                                    end_col as u32,
                                ));
                            }
                        }
                        continue; // Don't add @lang as a directive
                    }

                    let name = DirectiveName::new(db, dir.directive_name.to_string());
                    let args = dir.arguments.map(|s| s.to_string());
                    let full_end_column = dir.column + dir.full_text.len();
                    directives.push(DirectiveReference::new(
                        db,
                        name,
                        args,
                        dir.row as u32,
                        dir.column as u32,
                        full_end_column as u32,
                        dir.string_column as u32,
                        dir.string_end_column as u32,
                    ));
                }

                // Process PHP content inside {{ ... }} echo statements
                // Extract translation calls like __("Welcome"), trans("key"), etc.
                // (Per-echo logging demoted to debug — at scale, these were
                // tens of thousands of log lines that dominated warming cost.)
                debug!(
                    "🔍 Processing {} echo PHP snippets",
                    blade_patterns.echo_php.len()
                );
                for echo in blade_patterns.echo_php {
                    debug!(
                        "🔍 Echo PHP content: {:?} at row {} col {}",
                        echo.php_content, echo.row, echo.column
                    );
                    if let Some((trans_key, start_offset, end_offset)) =
                        extract_translation_from_echo(echo.php_content)
                    {
                        debug!(
                            "✅ Found translation '{}' at offsets {}-{}",
                            trans_key, start_offset, end_offset
                        );
                        let key = TranslationKey::new(db, trans_key.clone());
                        // Calculate column positions relative to the echo statement
                        let col = echo.column + start_offset;
                        let end_col = echo.column + end_offset;
                        debug!(
                            "📍 Translation ref: row={} col={}-{}",
                            echo.row, col, end_col
                        );
                        translation_refs.push(TranslationReference::new(
                            db,
                            key,
                            echo.row as u32,
                            col as u32,
                            end_col as u32,
                        ));
                    } else {
                        debug!("❌ No translation found in echo content");
                    }
                }
            }
        }
    }

    // For Blade files: every `{{ }}` / `{!! !!}` / `@php` region carries
    // PHP that tree-sitter-php can't recover when given the surrounding
    // Blade syntax. Extract each region individually, re-parse as PHP, and
    // accumulate its patterns into the same Salsa-tracked vectors that the
    // PHP path below populates. Without this, route/view/config/env/...
    // calls inside Blade `{{ }}` are invisible to find-references, hover,
    // and goto-definition.
    if is_blade {
        use crate::blade_embedded_php::{adjust_inner_position, extract_php_regions};
        let regions = extract_php_regions(text);
        let lang_php = language_php();
        for region in regions {
            let wrapped = format!("<?php {}", region.content);
            let Ok(snippet_tree) = parse_php(&wrapped) else {
                continue;
            };
            let Ok(snippet_patterns) = extract_all_php_patterns(&snippet_tree, &wrapped, &lang_php)
            else {
                continue;
            };
            for view in snippet_patterns.views {
                let (line, col) = adjust_inner_position(
                    view.row as u32,
                    view.column as u32,
                    region.row,
                    region.column,
                );
                let (_, end_col) = adjust_inner_position(
                    view.row as u32,
                    view.end_column as u32,
                    region.row,
                    region.column,
                );
                let name = ViewName::new(db, view.view_name.to_string());
                views.push(ViewReference::new(
                    db,
                    name,
                    line,
                    col,
                    end_col,
                    view.is_route_view,
                ));
            }
            for env in snippet_patterns.env_calls {
                let (line, col) = adjust_inner_position(
                    env.row as u32,
                    env.column as u32,
                    region.row,
                    region.column,
                );
                let (_, end_col) = adjust_inner_position(
                    env.row as u32,
                    env.end_column as u32,
                    region.row,
                    region.column,
                );
                let name = EnvVarName::new(db, env.var_name.to_string());
                env_refs.push(EnvReference::new(
                    db,
                    name,
                    env.has_fallback,
                    line,
                    col,
                    end_col,
                ));
            }
            for config in snippet_patterns.config_calls {
                let (line, col) = adjust_inner_position(
                    config.row as u32,
                    config.column as u32,
                    region.row,
                    region.column,
                );
                let (_, end_col) = adjust_inner_position(
                    config.row as u32,
                    config.end_column as u32,
                    region.row,
                    region.column,
                );
                let key = ConfigKey::new(db, config.config_key.to_string());
                config_refs.push(ConfigReference::new(db, key, line, col, end_col));
            }
            for mw in snippet_patterns.middleware_calls {
                let (line, col) = adjust_inner_position(
                    mw.row as u32,
                    mw.column as u32,
                    region.row,
                    region.column,
                );
                let (_, end_col) = adjust_inner_position(
                    mw.row as u32,
                    mw.end_column as u32,
                    region.row,
                    region.column,
                );
                let name = MiddlewareName::new(db, mw.middleware_name.to_string());
                middleware_refs.push(MiddlewareReference::new(db, name, line, col, end_col));
            }
            for trans in snippet_patterns.translation_calls {
                let (line, col) = adjust_inner_position(
                    trans.row as u32,
                    trans.column as u32,
                    region.row,
                    region.column,
                );
                let (_, end_col) = adjust_inner_position(
                    trans.row as u32,
                    trans.end_column as u32,
                    region.row,
                    region.column,
                );
                let key = TranslationKey::new(db, trans.translation_key.to_string());
                translation_refs.push(TranslationReference::new(db, key, line, col, end_col));
            }
            for asset in snippet_patterns.asset_calls {
                let (line, col) = adjust_inner_position(
                    asset.row as u32,
                    asset.column as u32,
                    region.row,
                    region.column,
                );
                let (_, end_col) = adjust_inner_position(
                    asset.row as u32,
                    asset.end_column as u32,
                    region.row,
                    region.column,
                );
                let path = AssetPath::new(db, asset.path.to_string());
                let helper_type = match asset.helper_type {
                    QueryAssetHelperType::Asset => AssetHelperType::Asset,
                    QueryAssetHelperType::PublicPath => AssetHelperType::PublicPath,
                    QueryAssetHelperType::BasePath => AssetHelperType::BasePath,
                    QueryAssetHelperType::AppPath => AssetHelperType::AppPath,
                    QueryAssetHelperType::StoragePath => AssetHelperType::StoragePath,
                    QueryAssetHelperType::DatabasePath => AssetHelperType::DatabasePath,
                    QueryAssetHelperType::LangPath => AssetHelperType::LangPath,
                    QueryAssetHelperType::ConfigPath => AssetHelperType::ConfigPath,
                    QueryAssetHelperType::ResourcePath => AssetHelperType::ResourcePath,
                    QueryAssetHelperType::Mix => AssetHelperType::Mix,
                    QueryAssetHelperType::ViteAsset => AssetHelperType::ViteAsset,
                };
                asset_refs.push(AssetReference::new(
                    db,
                    path,
                    helper_type,
                    line,
                    col,
                    end_col,
                ));
            }
            for binding in snippet_patterns.binding_calls {
                let (line, col) = adjust_inner_position(
                    binding.row as u32,
                    binding.column as u32,
                    region.row,
                    region.column,
                );
                let (_, end_col) = adjust_inner_position(
                    binding.row as u32,
                    binding.end_column as u32,
                    region.row,
                    region.column,
                );
                let name = BindingName::new(db, binding.binding_name.to_string());
                binding_refs.push(BindingReference::new(
                    db,
                    name,
                    binding.is_class_reference,
                    line,
                    col,
                    end_col,
                ));
            }
        }
    }

    // Full-file PHP parse — ONLY for .php files. See pattern_indexer.rs
    // for the rationale: tree-sitter-php on Blade content produces an
    // error tree that the PHP queries walk pathologically slowly on
    // certain real-world inputs (Flux icon SVG path data hit 394ms for
    // a 1.3KB file). All Blade-embedded PHP is extracted above via
    // extract_php_regions + per-region <?php-wrapped parsing.
    if !is_blade {
        if let Ok(tree) = parse_php(text) {
            let lang = language_php();

            if let Ok(php_patterns) = extract_all_php_patterns(&tree, text, &lang) {
                // Process views
                for view in php_patterns.views {
                    let name = ViewName::new(db, view.view_name.to_string());
                    views.push(ViewReference::new(
                        db,
                        name,
                        view.row as u32,
                        view.column as u32,
                        view.end_column as u32,
                        view.is_route_view,
                    ));
                }

                // Process env calls
                for env in php_patterns.env_calls {
                    let name = EnvVarName::new(db, env.var_name.to_string());
                    env_refs.push(EnvReference::new(
                        db,
                        name,
                        env.has_fallback,
                        env.row as u32,
                        env.column as u32,
                        env.end_column as u32,
                    ));
                }

                // Process config calls
                for config in php_patterns.config_calls {
                    let key = ConfigKey::new(db, config.config_key.to_string());
                    config_refs.push(ConfigReference::new(
                        db,
                        key,
                        config.row as u32,
                        config.column as u32,
                        config.end_column as u32,
                    ));
                }

                // Process middleware calls
                for mw in php_patterns.middleware_calls {
                    let name = MiddlewareName::new(db, mw.middleware_name.to_string());
                    middleware_refs.push(MiddlewareReference::new(
                        db,
                        name,
                        mw.row as u32,
                        mw.column as u32,
                        mw.end_column as u32,
                    ));
                }

                // Process translation calls
                for trans in php_patterns.translation_calls {
                    let key = TranslationKey::new(db, trans.translation_key.to_string());
                    translation_refs.push(TranslationReference::new(
                        db,
                        key,
                        trans.row as u32,
                        trans.column as u32,
                        trans.end_column as u32,
                    ));
                }

                // Process asset calls
                for asset in php_patterns.asset_calls {
                    let path = AssetPath::new(db, asset.path.to_string());
                    let helper_type = match asset.helper_type {
                        QueryAssetHelperType::Asset => AssetHelperType::Asset,
                        QueryAssetHelperType::PublicPath => AssetHelperType::PublicPath,
                        QueryAssetHelperType::BasePath => AssetHelperType::BasePath,
                        QueryAssetHelperType::AppPath => AssetHelperType::AppPath,
                        QueryAssetHelperType::StoragePath => AssetHelperType::StoragePath,
                        QueryAssetHelperType::DatabasePath => AssetHelperType::DatabasePath,
                        QueryAssetHelperType::LangPath => AssetHelperType::LangPath,
                        QueryAssetHelperType::ConfigPath => AssetHelperType::ConfigPath,
                        QueryAssetHelperType::ResourcePath => AssetHelperType::ResourcePath,
                        QueryAssetHelperType::Mix => AssetHelperType::Mix,
                        QueryAssetHelperType::ViteAsset => AssetHelperType::ViteAsset,
                    };
                    asset_refs.push(AssetReference::new(
                        db,
                        path,
                        helper_type,
                        asset.row as u32,
                        asset.column as u32,
                        asset.end_column as u32,
                    ));
                }

                // Process binding calls
                for binding in php_patterns.binding_calls {
                    let name = BindingName::new(db, binding.binding_name.to_string());
                    binding_refs.push(BindingReference::new(
                        db,
                        name,
                        binding.is_class_reference,
                        binding.row as u32,
                        binding.column as u32,
                        binding.end_column as u32,
                    ));
                }

                // Note: route_refs, url_refs, action_refs are extracted in handle_get_patterns
                // to keep ParsedPatterns field count under Salsa's 12-element limit
            }
        }
    } // end if !is_blade

    ParsedPatterns::new(
        db,
        file,
        views,
        components,
        directives,
        env_refs,
        config_refs,
        livewire_refs,
        middleware_refs,
        translation_refs,
        asset_refs,
        binding_refs,
    )
}

/// Parse composer.json to detect installed packages
/// Returns (has_livewire, list of installed packages)
#[salsa::tracked]
pub fn parse_composer_json(db: &dyn Db, file: ConfigFile) -> (bool, Vec<String>) {
    let text = file.text(db);

    // Parse JSON to detect Livewire
    let has_livewire = text.contains("\"livewire/livewire\"");

    // Extract package names from require and require-dev
    let mut packages = Vec::new();

    // Simple extraction - look for package patterns in require sections
    // This is a simplified version; could use serde_json for full parsing
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('"') && trimmed.contains('/') && trimmed.contains(':') {
            // Extract package name from "vendor/package": "version"
            if let Some(end) = trimmed.find(':') {
                let name = trimmed[1..end - 1].to_string();
                if name.contains('/') {
                    packages.push(name);
                }
            }
        }
    }

    (has_livewire, packages)
}

/// Parse config/view.php to extract view paths
#[salsa::tracked]
pub fn parse_view_config(db: &dyn Db, file: ConfigFile, root: PathBuf) -> Vec<PathBuf> {
    let text = file.text(db);
    let mut paths = Vec::new();

    // Look for resource_path('views') or base_path('some/path') patterns
    // This reuses logic from config.rs but in a Salsa-compatible way

    // Default: resources/views
    if text.contains("resource_path") && text.contains("views") {
        paths.push(root.join("resources/views"));
    }

    // Look for base_path calls
    for line in text.lines() {
        if line.contains("base_path") {
            // Extract path from base_path('path')
            if let Some(start) = line.find("base_path(") {
                let rest = &line[start + 10..];
                if let Some(quote_start) = rest.find(['\'', '"']) {
                    let quote_char = rest.chars().nth(quote_start).unwrap();
                    let path_start = quote_start + 1;
                    if let Some(quote_end) = rest[path_start..].find(quote_char) {
                        let path_str = &rest[path_start..path_start + quote_end];
                        paths.push(root.join(path_str));
                    }
                }
            }
        }
    }

    // If no paths found, use default
    if paths.is_empty() {
        paths.push(root.join("resources/views"));
    }

    paths
}

/// Parse a Blade file's loop-block structure (@foreach / @forelse / @for / @while).
/// Memoized: only re-runs when the file's text changes.
#[salsa::tracked]
pub fn parse_blade_loop_blocks(
    db: &dyn Db,
    file: SourceFile,
) -> Vec<crate::blade_loops::BladeLoopBlock> {
    let text = file.text(db);
    crate::blade_loops::find_loop_blocks(text)
}

/// Parse simple `$name = ...;` assignments out of a Blade file's `@php ... @endphp` blocks.
/// Memoized: only re-runs when the file's text changes.
#[salsa::tracked]
pub fn parse_blade_php_assignments(db: &dyn Db, file: SourceFile) -> Vec<(String, String)> {
    let text = file.text(db);
    crate::blade_php_block::extract_php_block_assignments(text)
}

/// Extract the document-symbol tree for a file (route file, Blade template,
/// Livewire component, or Eloquent model). Returns an empty vec for other file
/// kinds. Memoized: only re-runs when the file's text changes.
#[salsa::tracked]
pub fn extract_document_symbols(
    db: &dyn Db,
    file: SourceFile,
) -> Vec<crate::document_symbols::SymbolEntry> {
    let path = file.path(db);
    let text = file.text(db);
    let kind = crate::document_symbols::classify_file(path);
    crate::document_symbols::extract_symbols(text, kind)
}

/// Resolve a `$this->X` member access against a Livewire component's PHP file.
/// Tries property type first, then method return type. Memoized per (file_version, member).
#[salsa::tracked]
pub fn resolve_livewire_member_type(
    db: &dyn Db,
    file: SourceFile,
    member: String,
) -> Option<String> {
    let text = file.text(db);
    crate::php_class::resolve_member_type(text, &member)
}

/// Parse config/livewire.php to extract Livewire component path
#[salsa::tracked]
pub fn parse_livewire_config(db: &dyn Db, file: ConfigFile, root: PathBuf) -> Option<PathBuf> {
    let text = file.text(db);

    // Look for class_namespace patterns
    if text.contains("App\\Livewire") || text.contains("App\\\\Livewire") {
        return Some(root.join("app/Livewire"));
    }

    if text.contains("App\\Http\\Livewire") || text.contains("App\\\\Http\\\\Livewire") {
        return Some(root.join("app/Http/Livewire"));
    }

    None
}

/// Build complete Laravel configuration from individual config files
#[salsa::tracked]
pub fn build_laravel_config<'db>(
    db: &'db dyn Db,
    root: PathBuf,
    composer: Option<ConfigFile>,
    view_config: Option<ConfigFile>,
    livewire_config: Option<ConfigFile>,
) -> LaravelConfigRef<'db> {
    // Parse composer.json for Livewire detection
    let has_livewire = composer
        .map(|f| parse_composer_json(db, f).0)
        .unwrap_or(false);

    // Parse view config for view paths
    let view_paths = view_config
        .map(|f| parse_view_config(db, f, root.clone()))
        .unwrap_or_else(|| vec![root.join("resources/views")]);

    // Build component paths from view paths
    let component_paths: Vec<(String, PathBuf)> = view_paths
        .iter()
        .map(|p| (String::new(), p.join("components")))
        .collect();

    // Parse livewire config for component path
    let livewire_path = if has_livewire {
        livewire_config
            .and_then(|f| parse_livewire_config(db, f, root.clone()))
            .or_else(|| {
                // Default Livewire paths
                let v3_path = root.join("app/Livewire");
                let v2_path = root.join("app/Http/Livewire");
                if v3_path.exists() {
                    Some(v3_path)
                } else if v2_path.exists() {
                    Some(v2_path)
                } else {
                    Some(v3_path) // Default to v3 path
                }
            })
    } else {
        None
    };

    LaravelConfigRef::new(
        db,
        root,
        view_paths,
        component_paths,
        livewire_path,
        has_livewire,
    )
}

// ============================================================================
// Environment Variable Parsing (Salsa-based)
// ============================================================================

/// A parsed environment variable (Salsa tracked)
#[salsa::tracked]
pub struct ParsedEnvVar<'db> {
    /// Variable name
    pub name: EnvVarName<'db>,
    /// Variable value
    #[returns(ref)]
    pub value: String,
    /// Line number in source file (0-indexed)
    pub line: u32,
    /// Column of the variable name
    pub column: u32,
    /// Column where value starts
    pub value_column: u32,
    /// Whether this variable is commented out
    pub is_commented: bool,
    /// Priority of the source file (higher wins)
    pub priority: u8,
    /// Source file path
    #[returns(ref)]
    pub source_file: PathBuf,
}

/// Parse an environment file and extract all variables
#[salsa::tracked]
pub fn parse_env_source<'db>(db: &'db dyn Db, file: EnvFile) -> Vec<ParsedEnvVar<'db>> {
    let text = file.text(db);
    let path = file.path(db);
    let priority = file.priority(db);
    let mut variables = Vec::new();

    for (line_idx, line) in text.lines().enumerate() {
        // Skip empty lines
        if line.trim().is_empty() {
            continue;
        }

        // Check if line is commented
        let is_commented = line.trim_start().starts_with('#');
        let working_line = if is_commented {
            line.trim_start().trim_start_matches('#').trim_start()
        } else {
            line
        };

        // Parse VAR=value format
        if let Some((name_part, value_part)) = working_line.split_once('=') {
            let name = name_part.trim();

            // Skip if not a valid variable name
            if name.is_empty() || name.contains(' ') {
                continue;
            }

            // Parse the value, handling quotes
            let value = parse_env_value_internal(value_part.trim());

            // Calculate column positions
            let name_column = line.find(name).unwrap_or(0) as u32;
            let value_column = line
                .find('=')
                .map(|pos| pos + 1)
                .unwrap_or(name_column as usize) as u32;

            let var_name = EnvVarName::new(db, name.to_string());
            variables.push(ParsedEnvVar::new(
                db,
                var_name,
                value,
                line_idx as u32,
                name_column,
                value_column,
                is_commented,
                priority,
                path.clone(),
            ));
        }
    }

    variables
}

/// Parse an environment variable value, handling quotes
fn parse_env_value_internal(value: &str) -> String {
    let value = value.trim();

    // Handle quoted values
    if (value.starts_with('"') && value.ends_with('"'))
        || (value.starts_with('\'') && value.ends_with('\''))
    {
        // Remove quotes
        if value.len() >= 2 {
            return value[1..value.len() - 1].to_string();
        }
    }

    // Handle inline comments (# at end of line)
    if let Some(hash_pos) = value.find(" #") {
        return value[..hash_pos].trim().to_string();
    }

    value.to_string()
}

// ============================================================================
// Service Provider Parsing (Salsa-based)
// ============================================================================

/// Binding type for container bindings
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
pub enum BindingTypeEnum {
    Singleton,
    Bind,
    Instance,
    Alias,
}

/// A parsed middleware registration (Salsa tracked)
#[salsa::tracked]
pub struct ParsedMiddlewareReg<'db> {
    /// Middleware alias (e.g., "auth", "throttle")
    pub alias: MiddlewareName<'db>,
    /// Full class name
    #[returns(ref)]
    pub class_name: String,
    /// Resolved file path (if found)
    #[returns(ref)]
    pub file_path: Option<PathBuf>,
    /// Line in source file where registered
    pub source_line: u32,
    /// Priority (0=framework, 1=package, 2=app)
    pub priority: u8,
    /// Source file where registered
    #[returns(ref)]
    pub source_file: PathBuf,
}

/// A parsed container binding (Salsa tracked)
#[salsa::tracked]
pub struct ParsedBindingReg<'db> {
    /// Abstract name or interface
    pub abstract_name: BindingName<'db>,
    /// Concrete class name
    #[returns(ref)]
    pub concrete_class: String,
    /// Resolved file path (if found)
    #[returns(ref)]
    pub file_path: Option<PathBuf>,
    /// Type of binding
    pub binding_type: BindingTypeEnum,
    /// Line in source file where registered
    pub source_line: u32,
    /// Priority (0=framework, 1=package, 2=app)
    pub priority: u8,
    /// Source file where registered
    #[returns(ref)]
    pub source_file: PathBuf,
}

/// A parsed view namespace registration from loadViewsFrom() (Salsa tracked)
/// Example: $this->loadViewsFrom(__DIR__.'/../resources/views', 'courier')
#[salsa::tracked]
pub struct ParsedViewNamespaceReg<'db> {
    /// Package namespace (e.g., "courier")
    pub namespace: PackageNamespace<'db>,
    /// Resolved view path (if found)
    #[returns(ref)]
    pub view_path: Option<PathBuf>,
    /// Line in source file where registered
    pub source_line: u32,
    /// Priority (0=framework, 1=package, 2=app)
    pub priority: u8,
    /// Source file where registered
    #[returns(ref)]
    pub source_file: PathBuf,
}

/// A parsed Blade component registration from Blade::component() (Salsa tracked)
/// Example: Blade::component('package-alert', AlertComponent::class)
#[salsa::tracked]
pub struct ParsedBladeComponentReg<'db> {
    /// Component tag name (e.g., "package-alert")
    pub tag_name: ComponentName<'db>,
    /// Full class name
    #[returns(ref)]
    pub class_name: String,
    /// Resolved file path (if found)
    #[returns(ref)]
    pub file_path: Option<PathBuf>,
    /// Line in source file where registered
    pub source_line: u32,
    /// Priority (0=framework, 1=package, 2=app)
    pub priority: u8,
    /// Source file where registered
    #[returns(ref)]
    pub source_file: PathBuf,
}

/// A parsed component namespace registration from Blade::componentNamespace() (Salsa tracked)
/// Example: Blade::componentNamespace('Nightshade\\Views\\Components', 'nightshade')
#[salsa::tracked]
pub struct ParsedComponentNamespaceReg<'db> {
    /// Component namespace prefix (e.g., "nightshade")
    pub prefix: PackageNamespace<'db>,
    /// PHP namespace (e.g., "Nightshade\\Views\\Components")
    #[returns(ref)]
    pub php_namespace: String,
    /// Line in source file where registered
    pub source_line: u32,
    /// Priority (0=framework, 1=package, 2=app)
    pub priority: u8,
    /// Source file where registered
    #[returns(ref)]
    pub source_file: PathBuf,
}

/// A parsed anonymous component path registration from
/// Blade::anonymousComponentPath() (Salsa tracked).
/// Example: Blade::anonymousComponentPath(resource_path('views/backstage/components'), 'backstage')
#[salsa::tracked]
pub struct ParsedAnonymousComponentPathReg<'db> {
    /// Component prefix (e.g., "backstage")
    pub prefix: PackageNamespace<'db>,
    /// Resolved absolute directory holding the anonymous components
    #[returns(ref)]
    pub directory: PathBuf,
    /// Line in source file where registered
    pub source_line: u32,
    /// Priority (0=framework, 1=package, 2=app)
    pub priority: u8,
    /// Source file where registered
    #[returns(ref)]
    pub source_file: PathBuf,
}

/// A parsed anonymous component namespace registration from
/// Blade::anonymousComponentNamespace() (Salsa tracked).
/// Example: Blade::anonymousComponentNamespace('components.flux', 'flux')
#[salsa::tracked]
pub struct ParsedAnonymousComponentNamespaceReg<'db> {
    /// Component prefix (e.g., "flux")
    pub prefix: PackageNamespace<'db>,
    /// Directory relative to the view paths (dots normalized to slashes)
    #[returns(ref)]
    pub directory: String,
    /// Line in source file where registered
    pub source_line: u32,
    /// Priority (0=framework, 1=package, 2=app)
    pub priority: u8,
    /// Source file where registered
    #[returns(ref)]
    pub source_file: PathBuf,
}

/// Parsed service provider content
#[salsa::tracked]
pub struct ParsedServiceProvider<'db> {
    /// Middleware registrations found in this provider
    #[returns(ref)]
    pub middleware: Vec<ParsedMiddlewareReg<'db>>,
    /// Container bindings found in this provider
    #[returns(ref)]
    pub bindings: Vec<ParsedBindingReg<'db>>,
    /// View namespace registrations from loadViewsFrom()
    #[returns(ref)]
    pub view_namespaces: Vec<ParsedViewNamespaceReg<'db>>,
    /// Manual Blade component registrations from Blade::component()
    #[returns(ref)]
    pub blade_components: Vec<ParsedBladeComponentReg<'db>>,
    /// Component namespace registrations from Blade::componentNamespace()
    #[returns(ref)]
    pub component_namespaces: Vec<ParsedComponentNamespaceReg<'db>>,
    /// Anonymous component path registrations from Blade::anonymousComponentPath()
    #[returns(ref)]
    pub anonymous_component_paths: Vec<ParsedAnonymousComponentPathReg<'db>>,
    /// Anonymous component namespace registrations from Blade::anonymousComponentNamespace()
    #[returns(ref)]
    pub anonymous_component_namespaces: Vec<ParsedAnonymousComponentNamespaceReg<'db>>,
}

/// Parse a service provider file and extract middleware, bindings, views, and components
#[salsa::tracked]
pub fn parse_service_provider_source<'db>(
    db: &'db dyn Db,
    file: ServiceProviderFile,
    root: PathBuf,
) -> ParsedServiceProvider<'db> {
    use lazy_static::lazy_static;
    use regex::Regex;

    lazy_static! {
        /// Matches `$this->app->bind('name', Class::class)` or
        /// `$this->app->singleton('name', Class::class)` where the concrete
        /// is given as a static class reference.
        ///
        /// Two earlier shapes — `..., function () { ... })` (closure concrete)
        /// and `..., $variable` (variable concrete) — are matched separately
        /// below so they can be classified rather than mis-extracted as a
        /// class name. The original combined regex back-tracked into closure
        /// parameter lists (e.g. `function ($app)`) and pulled `"p"` out of
        /// `$app`, which is the bug being fixed here.
        static ref BINDING_CLASS_RE: Regex = Regex::new(
            r#"\$this->app->(bind|singleton)\s*\(\s*['"]([^'"]+)['"]\s*,\s*\\?([A-Za-z0-9_\\]+)::class"#
        ).unwrap();

        /// Matches `$this->app->bind('name', function ...)` or `fn (...) =>`.
        /// The concrete in this case is a closure — we record `"Closure"`
        /// rather than trying to derive a class name.
        static ref BINDING_CLOSURE_RE: Regex = Regex::new(
            r#"\$this->app->(bind|singleton)\s*\(\s*['"]([^'"]+)['"]\s*,\s*(?:function\s*\(|fn\s*\()"#
        ).unwrap();

        /// Matches a bare `$this->app->bind('name')` or `->singleton('name')`
        /// — no second argument. Falls back to abstract = concrete in the
        /// handler.
        static ref BINDING_BARE_RE: Regex = Regex::new(
            r#"\$this->app->(bind|singleton)\s*\(\s*['"]([^'"]+)['"]\s*\)"#
        ).unwrap();

        /// Matches $this->app->alias('concrete', 'alias')
        static ref ALIAS_RE: Regex = Regex::new(
            r#"\$this->app->alias\s*\(\s*\\?([A-Za-z0-9_\\]+)(?:::class)?\s*,\s*['"]([^'"]+)['"]\s*\)"#
        ).unwrap();

        /// Matches $this->loadViewsFrom(__DIR__.'/../path', 'namespace')
        static ref LOAD_VIEWS_RE: Regex = Regex::new(
            r#"\$this->loadViewsFrom\s*\(\s*__DIR__\s*\.\s*['"]([^'"]+)['"]\s*,\s*['"]([^'"]+)['"]\s*\)"#
        ).unwrap();

        /// Matches a class-backed component registration with the tag first:
        /// `Blade::component('tag-name', Class::class)` — facade form — or
        /// `$blade->component('tag-name', Class::class)` — the instance form
        /// the framework itself uses (ViewServiceProvider registers
        /// `dynamic-component` on the compiler instance inside a tap() closure).
        static ref BLADE_COMPONENT_RE: Regex = Regex::new(
            r#"(?:Blade::|\$\w+->)component\s*\(\s*['"]([^'"]+)['"]\s*,\s*\\?([A-Za-z0-9_\\]+)::class\s*\)"#
        ).unwrap();

        /// Same registration with the canonical argument order:
        /// `component(Class::class, 'tag-name')`. `BladeCompiler::component`
        /// accepts both orders and swaps internally; statically the `::class`
        /// suffix marks which argument is the class, so we match each order
        /// with its own pattern.
        static ref BLADE_COMPONENT_CLASS_FIRST_RE: Regex = Regex::new(
            r#"(?:Blade::|\$\w+->)component\s*\(\s*\\?([A-Za-z0-9_\\]+)::class\s*,\s*['"]([^'"]+)['"]\s*\)"#
        ).unwrap();

        /// Matches a class-backed registration whose tag is a config-driven
        /// prefix concatenation: `Blade::component($prefix . 'card', Card::class)`.
        /// MaryUI registers its whole catalog this way, reading the prefix
        /// from `config('mary.prefix')` once at the top of the method.
        static ref BLADE_COMPONENT_PREFIXED_RE: Regex = Regex::new(
            r#"(?:Blade::|\$\w+->)component\s*\(\s*\$(\w+)\s*\.\s*['"]([^'"]+)['"]\s*,\s*\\?([A-Za-z0-9_\\]+)::class\s*\)"#
        ).unwrap();

        /// Matches the prefix-variable assignment feeding the form above:
        /// `$prefix = config('mary.prefix');` (with or without a default arg).
        static ref CONFIG_VAR_ASSIGN_RE: Regex = Regex::new(
            r#"\$(\w+)\s*=\s*config\(\s*['"]([\w.-]+)['"]\s*[,)]"#
        ).unwrap();

        /// Matches Blade::componentNamespace('Namespace\\Path', 'prefix')
        static ref COMPONENT_NAMESPACE_RE: Regex = Regex::new(
            r#"Blade::componentNamespace\s*\(\s*['"]([^'"]+)['"]\s*,\s*['"]([^'"]+)['"]\s*\)"#
        ).unwrap();

        /// Matches a fluent package-builder name declaration: `->name('package')`.
        /// The literal `loadViewsFrom`/`loadTranslationsFrom` patterns above only
        /// see Laravel-native registration. Builder-convention providers (the
        /// dominant one being laravel-package-tools, but this is form-based, not
        /// vendor-tied) declare capabilities fluently — `->name('x')->hasViews()`
        /// — and the real `loadViewsFrom($computedDir, $name)` runs in a base
        /// class with runtime args the literal patterns can't see. This pair of
        /// patterns reconstructs that registration form.
        static ref BUILDER_NAME_RE: Regex = Regex::new(
            r#"->name\s*\(\s*['"]([^'"]+)['"]\s*\)"#
        ).unwrap();

        /// Matches the builder view capability: `->hasViews()` or
        /// `->hasViews('explicit-namespace')`. The optional capture is the
        /// namespace override; absent, the namespace is the package short-name.
        static ref BUILDER_HAS_VIEWS_RE: Regex = Regex::new(
            r#"->hasViews\s*\(\s*(?:['"]([^'"]+)['"])?\s*\)"#
        ).unwrap();
    }

    let text = file.text(db);
    let path = file.path(db);
    let priority = file.priority(db);

    let mut middleware = Vec::new();
    let mut bindings = Vec::new();
    let mut view_namespaces = Vec::new();
    let mut blade_components = Vec::new();
    let mut component_namespaces = Vec::new();

    // Parse middleware using tree-sitter for accurate context-aware extraction
    if let Ok(tree) = parse_php(text) {
        let language = language_php();
        if let Ok(patterns) = extract_all_php_patterns(&tree, text, &language) {
            tracing::debug!(
                "📦 Parsing {:?}: {} alias defs, {} group defs",
                path,
                patterns.middleware_alias_defs.len(),
                patterns.middleware_group_defs.len()
            );
            // Process middleware alias definitions (from $middlewareAliases property)
            for alias_def in &patterns.middleware_alias_defs {
                let class_str = alias_def.class_name.trim_start_matches('\\');
                let file_path = resolve_class_to_file_internal(class_str, &root);

                let alias_name = MiddlewareName::new(db, alias_def.alias.to_string());
                middleware.push(ParsedMiddlewareReg::new(
                    db,
                    alias_name,
                    class_str.to_string(),
                    file_path,
                    // Tree-sitter's row is 0-based, but the
                    // `source_line` field is 1-based by convention
                    // (matches binding source_line and the goto-def
                    // consumer that subtracts 1). +1 to convert.
                    alias_def.row as u32 + 1,
                    priority,
                    path.clone(),
                ));
            }

            // Process middleware group definitions (from $middlewareGroups property)
            // Track existing aliases to avoid duplicates
            let existing_aliases: std::collections::HashSet<String> = middleware
                .iter()
                .map(|m| m.alias(db).name(db).to_string())
                .collect();

            for group_def in &patterns.middleware_group_defs {
                // Skip if already registered as an alias
                if existing_aliases.contains(group_def.group_name) {
                    continue;
                }

                tracing::debug!("   Found group: '{}'", group_def.group_name);
                let alias_name = MiddlewareName::new(db, group_def.group_name.to_string());
                middleware.push(ParsedMiddlewareReg::new(
                    db,
                    alias_name,
                    format!("MiddlewareGroup<{}>", group_def.group_name), // Placeholder to indicate it's a group
                    None, // Groups don't have a single file
                    // Same 0-based → 1-based correction as the alias
                    // branch above; goto-def + the rename locator both
                    // expect 1-based source_line.
                    group_def.row as u32 + 1,
                    priority,
                    path.clone(),
                ));
            }

            if !middleware.is_empty() {
                tracing::info!(
                    "🔐 Extracted {} middleware from {:?}: {:?}",
                    middleware.len(),
                    path,
                    middleware
                        .iter()
                        .map(|m| m.alias(db).name(db).to_string())
                        .collect::<Vec<_>>()
                );
            }
        }
    }

    // Parse bind/singleton registrations. Three regexes target the three
    // forms a binding's concrete can take — explicit class, closure, or no
    // second argument. Each abstract name is registered exactly once, with
    // the class regex taking precedence over closure, which takes precedence
    // over bare (no second arg). The earlier combined regex back-tracked
    // into closure parameter lists and pulled garbage like `"p"` out of
    // `function ($app)`; the three narrower regexes can't do that.
    let mut bindings_seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for cap in BINDING_CLASS_RE.captures_iter(text) {
        if let (Some(method), Some(name), Some(concrete)) = (cap.get(1), cap.get(2), cap.get(3)) {
            let abstract_name = name.as_str();
            if !bindings_seen.insert(abstract_name.to_string()) {
                continue;
            }
            let concrete_class = concrete.as_str().trim_start_matches('\\').to_string();
            let binding_type = match method.as_str() {
                "singleton" => BindingTypeEnum::Singleton,
                _ => BindingTypeEnum::Bind,
            };
            let line = text[..name.start()].lines().count() as u32;
            let file_path = resolve_class_to_file_internal(&concrete_class, &root);
            let binding_name = BindingName::new(db, abstract_name.to_string());
            bindings.push(ParsedBindingReg::new(
                db,
                binding_name,
                concrete_class,
                file_path,
                binding_type,
                line,
                priority,
                path.clone(),
            ));
        }
    }

    for cap in BINDING_CLOSURE_RE.captures_iter(text) {
        if let (Some(method), Some(name)) = (cap.get(1), cap.get(2)) {
            let abstract_name = name.as_str();
            if !bindings_seen.insert(abstract_name.to_string()) {
                continue;
            }
            let binding_type = match method.as_str() {
                "singleton" => BindingTypeEnum::Singleton,
                _ => BindingTypeEnum::Bind,
            };
            let line = text[..name.start()].lines().count() as u32;
            let binding_name = BindingName::new(db, abstract_name.to_string());
            bindings.push(ParsedBindingReg::new(
                db,
                binding_name,
                "Closure".to_string(),
                None,
                binding_type,
                line,
                priority,
                path.clone(),
            ));
        }
    }

    for cap in BINDING_BARE_RE.captures_iter(text) {
        if let (Some(method), Some(name)) = (cap.get(1), cap.get(2)) {
            let abstract_name = name.as_str();
            if !bindings_seen.insert(abstract_name.to_string()) {
                continue;
            }
            let binding_type = match method.as_str() {
                "singleton" => BindingTypeEnum::Singleton,
                _ => BindingTypeEnum::Bind,
            };
            let line = text[..name.start()].lines().count() as u32;
            let file_path = resolve_class_to_file_internal(abstract_name, &root);
            let binding_name = BindingName::new(db, abstract_name.to_string());
            bindings.push(ParsedBindingReg::new(
                db,
                binding_name,
                abstract_name.to_string(),
                file_path,
                binding_type,
                line,
                priority,
                path.clone(),
            ));
        }
    }

    // Parse alias registrations
    for cap in ALIAS_RE.captures_iter(text) {
        if let (Some(concrete), Some(alias)) = (cap.get(1), cap.get(2)) {
            let concrete_class = concrete.as_str().trim_start_matches('\\');
            let alias_name = alias.as_str();

            let line = text[..alias.start()].lines().count() as u32;
            let file_path = resolve_class_to_file_internal(concrete_class, &root);

            let binding_name = BindingName::new(db, alias_name.to_string());
            bindings.push(ParsedBindingReg::new(
                db,
                binding_name,
                concrete_class.to_string(),
                file_path,
                BindingTypeEnum::Alias,
                line,
                priority,
                path.clone(),
            ));
        }
    }

    // Parse loadViewsFrom() registrations
    // Example: $this->loadViewsFrom(__DIR__.'/../resources/views', 'courier')
    for cap in LOAD_VIEWS_RE.captures_iter(text) {
        if let (Some(relative_path), Some(namespace)) = (cap.get(1), cap.get(2)) {
            let relative_path_str = relative_path.as_str();
            let namespace_str = namespace.as_str();

            let line = text[..namespace.start()].lines().count() as u32;

            // Resolve __DIR__ + relative path
            // __DIR__ is the directory containing the service provider file
            let provider_dir = path.parent().unwrap_or(path.as_path());
            let view_path = provider_dir.join(relative_path_str);
            let resolved_path = if view_path.exists() {
                Some(view_path.canonicalize().unwrap_or(view_path))
            } else {
                // For non-existent paths, store the normalized form so the
                // diagnostic can show the expected location even if it
                // doesn't resolve on disk yet.
                Some(normalize_path(&view_path))
            };

            let pkg_namespace = PackageNamespace::new(db, namespace_str.to_string());
            view_namespaces.push(ParsedViewNamespaceReg::new(
                db,
                pkg_namespace,
                resolved_path,
                line,
                priority,
                path.clone(),
            ));
        }
    }

    // Parse imperative View-factory namespace registrations:
    //   View::addNamespace('ai-prompts', app_path('Ai/Prompts'))
    //   app('view')->prependNamespace('ns', resource_path('views/ns'))
    // Unlike loadViewsFrom() these take a Laravel path helper rather than a
    // __DIR__ concatenation, so the directory is resolved via the shared
    // path-expression resolver.
    {
        let provider_dir = path.parent().unwrap_or(path.as_path());
        for (namespace, directory, line) in
            extract_add_namespace_view_registrations(text, &root, provider_dir)
        {
            let pkg_namespace = PackageNamespace::new(db, namespace);
            view_namespaces.push(ParsedViewNamespaceReg::new(
                db,
                pkg_namespace,
                Some(directory),
                line,
                priority,
                path.clone(),
            ));
        }
    }

    // Parse the fluent package-builder view registration form:
    //   $package->name('filament')->hasViews();
    // Builder-convention providers register views from a base class
    // (`loadViewsFrom($computedDir, $name)`) with runtime-computed arguments, so
    // the literal LOAD_VIEWS_RE above never sees them. Reconstruct the
    // (namespace, directory) pair from the convention: the namespace is the
    // explicit `->hasViews('ns')` argument or the package short-name (a leading
    // `laravel-` stripped, matching the builder's own `shortName()` rule), and
    // the directory is the package's `resources/views` — one level up from the
    // provider's `src/` dir, which is where these builders resolve their base
    // path. The capability (`->hasViews(`) gates this: without it the provider
    // isn't registering views, so a stray `->name(` elsewhere can't misfire.
    if let Some(has_views) = BUILDER_HAS_VIEWS_RE.captures(text) {
        if let Some(name_cap) = BUILDER_NAME_RE.captures(text) {
            let package_name = name_cap.get(1).map(|m| m.as_str()).unwrap_or("");
            let namespace = has_views
                .get(1)
                .map(|m| m.as_str().to_string())
                .unwrap_or_else(|| builder_short_name(package_name));

            if !namespace.is_empty() {
                let line = text[..name_cap.get(0).map(|m| m.start()).unwrap_or(0)]
                    .lines()
                    .count() as u32;

                // Convention: provider in `<pkg>/src`, views at `<pkg>/resources/views`.
                let provider_dir = path.parent().unwrap_or(path.as_path());
                let view_path = provider_dir.join("../resources/views");
                let resolved_path = if view_path.exists() {
                    Some(view_path.canonicalize().unwrap_or(view_path))
                } else {
                    Some(normalize_path(&view_path))
                };

                let pkg_namespace = PackageNamespace::new(db, namespace);
                view_namespaces.push(ParsedViewNamespaceReg::new(
                    db,
                    pkg_namespace,
                    resolved_path,
                    line,
                    priority,
                    path.clone(),
                ));
            }
        }
    }

    // Parse class-backed component registrations, both argument orders and
    // both receivers (Blade:: facade and $instance->):
    //   Blade::component('package-alert', AlertComponent::class)
    //   $blade->component('dynamic-component', DynamicComponent::class)
    //   Blade::component(AlertComponent::class, 'alert')
    // A bare class name (`DynamicComponent::class`) is expanded to its FQN via
    // the file's `use` statements before file resolution, mirroring how PHP
    // itself resolves the reference.
    {
        let mut push_blade_component =
            |tag_name_str: &str, tag_offset: usize, class_match: regex::Match| {
                let class_str = expand_class_via_use_statements(
                    class_match.as_str().trim_start_matches('\\'),
                    text,
                );

                let line = text[..tag_offset].lines().count() as u32;
                let file_path = resolve_class_to_file_internal(&class_str, &root);

                let component_name = ComponentName::new(db, tag_name_str.to_string());
                blade_components.push(ParsedBladeComponentReg::new(
                    db,
                    component_name,
                    class_str,
                    file_path,
                    line,
                    priority,
                    path.clone(),
                ));
            };

        for cap in BLADE_COMPONENT_RE.captures_iter(text) {
            if let (Some(tag_name), Some(class)) = (cap.get(1), cap.get(2)) {
                push_blade_component(tag_name.as_str(), tag_name.start(), class);
            }
        }
        for cap in BLADE_COMPONENT_CLASS_FIRST_RE.captures_iter(text) {
            if let (Some(class), Some(tag_name)) = (cap.get(1), cap.get(2)) {
                push_blade_component(tag_name.as_str(), tag_name.start(), class);
            }
        }

        // Prefix-computed registrations (MaryUI's catalog form):
        //   $prefix = config('mary.prefix');
        //   Blade::component($prefix . 'card', Card::class);
        // The tag only exists after concatenating a config value, so resolve
        // the variable's config key from the same file and read the value the
        // way Laravel would at boot — the app's config override wins, else the
        // package's bundled config default. A key neither defines is PHP null,
        // which string-concatenates to '' (MaryUI's actual default).
        let config_vars: HashMap<&str, &str> = CONFIG_VAR_ASSIGN_RE
            .captures_iter(text)
            .filter_map(|cap| match (cap.get(1), cap.get(2)) {
                (Some(var), Some(key)) => Some((var.as_str(), key.as_str())),
                _ => None,
            })
            .collect();
        if !config_vars.is_empty() {
            for cap in BLADE_COMPONENT_PREFIXED_RE.captures_iter(text) {
                if let (Some(var), Some(suffix), Some(class)) = (cap.get(1), cap.get(2), cap.get(3))
                {
                    let Some(key) = config_vars.get(var.as_str()) else {
                        continue;
                    };
                    let prefix = crate::config::resolve_config_string_for_package(&root, key, path)
                        .unwrap_or_default();
                    let tag = format!("{prefix}{}", suffix.as_str());
                    push_blade_component(&tag, suffix.start(), class);
                }
            }
        }
    }

    // Parse Blade::componentNamespace() registrations
    // Example: Blade::componentNamespace('Nightshade\\Views\\Components', 'nightshade')
    for cap in COMPONENT_NAMESPACE_RE.captures_iter(text) {
        if let (Some(php_ns), Some(prefix)) = (cap.get(1), cap.get(2)) {
            let php_namespace_str = php_ns.as_str();
            let prefix_str = prefix.as_str();

            let line = text[..prefix.start()].lines().count() as u32;

            let pkg_namespace = PackageNamespace::new(db, prefix_str.to_string());
            component_namespaces.push(ParsedComponentNamespaceReg::new(
                db,
                pkg_namespace,
                php_namespace_str.to_string(),
                line,
                priority,
                path.clone(),
            ));
        }
    }

    // Parse Blade::anonymousComponentPath() registrations.
    // Example: Blade::anonymousComponentPath(resource_path('views/backstage/components'), 'backstage')
    let provider_dir = path.parent().unwrap_or(path.as_path());
    let mut anonymous_component_paths = Vec::new();
    for (prefix, directory, line) in extract_anonymous_component_paths(text, &root, provider_dir) {
        let pkg_namespace = PackageNamespace::new(db, prefix);
        anonymous_component_paths.push(ParsedAnonymousComponentPathReg::new(
            db,
            pkg_namespace,
            directory,
            line,
            priority,
            path.clone(),
        ));
    }

    // Parse Blade::anonymousComponentNamespace() registrations.
    // Example: Blade::anonymousComponentNamespace('components.flux', 'flux')
    let mut anonymous_component_namespaces = Vec::new();
    for (prefix, directory, line) in extract_anonymous_component_namespaces(text) {
        let pkg_namespace = PackageNamespace::new(db, prefix);
        anonymous_component_namespaces.push(ParsedAnonymousComponentNamespaceReg::new(
            db,
            pkg_namespace,
            directory,
            line,
            priority,
            path.clone(),
        ));
    }

    ParsedServiceProvider::new(
        db,
        middleware,
        bindings,
        view_namespaces,
        blade_components,
        component_namespaces,
        anonymous_component_paths,
        anonymous_component_namespaces,
    )
}

/// Resolve a PHP path expression to an absolute filesystem path without
/// executing PHP. Handles the path forms that appear in real service-provider
/// `anonymousComponentPath()` calls:
///
/// - Laravel path helpers: `resource_path('x')`, `base_path('x')`, `app_path('x')`,
///   `storage_path('x')`, `public_path('x')`, `config_path('x')`,
///   `database_path('x')`, `lang_path('x')` — and their no-argument forms.
/// - `__DIR__ . '/relative'` — resolved against the provider file's directory.
/// - A plain string literal — absolute as-is, otherwise joined to the project root.
///
/// Returns `None` for expressions we can't statically resolve (e.g. a variable).
fn resolve_php_path_expr(expr: &str, root: &Path, provider_dir: &Path) -> Option<PathBuf> {
    use lazy_static::lazy_static;
    use regex::Regex;

    lazy_static! {
        /// `helper('sub/dir')` or `helper()` for the Laravel path helpers.
        static ref HELPER_RE: Regex = Regex::new(
            r#"^(resource_path|base_path|app_path|storage_path|public_path|config_path|database_path|lang_path)\s*\(\s*(?:['"]([^'"]*)['"]\s*)?\)$"#
        ).unwrap();
        /// `__DIR__ . '/relative'`
        static ref DIR_CONST_RE: Regex = Regex::new(
            r#"^__DIR__\s*\.\s*['"]([^'"]+)['"]$"#
        ).unwrap();
        /// A bare string literal.
        static ref LITERAL_RE: Regex = Regex::new(r#"^['"]([^'"]+)['"]$"#).unwrap();
    }

    let expr = expr.trim();

    if let Some(cap) = HELPER_RE.captures(expr) {
        let helper = cap.get(1).unwrap().as_str();
        let sub = cap.get(2).map(|m| m.as_str()).unwrap_or("");
        let base = match helper {
            "base_path" => root.to_path_buf(),
            "resource_path" => root.join("resources"),
            "app_path" => root.join("app"),
            "storage_path" => root.join("storage"),
            "public_path" => root.join("public"),
            "config_path" => root.join("config"),
            "database_path" => root.join("database"),
            "lang_path" => root.join("lang"),
            _ => return None,
        };
        let joined = if sub.is_empty() {
            base
        } else {
            base.join(sub.trim_start_matches('/'))
        };
        return Some(normalize_path(&joined));
    }

    if let Some(cap) = DIR_CONST_RE.captures(expr) {
        let sub = cap.get(1).unwrap().as_str().trim_start_matches('/');
        return Some(normalize_path(&provider_dir.join(sub)));
    }

    if let Some(cap) = LITERAL_RE.captures(expr) {
        let lit = cap.get(1).unwrap().as_str();
        let p = Path::new(lit);
        let joined = if p.is_absolute() {
            p.to_path_buf()
        } else {
            root.join(lit)
        };
        return Some(normalize_path(&joined));
    }

    None
}

/// Extract `Blade::anonymousComponentPath(<path>, 'prefix')` registrations from
/// service-provider source. Pure (regex + path resolution) so it can be unit
/// tested without a Salsa database. Returns `(prefix, absolute_directory,
/// source_line)` tuples; registrations whose path argument can't be statically
/// resolved are skipped.
fn extract_anonymous_component_paths(
    text: &str,
    root: &Path,
    provider_dir: &Path,
) -> Vec<(String, PathBuf, u32)> {
    use lazy_static::lazy_static;
    use regex::Regex;

    lazy_static! {
        /// Group 1 is the path expression (non-greedy, single line); group 2 is
        /// the string prefix. The two-argument (prefixed) form is the only one
        /// that produces `<x-prefix::component>` namespaced usage.
        static ref ANON_PATH_RE: Regex = Regex::new(
            r#"Blade::anonymousComponentPath\s*\(\s*(.+?)\s*,\s*['"]([^'"]+)['"]\s*\)"#
        ).unwrap();
    }

    let mut out = Vec::new();
    for cap in ANON_PATH_RE.captures_iter(text) {
        if let (Some(path_expr), Some(prefix)) = (cap.get(1), cap.get(2)) {
            if let Some(directory) = resolve_php_path_expr(path_expr.as_str(), root, provider_dir) {
                let line = text[..prefix.start()].lines().count() as u32;
                out.push((prefix.as_str().to_string(), directory, line));
            }
        }
    }
    out
}

/// Extract runtime view-namespace registrations made through the `View` factory:
///   View::addNamespace('ns', app_path('Ai/Prompts'))
///   View::prependNamespace('ns', resource_path('views/ns'))
///   app('view')->addNamespace('ns', base_path('packages/ns/views'))
///   $factory->addNamespace('ns', __DIR__ . '/../views')
///
/// Laravel's literal `$this->loadViewsFrom(__DIR__.'…', 'ns')` is matched
/// elsewhere (`LOAD_VIEWS_RE`); this covers the imperative facade/factory form,
/// where the directory argument is commonly a Laravel path helper rather than a
/// `__DIR__` concatenation. The path expression is delegated to
/// `resolve_php_path_expr`, so `app_path()`, `base_path()`, `resource_path()`,
/// the other path helpers, `__DIR__ . '…'`, and bare string literals all
/// resolve. Registrations whose path argument can't be statically resolved
/// (e.g. a variable) are skipped. Returns `(namespace, absolute_directory,
/// source_line)` tuples.
fn extract_add_namespace_view_registrations(
    text: &str,
    root: &Path,
    provider_dir: &Path,
) -> Vec<(String, PathBuf, u32)> {
    use lazy_static::lazy_static;
    use regex::Regex;

    lazy_static! {
        /// Receiver is the `View` facade, an `app('view')` resolve, or any
        /// `$factory->` instance; method is `addNamespace` or `prependNamespace`
        /// (both register a hint path — `prepend` only changes precedence).
        /// Group 1 is the namespace string; group 2 is the path expression,
        /// allowing one level of nested parentheses so helper calls like
        /// `app_path('Ai/Prompts')` are captured whole rather than truncated at
        /// the inner `)`.
        static ref ADD_NAMESPACE_RE: Regex = Regex::new(
            r#"(?:View::|app\(\s*['"]view['"]\s*\)->|\$\w+->)(?:add|prepend)Namespace\s*\(\s*['"]([^'"]+)['"]\s*,\s*((?:[^()]|\([^()]*\))+?)\s*\)"#
        ).unwrap();
    }

    let mut out = Vec::new();
    for cap in ADD_NAMESPACE_RE.captures_iter(text) {
        if let (Some(namespace), Some(path_expr)) = (cap.get(1), cap.get(2)) {
            if let Some(directory) = resolve_php_path_expr(path_expr.as_str(), root, provider_dir) {
                let line = text[..namespace.start()].lines().count() as u32;
                out.push((namespace.as_str().to_string(), directory, line));
            }
        }
    }
    out
}

/// Extract `Blade::anonymousComponentNamespace('dir', 'prefix')` registrations.
/// The directory is relative to the registered view paths; dots are normalized
/// to slashes (Laravel resolves it through the dot-notation view finder).
/// Returns `(prefix, view_relative_directory, source_line)` tuples.
fn extract_anonymous_component_namespaces(text: &str) -> Vec<(String, String, u32)> {
    use lazy_static::lazy_static;
    use regex::Regex;

    lazy_static! {
        static ref ANON_NS_RE: Regex = Regex::new(
            r#"Blade::anonymousComponentNamespace\s*\(\s*['"]([^'"]+)['"]\s*,\s*['"]([^'"]+)['"]\s*\)"#
        ).unwrap();
    }

    let mut out = Vec::new();
    for cap in ANON_NS_RE.captures_iter(text) {
        if let (Some(directory), Some(prefix)) = (cap.get(1), cap.get(2)) {
            let line = text[..prefix.start()].lines().count() as u32;
            let normalized = directory.as_str().replace('.', "/");
            out.push((prefix.as_str().to_string(), normalized, line));
        }
    }
    out
}

/// Normalize a path by resolving . and .. components without requiring the path to exist
fn normalize_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                components.pop();
            }
            std::path::Component::CurDir => {}
            c => components.push(c),
        }
    }
    components.iter().collect()
}

/// The namespace a fluent package-builder derives from a package name when
/// `->hasViews()` is called without an explicit argument: everything after a
/// leading `laravel-`. Mirrors the builder's own `shortName()`
/// (`Str::after($name, 'laravel-')`) so discovery matches runtime resolution.
///
/// `filament` → `filament`; `laravel-foo` → `foo`; `my-laravel-bar` → `bar`.
pub(crate) fn builder_short_name(package_name: &str) -> String {
    package_name
        .split_once("laravel-")
        .map(|(_, after)| after.to_string())
        .unwrap_or_else(|| package_name.to_string())
}

/// Expand a bare class name from a registration argument to its FQN using the
/// source file's `use` statements, the same way PHP resolves the reference.
/// `DynamicComponent` + `use Illuminate\View\DynamicComponent;` →
/// `Illuminate\View\DynamicComponent`. Aliased imports (`use Foo\Bar as Baz;`)
/// match on the alias. Names already carrying a `\` are returned unchanged;
/// so is a name with no matching import (group-use bodies are not expanded —
/// resolution then simply fails downstream, same as before).
fn expand_class_via_use_statements(class_name: &str, source: &str) -> String {
    if class_name.contains('\\') {
        return class_name.to_string();
    }

    for line in source.lines() {
        let trimmed = line.trim();
        let Some(import) = trimmed.strip_prefix("use ") else {
            continue;
        };
        // `use function`/`use const` imports and trait-`use` inside class
        // bodies (no namespace separator, e.g. `use HasFactory;`) are not
        // class imports we can expand from.
        if import.starts_with("function ") || import.starts_with("const ") {
            continue;
        }
        let Some(import) = import.strip_suffix(';') else {
            continue;
        };

        let (fqn, visible_name) = match import.split_once(" as ") {
            Some((fqn, alias)) => (fqn.trim(), alias.trim()),
            None => {
                let fqn = import.trim();
                let basename = fqn.rsplit('\\').next().unwrap_or(fqn);
                (fqn, basename)
            }
        };

        if visible_name == class_name && fqn.contains('\\') {
            return fqn.trim_start_matches('\\').to_string();
        }
    }

    class_name.to_string()
}

/// Resolve a class name to a file path using PSR-4 conventions
fn resolve_class_to_file_internal(class_name: &str, root_path: &Path) -> Option<PathBuf> {
    // PSR-4 via the composer autoload map first — the authoritative answer
    // for any installed package (vendor or app). The legacy prefix mappings
    // below stay as fallbacks for projects without a readable autoload map.
    if let Some((namespace, class)) = class_name.rsplit_once('\\') {
        let autoload = crate::composer_autoload::ComposerAutoload::for_project(root_path);
        for dir in autoload.resolve_namespace_dirs(namespace) {
            let file = dir.join(class).with_extension("php");
            if file.exists() {
                return Some(file);
            }
        }
    }

    // Common namespace to directory mappings
    let mappings = [
        ("App\\", "app/"),
        ("Illuminate\\", "vendor/laravel/framework/src/Illuminate/"),
        ("Laravel\\", "vendor/laravel/"),
    ];

    for (namespace, dir) in &mappings {
        if class_name.starts_with(namespace) {
            let relative = class_name.strip_prefix(namespace)?;
            let file_path = root_path
                .join(dir)
                .join(relative.replace('\\', "/"))
                .with_extension("php");
            if file_path.exists() {
                return Some(file_path);
            }
        }
    }

    // Try direct class name as path
    let direct_path = root_path
        .join(class_name.replace('\\', "/"))
        .with_extension("php");
    if direct_path.exists() {
        return Some(direct_path);
    }

    None
}

// ============================================================================
// Helper Functions
// ============================================================================

impl LaravelDatabase {
    /// Create a new database instance
    pub fn new() -> Self {
        Self::default()
    }
}

// ============================================================================
// Data Transfer Types - Plain structs for sending data across threads
// ============================================================================

/// View reference data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ViewReferenceData {
    pub name: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
    pub is_route_view: bool,
}

/// Component reference data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ComponentReferenceData {
    pub name: String,
    pub tag_name: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Directive reference data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DirectiveReferenceData {
    pub name: String,
    pub arguments: Option<String>,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
    /// Column of first character INSIDE the quoted string (after opening quote)
    pub string_column: u32,
    /// Column one past the last character INSIDE the quoted string (before closing quote)
    pub string_end_column: u32,
}

/// Env reference data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EnvReferenceData {
    pub name: String,
    pub has_fallback: bool,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Config reference data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ConfigReferenceData {
    pub key: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Livewire reference data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LivewireReferenceData {
    pub name: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Middleware reference data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MiddlewareReferenceData {
    pub name: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Translation reference data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TranslationReferenceData {
    pub key: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Asset reference data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AssetReferenceData {
    pub path: String,
    pub helper_type: AssetHelperType,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Binding reference data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BindingReferenceData {
    pub name: String,
    pub is_class_reference: bool,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Route reference data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RouteReferenceData {
    pub name: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// URL reference data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UrlReferenceData {
    pub path: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Action reference data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ActionReferenceData {
    pub action: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Feature reference data for transfer across async boundaries (Laravel Pennant)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FeatureReferenceData {
    /// The feature name (string key like 'new-api' or class name like 'NewApi')
    pub feature_name: String,
    /// The method being called (active, inactive, value, when, etc.)
    pub method_name: String,
    /// Whether this is a class-based feature (Feature::active(NewApi::class))
    pub is_class_reference: bool,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Confidence that a captured member-access site's receiver was resolved to a
/// concrete declaring class.
///
/// Populated by M3's receiver resolution; at capture time (M2) every site is
/// [`Confidence::Unresolved`]. The tiers mirror the plan's resolution tiers:
/// HIGH (static call, `(new X)`, typed param, `@var`, simple local assignment),
/// MEDIUM (multi-hop reassignment / indirect flow), LOW (foreach iter var,
/// typed property, return chain — captured but not yet resolvable; widened in
/// later work, never guessed).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub enum Confidence {
    High,
    Medium,
    Low,
    /// Not yet run through the resolver — the state at capture time (M2).
    #[default]
    Unresolved,
}

/// The kind of member a resolved access maps to.
///
/// `None` on the reference until M3 classifies the site against the
/// class-hierarchy index. The Eloquent-magic variants are what make
/// find-references / rename / hover magic-aware; `PlainMember` is a generic
/// (non-magic) property on a resolved class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum MagicMemberKind {
    /// Eloquent local scope accessed via `__call` (`scopeActive` → `->active()`).
    Scope,
    /// Eloquent accessor / attribute (`getFullNameAttribute` / `Attribute`).
    Accessor,
    /// Eloquent relationship method accessed as a property (`$user->posts`).
    Relationship,
    /// Database column surfaced as a model attribute (`$user->email`).
    Column,
    /// Dynamic finder (`User::whereEmail(...)` → `where('email', ...)`).
    DynamicFinder,
    /// Generic (non-magic) property on a resolved class.
    PlainMember,
}

/// How a member was syntactically accessed. Drives which magic kinds are even
/// possible: a scope is only reachable via a call, an accessor only via a
/// property read. Lives here (not `member_resolver`, which re-exports it)
/// because it travels inside [`MemberAccessReferenceData`] through the
/// per-file pattern cache. NOTE: that cache is bincode (non-self-describing),
/// so `serde(default)` does NOT make old caches decodable — the
/// `pattern_disk_cache` SCHEMA_VERSION bump is what protects against stale
/// shapes. Any future field change here needs another bump.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub enum AccessForm {
    /// `$user->email` — property read (no call parens).
    #[default]
    Property,
    /// `User::active()` — static call (`::`).
    StaticCall,
    /// `$user->active()` / `$user->posts()` — instance method call (`->m()`).
    InstanceCall,
}

impl AccessForm {
    /// Call-form (`::m()` or `->m()`) vs property read.
    pub fn is_call(self) -> bool {
        matches!(self, AccessForm::StaticCall | AccessForm::InstanceCall)
    }
}

/// A member access (`$user->email`, `$user->active()`, `User::whereEmail()`)
/// captured for the magic-member semantic index.
///
/// **Capture-only at M2.** The `member`, `receiver`, byte ranges, nullsafe
/// flag, and position fields are populated now. The resolution fields
/// (`declaring_fqcn`, `kind`, `confidence`) are a reserved scaffold M3 fills
/// once receiver resolution + `ClassView` classification land — until then
/// `declaring_fqcn`/`kind` are `None` and `confidence` is
/// [`Confidence::Unresolved`]. Wiring the index here once keeps M3 a pure
/// "fill in resolution" diff with no structural churn.
/// A Blade `@foreach`/`@forelse` loop's item variable + iterable, captured for
/// magic-member loop-variable typing. `{{ $user->email }}` inside
/// `@foreach($users as $user)` types `$user` from `$users`' element type.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BladeLoopVar {
    /// Loop value variable, without `$` (`user` from `… as $user`).
    pub item_var: String,
    /// Iterable expression, as written (`$users`, `$this->users`, `User::all()`).
    pub iterable: String,
    /// 0-based line of the `@foreach`/`@forelse` directive.
    pub start_line: u32,
    /// 0-based line of the matching `@endforeach`/`@endforelse`; `u32::MAX` if
    /// the loop is unclosed (treat as extending to end of file).
    pub end_line: u32,
}

/// Extract the `@foreach`/`@forelse` loops worth capturing for loop-variable
/// typing: those with an iterable and a value variable. The value variable is
/// the last of the parsed loop variables (`$key => $value` keeps `value`).
pub fn blade_loop_vars(content: &str) -> Vec<BladeLoopVar> {
    use crate::blade_loops::{find_loop_blocks, BladeLoopType};
    find_loop_blocks(content)
        .into_iter()
        .filter(|b| matches!(b.loop_type, BladeLoopType::Foreach | BladeLoopType::Forelse))
        .filter_map(|b| {
            let iterable = b.iterable?;
            let item_var = b.variables.last()?.0.clone();
            Some(BladeLoopVar {
                item_var,
                iterable,
                start_line: b.start_line as u32,
                end_line: b.end_line.map(|e| e as u32).unwrap_or(u32::MAX),
            })
        })
        .collect()
}

/// Member accesses written inside `@foreach`/`@forelse` *iterable* expressions
/// (`@foreach($this->entities as $e)` → a read of `$this->entities`). These
/// live in directive arguments, not `{{ }}` echoes or PHP blocks, so the normal
/// capture misses them — yet they're real references a find-references should
/// surface. We synthesize a `MemberAccessReferenceData` for the last `->member`
/// of each member-access iterable, positioned at the member name in the
/// directive line.
pub fn blade_loop_iterable_accesses(content: &str) -> Vec<MemberAccessReferenceData> {
    let mut out = Vec::new();
    for loop_var in blade_loop_vars(content) {
        let iter = loop_var.iterable.trim();
        // Only member-access iterables (`$x->y`, `$this->y`); a bare `$users`
        // collection has no member to reference.
        let Some(arrow) = iter.rfind("->") else {
            continue;
        };
        let member = &iter[arrow + 2..];
        let receiver = &iter[..arrow];
        if member.is_empty() || !member.chars().all(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        // Locate the iterable on its directive line to position the member.
        let Some(line_text) = content.lines().nth(loop_var.start_line as usize) else {
            continue;
        };
        let Some(iter_col) = line_text.find(iter) else {
            continue;
        };
        let member_col = (iter_col + arrow + 2) as u32;
        out.push(MemberAccessReferenceData {
            member: member.to_string(),
            receiver: receiver.to_string(),
            receiver_byte_start: 0,
            receiver_byte_end: 0,
            is_nullsafe: false,
            form: AccessForm::Property,
            line: loop_var.start_line,
            column: member_col,
            end_column: member_col + member.len() as u32,
            declaring_fqcn: None,
            kind: None,
            confidence: Confidence::Unresolved,
        });
    }
    out
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MemberAccessReferenceData {
    /// The accessed member name (`email`, `posts`, `profile`).
    pub member: String,
    /// Raw source text of the receiver expression (`$user`, `$this`).
    pub receiver: String,
    /// Byte range of the receiver expression in the file — lets the M3
    /// resolver locate the receiver node in the live tree for
    /// `var_type::resolve`.
    pub receiver_byte_start: usize,
    pub receiver_byte_end: usize,
    /// Whether the access used the nullsafe operator (`?->`).
    pub is_nullsafe: bool,
    /// How the member was accessed. Call-form sites can only classify as
    /// scopes / dynamic finders / relationships; property-form as accessors /
    /// relationships / columns. (`serde(default)` helps only self-describing
    /// formats — the bincode pattern cache is guarded by its SCHEMA_VERSION
    /// bump, not by this default.)
    #[serde(default)]
    pub form: AccessForm,
    /// Position of the member name (0-based — repo convention).
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
    // ─── Reserved resolution scaffold (filled by M3) ───
    /// Declaring class FQCN once the receiver resolves (inheritance/trait
    /// resolved). `None` until M3.
    #[serde(default)]
    pub declaring_fqcn: Option<String>,
    /// What kind of member this resolves to. `None` until M3 classifies.
    #[serde(default)]
    pub kind: Option<MagicMemberKind>,
    /// Resolution confidence. [`Confidence::Unresolved`] until M3.
    #[serde(default)]
    pub confidence: Confidence,
}

/// Hover payload for a resolved magic member (M6). Crosses the Salsa async
/// boundary, so it owns plain data (no lifetimes / borrows). `decl_file` /
/// `decl_line` locate the declaration for a source link — `None` when the
/// declaring class isn't in the hierarchy index.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MagicMemberHoverData {
    pub declaring_fqcn: String,
    pub member: String,
    pub kind: MagicMemberKind,
    pub confidence: Confidence,
    pub decl_file: Option<PathBuf>,
    /// 0-based start line of the declaration (method or property), for the link.
    pub decl_line: Option<u32>,
    /// 0-based end line — present only for a *method* declaration, so the async
    /// hover builder knows to read the declaring file and extract a snippet.
    pub decl_end_line: Option<u32>,
    /// True when the resolver couldn't classify the member but the receiver
    /// resolved to a model — a likely *plain DB column* (not `$casts`-declared,
    /// so invisible to the source-only `ClassView`). The main side must confirm
    /// it against migrations/DB before rendering, and skip the card otherwise.
    pub tentative: bool,
}

/// Resolution result for renaming a magic member (M7). Crosses the async
/// boundary (owns plain data). Only method-backed kinds — relationship, scope,
/// accessor, dynamic finder — produce this; columns/plain members return `None`
/// (a DB column rename is a migration concern, out of scope).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MagicMemberRenameData {
    pub fqcn: String,
    /// Usage name (the find-references key + the call-site rewrite text).
    pub member: String,
    pub kind: MagicMemberKind,
    /// The actual declared method name (`scopeActive`, `getFullNameAttribute`,
    /// `posts`) — the decl site to rewrite, transformed by the caller.
    pub method_name: String,
    pub decl_file: PathBuf,
}

/// Laravel configuration data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LaravelConfigData {
    pub root: PathBuf,
    pub view_paths: Vec<PathBuf>,
    pub component_paths: Vec<(String, PathBuf)>,
    pub livewire_path: Option<PathBuf>,
    pub has_livewire: bool,
    /// Package view namespaces from loadViewsFrom() calls
    /// Maps namespace (e.g., "courier") to view path
    pub view_namespaces: HashMap<String, PathBuf>,
    /// Package component namespaces from Blade::componentNamespace() calls
    /// Maps prefix (e.g., "nightshade") to PHP namespace
    pub component_namespaces: HashMap<String, String>,
    /// Anonymous component paths from Blade::anonymousComponentPath($path, 'prefix').
    /// Maps prefix (e.g., "backstage") to the **absolute** directory holding the
    /// anonymous components. Resolution is `{dir}/{component}.blade.php` — no
    /// `components/` segment is appended, because Laravel registers the directory
    /// itself (unlike the package-publish `resources/views/vendor/<ns>/` convention).
    pub anonymous_component_paths: HashMap<String, PathBuf>,
    /// Anonymous component namespaces from Blade::anonymousComponentNamespace($dir, 'prefix').
    /// Maps prefix (e.g., "flux") to a directory **relative to the view paths**
    /// (dots normalized to slashes). Resolution is
    /// `{view_path}/{dir}/{component}.blade.php`.
    pub anonymous_component_namespaces: HashMap<String, String>,
    /// Component aliases registered via Blade::component($view, $alias) or via
    /// config-based registration loops. Maps alias (e.g., "light-button") to
    /// the target view path in dot notation (e.g., "components.buttons.light-button").
    /// Consulted before falling back to the directory-convention lookup.
    pub component_aliases: HashMap<String, String>,
    /// Icon-set component aliases registered via blade-icons' Factory pattern.
    /// Maps the full tag name (e.g., "heroicon-o-clock") to the absolute SVG
    /// file path. Built by walking vendor packages with `resources/svg/` +
    /// `config/blade-*.php` shape and combining the prefix with each SVG file.
    pub icon_aliases: HashMap<String, String>,
    /// Class-backed component registrations from
    /// `Blade::component('tag', Class::class)` (facade or instance form,
    /// either argument order). Maps the `<x-{tag}>` tag to the registered
    /// class's resolved file. Laravel core registers `dynamic-component` →
    /// `Illuminate\View\DynamicComponent` this way. `serde(default)` keeps
    /// disk-cached configs written before this field deserializable.
    #[serde(default)]
    pub class_component_files: HashMap<String, PathBuf>,
}

impl LaravelConfigData {
    /// Resolve a view name to possible file paths
    ///
    /// Returns all possible paths where this view could exist,
    /// in order of priority.
    pub fn resolve_view_path(&self, view_name: &str) -> Vec<PathBuf> {
        let mut paths = Vec::new();

        // Handle package views (e.g., "package::view.name")
        let (namespace, actual_view) = if let Some(pos) = view_name.find("::") {
            let namespace = &view_name[..pos];
            let view = &view_name[pos + 2..];
            (Some(namespace), view)
        } else {
            (None, view_name)
        };

        // Convert dots to path separators
        let view_path = actual_view.replace('.', "/");

        // If there's a namespace, resolve using package view paths
        if let Some(ns) = namespace {
            if let Some(package_view_path) = self.view_namespaces.get(ns) {
                // Package views - use the registered path
                let mut full_path = package_view_path.join(&view_path);
                full_path.set_extension("blade.php");
                paths.push(full_path);
            }
            // Also check vendor published views: resources/views/vendor/{namespace}/
            let mut vendor_path = self
                .root
                .join("resources/views/vendor")
                .join(ns)
                .join(&view_path);
            vendor_path.set_extension("blade.php");
            paths.push(vendor_path);
        } else {
            // Regular views - check each configured view path
            for base_path in &self.view_paths {
                let mut full_path = self.root.join(base_path).join(&view_path);
                full_path.set_extension("blade.php");
                paths.push(full_path);
            }
        }

        paths
    }

    /// Resolve a component name to file path
    pub fn resolve_component_path(&self, component_name: &str) -> Vec<PathBuf> {
        let mut paths = Vec::new();

        // Icon-set check first: <x-heroicon-o-clock> and friends resolve to a
        // concrete SVG file path. The blade-icons Factory registers each icon
        // at runtime via a loop over filesystem manifests, so static AST analysis
        // can't extract the pairs — we precompute the map by walking the SVG
        // directories of any blade-icons-shaped vendor package.
        if !component_name.contains("::") {
            if let Some(svg_path) = self.icon_aliases.get(component_name) {
                paths.push(PathBuf::from(svg_path));
                return paths;
            }
        }

        // Check explicit Blade component aliases first. Alias registrations like
        // Blade::component('components.buttons.light-button', 'light-button') or
        // their config-driven equivalents override the directory convention, so
        // the alias map wins when there's a hit.
        if !component_name.contains("::") {
            if let Some(aliased) = self.component_aliases.get(component_name) {
                let aliased_path = aliased.replace('.', "/");
                for view_path in &self.view_paths {
                    let mut full_path = self.root.join(view_path).join(&aliased_path);
                    full_path.set_extension("blade.php");
                    paths.push(full_path);
                }
                if paths.is_empty() {
                    let mut full_path = self.root.join("resources/views").join(&aliased_path);
                    full_path.set_extension("blade.php");
                    paths.push(full_path);
                }
                return paths;
            }
        }

        // Handle package components (e.g., "courier::alert")
        let (namespace, actual_component) = if let Some(pos) = component_name.find("::") {
            let namespace = &component_name[..pos];
            let component = &component_name[pos + 2..];
            (Some(namespace), component)
        } else {
            (None, component_name)
        };

        // Component name uses dots: "forms.input" -> "forms/input.blade.php"
        let component_path = actual_component.replace('.', "/");

        if let Some(ns) = namespace {
            // Markdown mail components are hardcoded in Laravel's
            // ComponentTagCompiler: `<x-mail::message>` maps straight to view
            // `mail::message`, and at render time `Markdown` points the `mail`
            // namespace at `{path}/html` for each configured path — the
            // published `resources/views/vendor/mail` first, then the
            // framework's bundled views. There is no `components/` segment.
            // Pushed first so the published path is `paths.first()` — the
            // diagnostic reports that as the "Expected at:" location.
            if ns == "mail" {
                let mut published = self
                    .root
                    .join("resources/views/vendor/mail/html")
                    .join(&component_path);
                published.set_extension("blade.php");
                paths.push(published);
                let mut framework = self
                    .root
                    .join("vendor/laravel/framework/src/Illuminate/Mail/resources/views/html")
                    .join(&component_path);
                framework.set_extension("blade.php");
                paths.push(framework);
            }

            // Anonymous component path (Blade::anonymousComponentPath): the
            // registered directory IS the components directory, so resolve
            // directly with no `components/` segment.
            if let Some(dir) = self.anonymous_component_paths.get(ns) {
                push_component_file_candidates(&mut paths, dir.join(&component_path));
            }

            // Anonymous component namespace (Blade::anonymousComponentNamespace):
            // the registered directory is relative to each view path.
            if let Some(dir) = self.anonymous_component_namespaces.get(ns) {
                for view_path in &self.view_paths {
                    let base = self.root.join(view_path).join(dir);
                    push_component_file_candidates(&mut paths, base.join(&component_path));
                }
            }

            // Package component - check package view path first
            if let Some(package_view_path) = self.view_namespaces.get(ns) {
                // Anonymous package component: {package_views}/components/{component}.blade.php
                push_component_file_candidates(
                    &mut paths,
                    package_view_path.join("components").join(&component_path),
                );
            }

            // Also check component namespace (Blade::componentNamespace)
            if let Some(php_namespace) = self.component_namespaces.get(ns) {
                // Convert component name to PascalCase class path
                // "alert" -> "Alert.php", "alert-box" -> "AlertBox.php"
                let class_name = kebab_to_pascal_case(&component_path.replace('/', "\\"));
                let class_path = format!("{}/{}.php", php_namespace.replace('\\', "/"), class_name);
                // Try common locations for package classes
                paths.push(self.root.join("vendor").join(&class_path));
                paths.push(
                    self.root
                        .join("app/View/Components")
                        .join(&class_name)
                        .with_extension("php"),
                );
            }

            // Check vendor published components: resources/views/vendor/{namespace}/components/
            push_component_file_candidates(
                &mut paths,
                self.root
                    .join("resources/views/vendor")
                    .join(ns)
                    .join("components")
                    .join(&component_path),
            );
        } else {
            // Regular component - check each component path
            for (_namespace, base_path) in &self.component_paths {
                let mut full_path = self.root.join(base_path).join(&component_path);
                full_path.set_extension("blade.php");
                paths.push(full_path);
            }

            // If no component paths found, use default within view paths
            if paths.is_empty() {
                for view_path in &self.view_paths {
                    let mut full_path = self
                        .root
                        .join(view_path)
                        .join("components")
                        .join(&component_path);
                    full_path.set_extension("blade.php");
                    paths.push(full_path);
                }
            }
        }

        paths
    }

    /// Resolve a Livewire component name to file path
    pub fn resolve_livewire_path(&self, component_name: &str) -> Option<PathBuf> {
        let livewire_base = self.livewire_path.as_ref()?;

        // Convert component name to PascalCase path
        // "user-profile" -> "UserProfile.php"
        // "admin.dashboard" -> "Admin/Dashboard.php"

        let parts: Vec<&str> = component_name.split('.').collect();
        let mut path = self.root.join(livewire_base);

        for (i, part) in parts.iter().enumerate() {
            let pascal_case = kebab_to_pascal_case(part);

            if i == parts.len() - 1 {
                // Last part becomes the PHP file
                path.push(format!("{}.php", pascal_case));
            } else {
                // Other parts are directories
                path.push(pascal_case);
            }
        }

        Some(path)
    }
}

/// Push the three file shapes Laravel accepts for an anonymous component at
/// `base` (the component's path under its directory, *without* extension),
/// mirroring `ComponentTagCompiler`'s guess order:
///   1. `{base}.blade.php`            — flat file
///   2. `{base}/index.blade.php`      — directory-index convention
///   3. `{base}/{last}.blade.php`     — directory-self convention
///      (`<x-ns::button>` → `button/button.blade.php`)
fn push_component_file_candidates(paths: &mut Vec<PathBuf>, base: PathBuf) {
    let mut direct = base.clone();
    direct.set_extension("blade.php");
    paths.push(direct);
    paths.push(base.join("index.blade.php"));
    if let Some(last) = base.file_name().and_then(|s| s.to_str()) {
        let self_named = format!("{last}.blade.php");
        paths.push(base.join(self_named));
    }
}

/// All candidate file paths that could back a Blade component tag, in
/// priority order. This is the **single source of truth** shared by
/// goto-definition and the "component not found" diagnostic, so the two can
/// never disagree about whether a component resolves (issue #69).
///
/// Layers, in order:
///   1. [`LaravelConfigData::resolve_component_path`] — conventional,
///      aliased, icon, anonymous-path/namespace, package-view, vendor-publish,
///      and the *naive* class-namespace guesses.
///   2. The conventional class-backed component file
///      (`app/View/Components/<Pascal>.php`).
///   3. **PSR-4 class-based `Blade::componentNamespace` components.** Layer 1
///      only emits a guessed `vendor/<Namespace>/...` path that ignores how
///      Composer actually lays packages out on disk, so namespaced class
///      components (`<x-filament::badge>`, `<x-mail::message>`) never matched.
///      Here we walk the registered PHP namespace to its real source
///      directory via the autoload map and append the class file path.
///   4. **Explicit class-backed registrations** —
///      `Blade::component('tag', Class::class)` in any provider, facade or
///      instance form (Laravel core registers `dynamic-component` this way).
///      The tag maps straight to the registered class's resolved file.
///
/// `autoload` supplies the project's PSR-4 prefix map (see
/// [`crate::composer_autoload::ComposerAutoload`]). The function does **not**
/// touch the filesystem itself — callers decide existence (cached async check
/// for the live server, direct `Path::exists` in tests).
pub fn component_candidate_paths(
    name: &str,
    config: &LaravelConfigData,
    autoload: &crate::composer_autoload::ComposerAutoload,
) -> Vec<PathBuf> {
    let mut candidates = config.resolve_component_path(name);

    // Conventional class-backed component (non-namespaced names).
    candidates
        .push(crate::component_declaration_locator::conventional_class_file_path(name, config));

    // Explicit class-backed registration: Blade::component('tag', Class::class)
    // in any provider (facade or instance form). Laravel core registers
    // `dynamic-component` → Illuminate\View\DynamicComponent this way.
    if let Some(class_file) = config.class_component_files.get(name) {
        candidates.push(class_file.clone());
    }

    // PSR-4 class-based componentNamespace resolution.
    if let Some((namespace, component)) = name.split_once("::") {
        if let Some(php_namespace) = config.component_namespaces.get(namespace) {
            // `forms.text-input` → relative class path `Forms/TextInput.php`,
            // matching the FQCN `<php_namespace>\Forms\TextInput`. Each
            // `\`-delimited segment must be PascalCased independently, since
            // `kebab_to_pascal_case` only splits on `-`.
            let class_name = component
                .replace('.', "\\")
                .split('\\')
                .map(kebab_to_pascal_case)
                .collect::<Vec<_>>()
                .join("\\");
            let mut rel = PathBuf::new();
            for segment in class_name.split('\\') {
                rel.push(segment);
            }
            rel.set_extension("php");

            for dir in autoload.resolve_namespace_dirs(php_namespace) {
                candidates.push(dir.join(&rel));
            }
        }
    }

    candidates
}

/// Type of file that contains a view reference
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum FileReferenceType {
    Controller,
    BladeTemplate,
    LivewireComponent,
    Route,
}

/// Classified symbol under the cursor — the payload `Backend::references`
/// (and later `Backend::rename`) hands across the Salsa actor boundary.
///
/// We never raw-shape-match: a position only counts as a reference to the
/// requested symbol when (a) the parser tagged the position as that pattern
/// kind AND (b) the carried name matches. Random PHP strings that happen to
/// share the shape are not returned.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum SymbolRefData {
    View(String),
    Route(String),
    Config(String),
    Translation(String),
    Env(String),
    Component(String),
    Livewire(String),
    Middleware(String),
    Binding(String),
    /// An Eloquent magic member (accessor / column / relationship / scope /
    /// dynamic finder) or a plain class member, keyed by its inheritance-
    /// resolved declaring class FQCN + member name. Unlike the literal kinds
    /// above — whose name is a raw string the parser tagged — this key is
    /// produced by the M3 resolver, so a trait-shared member keys once and
    /// every inheriting model's usages collapse to the same entry.
    MagicMember {
        fqcn: String,
        member: String,
    },
}

/// Location of a single parser-classified reference. Generic across pattern
/// kinds — `Backend::references` converts these into LSP `Location`s.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReferenceLocationData {
    pub file_path: PathBuf,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// View reference location data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize)]
pub struct ViewReferenceLocationData {
    /// The file that contains the reference
    pub file_path: PathBuf,
    /// The line number where the reference occurs (0-based)
    pub line: u32,
    /// The character position where the reference starts (0-based)
    pub character: u32,
    /// The type of file containing the reference
    pub reference_type: FileReferenceType,
    /// The view name being referenced
    pub view_name: String,
    /// Whether this is a route view (Route::view or response()->view)
    pub is_route_view: bool,
}

/// Middleware registration data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize)]
pub struct MiddlewareRegistrationData {
    /// The middleware alias (e.g., "auth")
    pub alias: String,
    /// Fully qualified class name
    pub class_name: String,
    /// Resolved file path of the middleware class
    pub file_path: Option<PathBuf>,
    /// Source file where the alias is defined
    pub source_file: Option<PathBuf>,
    /// Line number in source file (0-based)
    pub source_line: Option<usize>,
    /// Priority: 0=framework, 1=package, 2=app
    pub priority: u8,
}

/// Binding type enum for transfer
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum BindingTypeData {
    Bind,
    Singleton,
    Scoped,
    Alias,
}

/// Container binding data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize)]
pub struct BindingRegistrationData {
    /// The abstract/alias name
    pub abstract_name: String,
    /// Fully qualified concrete class name
    pub concrete_class: String,
    /// Resolved file path of the concrete class
    pub file_path: Option<PathBuf>,
    /// Binding type
    pub binding_type: BindingTypeData,
    /// Source file where the binding is defined
    pub source_file: Option<PathBuf>,
    /// Line number in source file (0-based)
    pub source_line: Option<usize>,
    /// Priority: 0=framework, 1=package, 2=app
    pub priority: u8,
}

/// Environment variable data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize)]
pub struct EnvVariableData {
    /// The variable name (e.g., "APP_NAME")
    pub name: String,
    /// The value (e.g., "Laravel")
    pub value: String,
    /// Which file this was defined in
    pub file_path: PathBuf,
    /// Line number where defined (0-based)
    pub line: usize,
    /// Column where the variable name starts (0-based)
    pub column: usize,
    /// Column where the value starts (after the =)
    pub value_column: usize,
    /// Whether this variable is commented out
    pub is_commented: bool,
}

/// Package view namespace data for transfer across async boundaries
/// From: $this->loadViewsFrom(__DIR__.'/../resources/views', 'courier')
#[derive(Debug, Clone, serde::Serialize)]
pub struct ViewNamespaceData {
    /// The namespace prefix (e.g., "courier")
    pub namespace: String,
    /// Resolved view path
    pub view_path: Option<PathBuf>,
    /// Source file where registered
    pub source_file: PathBuf,
    /// Line number in source file
    pub source_line: u32,
    /// Priority: 0=framework, 1=package, 2=app
    pub priority: u8,
}

/// Blade component registration data for transfer across async boundaries
/// From: Blade::component('package-alert', AlertComponent::class)
#[derive(Debug, Clone, serde::Serialize)]
pub struct BladeComponentRegData {
    /// Component tag name (e.g., "package-alert")
    pub tag_name: String,
    /// Full class name
    pub class_name: String,
    /// Resolved file path of the component class
    pub file_path: Option<PathBuf>,
    /// Source file where registered
    pub source_file: PathBuf,
    /// Line number in source file
    pub source_line: u32,
    /// Priority: 0=framework, 1=package, 2=app
    pub priority: u8,
}

/// Component namespace registration data for transfer across async boundaries
/// From: Blade::componentNamespace('Nightshade\\Views\\Components', 'nightshade')
#[derive(Debug, Clone, serde::Serialize)]
pub struct ComponentNamespaceData {
    /// Namespace prefix (e.g., "nightshade")
    pub prefix: String,
    /// PHP namespace (e.g., "Nightshade\\Views\\Components")
    pub php_namespace: String,
    /// Source file where registered
    pub source_file: PathBuf,
    /// Line number in source file
    pub source_line: u32,
    /// Priority: 0=framework, 1=package, 2=app
    pub priority: u8,
}

// ============================================================================
// Salsa-based Data Transfer Types (for new incremental parsing)
// ============================================================================

/// Parsed environment variable data from Salsa (for transfer across async boundaries)
#[derive(Debug, Clone, serde::Serialize)]
pub struct ParsedEnvVarData {
    /// Variable name
    pub name: String,
    /// Variable value
    pub value: String,
    /// Line number (0-indexed)
    pub line: u32,
    /// Column of variable name
    pub column: u32,
    /// Column where value starts
    pub value_column: u32,
    /// Whether commented out
    pub is_commented: bool,
    /// Priority (0=.env.example, 1=.env.local, 2=.env)
    pub priority: u8,
    /// Source file path
    pub source_file: PathBuf,
}

/// Parsed middleware data from Salsa (for transfer across async boundaries)
#[derive(Debug, Clone, serde::Serialize)]
pub struct ParsedMiddlewareData {
    /// Middleware alias
    pub alias: String,
    /// Full class name
    pub class_name: String,
    /// Resolved file path
    pub file_path: Option<PathBuf>,
    /// Line in source file
    pub source_line: u32,
    /// Priority (0=framework, 1=package, 2=app)
    pub priority: u8,
    /// Source file path
    pub source_file: PathBuf,
}

/// Parsed binding data from Salsa (for transfer across async boundaries)
#[derive(Debug, Clone, serde::Serialize)]
pub struct ParsedBindingData {
    /// Abstract name or interface
    pub abstract_name: String,
    /// Concrete class name
    pub concrete_class: String,
    /// Resolved file path
    pub file_path: Option<PathBuf>,
    /// Binding type
    pub binding_type: BindingTypeEnum,
    /// Line in source file
    pub source_line: u32,
    /// Priority (0=framework, 1=package, 2=app)
    pub priority: u8,
    /// Source file path
    pub source_file: PathBuf,
}

/// Entry in the sorted position index for fast lookup
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PositionEntry {
    line: u32,
    column: u32,
    end_column: u32,
    pattern: PatternAtPosition,
}

/// All parsed patterns for a file - plain data for transfer
/// Uses Rc for efficient cloning when building the position index
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ParsedPatternsData {
    pub views: Vec<Arc<ViewReferenceData>>,
    pub components: Vec<Arc<ComponentReferenceData>>,
    pub directives: Vec<Arc<DirectiveReferenceData>>,
    pub env_refs: Vec<Arc<EnvReferenceData>>,
    pub config_refs: Vec<Arc<ConfigReferenceData>>,
    pub livewire_refs: Vec<Arc<LivewireReferenceData>>,
    pub middleware_refs: Vec<Arc<MiddlewareReferenceData>>,
    pub translation_refs: Vec<Arc<TranslationReferenceData>>,
    pub asset_refs: Vec<Arc<AssetReferenceData>>,
    pub binding_refs: Vec<Arc<BindingReferenceData>>,
    pub route_refs: Vec<Arc<RouteReferenceData>>,
    pub url_refs: Vec<Arc<UrlReferenceData>>,
    pub action_refs: Vec<Arc<ActionReferenceData>>,
    pub feature_refs: Vec<Arc<FeatureReferenceData>>,
    /// Eloquent / DB query builder chains extracted in the same PHP parse pass
    /// as route/url/action/feature refs (see [`crate::query_chain::extractor`]).
    /// Stored alongside the other patterns rather than as a `ParsedPatterns`
    /// field because that struct is at Salsa's 12-element tuple-Hash cap.
    ///
    /// `#[serde(default)]` so disk-cache entries written by older builds (which
    /// lacked this field) deserialize with an empty chains list rather than
    /// failing the whole entry. The next file edit re-runs extraction and
    /// populates chains properly.
    #[serde(default)]
    pub chains: Vec<Arc<crate::query_chain::BuilderChain>>,
    /// Property-form member accesses (`$user->email`, `$this->profile`)
    /// captured for the magic-member semantic index (M2). Like `chains`,
    /// stored here rather than as a `ParsedPatterns` Salsa field because that
    /// struct is at its 12-element tuple-Hash cap.
    ///
    /// `#[serde(default)]` so older disk-cache entries (written before this
    /// field existed) deserialize with an empty list; the next edit re-runs
    /// extraction and repopulates.
    #[serde(default)]
    pub member_access_refs: Vec<Arc<MemberAccessReferenceData>>,
    /// Whether this (Blade) file is a Volt component — captured once at parse
    /// time (the source is already in hand) so the magic-build's Blade pass can
    /// route Volt vs. controller-rendered resolution without re-reading the
    /// file. Critical on projects with large published Blade sets (e.g. Flux's
    /// ~58k FontAwesome icon templates): without it the pass would open every
    /// one just to check the Volt signature. Always `false` for `.php` files.
    #[serde(default)]
    pub is_volt: bool,
    /// Blade `@foreach`/`@forelse` loops in this file — item variable, iterable
    /// expression, and line range. Lets the magic-build type a loop variable
    /// (`@foreach($users as $user) … {{ $user->email }}`) from its iterable's
    /// element type without re-reading the file at build time. Captured at parse
    /// (source in hand). Empty for `.php` files.
    #[serde(default)]
    pub blade_loops: Vec<BladeLoopVar>,
    /// Sorted index of all patterns by (line, column) for O(log n) lookup.
    /// Skipped during (de)serialization — when loading from the on-disk
    /// cache, the caller must invoke `build_position_index()` to rebuild
    /// this. We don't persist it because (a) it duplicates data already
    /// in the Vec fields above, (b) rebuilding is O(n log n) and fast,
    /// and (c) PatternAtPosition's Arc fields would deserialize as
    /// independent allocations, which is wasteful.
    #[serde(skip)]
    sorted_positions: Vec<PositionEntry>,
}

/// A pattern found at a specific cursor position
/// Uses Rc for cheap cloning (just increments reference count)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum PatternAtPosition {
    View(Arc<ViewReferenceData>),
    Component(Arc<ComponentReferenceData>),
    Directive(Arc<DirectiveReferenceData>),
    EnvRef(Arc<EnvReferenceData>),
    ConfigRef(Arc<ConfigReferenceData>),
    Livewire(Arc<LivewireReferenceData>),
    Middleware(Arc<MiddlewareReferenceData>),
    Translation(Arc<TranslationReferenceData>),
    Asset(Arc<AssetReferenceData>),
    Binding(Arc<BindingReferenceData>),
    Route(Arc<RouteReferenceData>),
    Url(Arc<UrlReferenceData>),
    Action(Arc<ActionReferenceData>),
    Feature(Arc<FeatureReferenceData>),
    MemberAccess(Arc<MemberAccessReferenceData>),
}

impl ParsedPatternsData {
    /// Build the sorted position index from all pattern vectors
    /// This should be called after populating the pattern vectors
    pub fn build_position_index(&mut self) {
        let mut entries = Vec::new();

        // Add all patterns to the index
        for comp in &self.components {
            entries.push(PositionEntry {
                line: comp.line,
                column: comp.column,
                end_column: comp.end_column,
                pattern: PatternAtPosition::Component(comp.clone()),
            });
        }

        for lw in &self.livewire_refs {
            entries.push(PositionEntry {
                line: lw.line,
                column: lw.column,
                end_column: lw.end_column,
                pattern: PatternAtPosition::Livewire(lw.clone()),
            });
        }

        for dir in &self.directives {
            entries.push(PositionEntry {
                line: dir.line,
                column: dir.column,
                end_column: dir.end_column,
                pattern: PatternAtPosition::Directive(dir.clone()),
            });
        }

        for view in &self.views {
            entries.push(PositionEntry {
                line: view.line,
                column: view.column,
                end_column: view.end_column,
                pattern: PatternAtPosition::View(view.clone()),
            });
        }

        for env in &self.env_refs {
            entries.push(PositionEntry {
                line: env.line,
                column: env.column,
                end_column: env.end_column,
                pattern: PatternAtPosition::EnvRef(env.clone()),
            });
        }

        for config in &self.config_refs {
            entries.push(PositionEntry {
                line: config.line,
                column: config.column,
                end_column: config.end_column,
                pattern: PatternAtPosition::ConfigRef(config.clone()),
            });
        }

        for mw in &self.middleware_refs {
            entries.push(PositionEntry {
                line: mw.line,
                column: mw.column,
                end_column: mw.end_column,
                pattern: PatternAtPosition::Middleware(mw.clone()),
            });
        }

        for trans in &self.translation_refs {
            entries.push(PositionEntry {
                line: trans.line,
                column: trans.column,
                end_column: trans.end_column,
                pattern: PatternAtPosition::Translation(trans.clone()),
            });
        }

        for asset in &self.asset_refs {
            entries.push(PositionEntry {
                line: asset.line,
                column: asset.column,
                end_column: asset.end_column,
                pattern: PatternAtPosition::Asset(asset.clone()),
            });
        }

        for binding in &self.binding_refs {
            entries.push(PositionEntry {
                line: binding.line,
                column: binding.column,
                end_column: binding.end_column,
                pattern: PatternAtPosition::Binding(binding.clone()),
            });
        }

        for route in &self.route_refs {
            entries.push(PositionEntry {
                line: route.line,
                column: route.column,
                end_column: route.end_column,
                pattern: PatternAtPosition::Route(route.clone()),
            });
        }

        for url in &self.url_refs {
            entries.push(PositionEntry {
                line: url.line,
                column: url.column,
                end_column: url.end_column,
                pattern: PatternAtPosition::Url(url.clone()),
            });
        }

        for action in &self.action_refs {
            entries.push(PositionEntry {
                line: action.line,
                column: action.column,
                end_column: action.end_column,
                pattern: PatternAtPosition::Action(action.clone()),
            });
        }

        for feature in &self.feature_refs {
            entries.push(PositionEntry {
                line: feature.line,
                column: feature.column,
                end_column: feature.end_column,
                pattern: PatternAtPosition::Feature(feature.clone()),
            });
        }

        for member in &self.member_access_refs {
            entries.push(PositionEntry {
                line: member.line,
                column: member.column,
                end_column: member.end_column,
                pattern: PatternAtPosition::MemberAccess(member.clone()),
            });
        }

        // Sort by (line, column) for efficient binary search
        entries.sort_by(|a, b| a.line.cmp(&b.line).then_with(|| a.column.cmp(&b.column)));

        self.sorted_positions = entries;
    }

    /// Find a pattern at the given cursor position (line, column)
    /// Uses binary search for O(log n) lookup to find the line, then linear scan within line
    pub fn find_at_position(&self, line: u32, column: u32) -> Option<PatternAtPosition> {
        if self.sorted_positions.is_empty() {
            return None;
        }

        // Binary search to find the first entry on or after target line
        let start_idx = self.sorted_positions.partition_point(|e| e.line < line);

        // Scan entries on this line
        for entry in &self.sorted_positions[start_idx..] {
            // Stop when we've passed the target line
            if entry.line > line {
                break;
            }

            // Check if cursor is within this pattern's column range
            if column >= entry.column && column <= entry.end_column {
                return Some(entry.pattern.clone());
            }
        }

        None
    }
}

// ============================================================================
// Actor Pattern - For async integration
// ============================================================================

/// Requests that can be sent to the Salsa actor
pub enum SalsaRequest {
    /// Update or create a file in the database
    UpdateFile {
        path: PathBuf,
        version: i32,
        text: String,
        reply: oneshot::Sender<()>,
    },
    /// Get parsed patterns for a file
    GetPatterns {
        path: PathBuf,
        reply: oneshot::Sender<Option<Arc<ParsedPatternsData>>>,
    },
    /// Get parsed loop blocks for a Blade file
    GetLoopBlocks {
        path: PathBuf,
        reply: oneshot::Sender<Option<Arc<Vec<crate::blade_loops::BladeLoopBlock>>>>,
    },
    /// Get parsed @php block assignments for a Blade file
    GetPhpAssignments {
        path: PathBuf,
        reply: oneshot::Sender<Option<Arc<Vec<(String, String)>>>>,
    },
    /// Get the document-symbol tree for a file (drives textDocument/documentSymbol)
    GetDocumentSymbols {
        path: PathBuf,
        reply: oneshot::Sender<Option<Arc<Vec<crate::document_symbols::SymbolEntry>>>>,
    },
    /// Resolve a `$this->X` member access in a Livewire component PHP file
    /// (auto-registers the file as a Salsa input via mtime-based invalidation).
    ResolveLivewireMember {
        path: PathBuf,
        member: String,
        reply: oneshot::Sender<Option<String>>,
    },
    /// Remove a file from the database
    RemoveFile {
        path: PathBuf,
        reply: oneshot::Sender<()>,
    },

    /// Tell the actor to update its per-category file lists in
    /// response to a filesystem event from the language client. The
    /// path is classified against the roots captured at the last
    /// `register_project_files` call; if it falls under a known root,
    /// it's added to (or removed from) that category's list.
    ///
    /// Returns the assigned `FileCategory` as a tuple-stringified
    /// label, or `None` if the path didn't match any project root
    /// (the actor silently ignored it). Used for logging — the
    /// watcher handler isn't required to do anything with the value.
    UpdateProjectFileList {
        path: PathBuf,
        op: FileListOp,
        reply: oneshot::Sender<Option<&'static str>>,
    },

    /// Rebuild the symbol_index from the current pattern_cache. Sent
    /// once after warming completes so the first find-references query
    /// is fast. Replies with the total entry count for logging.
    ///
    /// Watcher events don't need to send this — they incrementally
    /// update the index via `mark_dirty` from inside the relevant
    /// handlers, processed lazily on next query.
    BuildSymbolIndex { reply: oneshot::Sender<usize> },

    // === Config Management ===
    /// Register configuration files for the project
    RegisterConfigFiles {
        root_path: PathBuf,
        composer_json: Option<String>,
        view_config: Option<String>,
        livewire_config: Option<String>,
        reply: oneshot::Sender<()>,
    },
    /// Update a specific configuration file
    UpdateConfigFile {
        path: PathBuf,
        text: String,
        reply: oneshot::Sender<()>,
    },
    /// Get the current Laravel configuration
    GetLaravelConfig {
        reply: oneshot::Sender<Option<LaravelConfigData>>,
    },

    // === Reference Finding ===
    /// Register project files for reference finding
    /// Scans directories and registers all PHP/Blade files
    RegisterProjectFiles {
        root_path: PathBuf,
        controller_paths: Vec<PathBuf>,
        view_paths: Vec<PathBuf>,
        livewire_path: Option<PathBuf>,
        routes_path: PathBuf,
        reply: oneshot::Sender<()>,
    },
    /// Find all references to a specific view across the project
    FindViewReferences {
        view_name: String,
        reply: oneshot::Sender<Vec<ViewReferenceLocationData>>,
    },
    /// Find all references to a classified symbol across the project.
    /// Iterates `ProjectFiles` and filters parser-classified patterns by name —
    /// never matches by raw string shape.
    FindReferences {
        symbol: SymbolRefData,
        include_declaration: bool,
        reply: oneshot::Sender<Vec<ReferenceLocationData>>,
    },
    /// Count references for a symbol straight from the inverted index — the
    /// cheap, lazy primitive behind code-lens `resolve` (#59). Unlike
    /// `FindReferences` it does no dirty-refresh / project walk; it's a direct
    /// `symbol_index` lookup returning the occurrence count.
    CountSymbolReferences {
        symbol: SymbolRefData,
        reply: oneshot::Sender<usize>,
    },
    /// Return every project file path the actor currently has registered.
    /// Used by the warming task to compute which files to parse out-of-band.
    ListProjectFiles {
        reply: oneshot::Sender<Vec<PathBuf>>,
    },
    /// Bulk-import a batch of pre-parsed `ParsedPatternsData` into the
    /// actor's pattern cache. The warming task uses this to push the
    /// results of parallel out-of-actor parsing back into the cache in
    /// one shot, instead of paying the per-file actor round-trip cost.
    BulkImportPatterns {
        entries: Vec<(PathBuf, Arc<ParsedPatternsData>)>,
        reply: oneshot::Sender<usize>,
    },
    /// Bulk-import class-hierarchy nodes parsed out-of-actor during warming.
    /// Each entry's nodes replace any existing entry for that path. Replies
    /// with the total class count after import (for logging).
    BulkImportHierarchy {
        entries: Vec<(PathBuf, Vec<crate::class_hierarchy_index::ClassNode>)>,
        reply: oneshot::Sender<usize>,
    },
    /// Snapshot the `fqcn → declaring file` map for the out-of-actor
    /// magic-member index build (M4).
    SnapshotClassFiles {
        reply: oneshot::Sender<Arc<std::collections::HashMap<String, PathBuf>>>,
    },
    /// Snapshot every indexed class grouped by file, so warming can persist
    /// the hierarchy to the disk cache.
    SnapshotHierarchyNodes {
        reply: oneshot::Sender<
            std::collections::HashMap<PathBuf, Vec<crate::class_hierarchy_index::ClassNode>>,
        >,
    },
    /// Surface signatures (`fqcn → u64`) for every class `path` declares.
    /// The save flow snapshots this *before* pushing the saved buffer into
    /// Salsa, then diffs against the re-parse to decide whether the edit
    /// could affect other files (incremental refresh, #80).
    FileClassSurfaces {
        path: PathBuf,
        reply: oneshot::Sender<std::collections::HashMap<String, u64>>,
    },
    /// Expand `seeds` to include every transitive descendant (subclasses,
    /// implementers, trait users) — the class-level blast radius of a
    /// surface change.
    ExpandClassDescendants {
        seeds: Vec<String>,
        reply: oneshot::Sender<std::collections::HashSet<String>>,
    },
    /// Export every magic-member entry grouped by usage file, for the
    /// incremental magic-cache re-save (#80).
    ExportMagicMembers {
        reply: oneshot::Sender<
            std::collections::HashMap<PathBuf, Vec<crate::symbol_index::MagicMemberEntry>>,
        >,
    },
    /// Bulk-import resolved magic-member occurrences into the symbol index
    /// (M4). Appends to each path's existing (literal-symbol) entries.
    BulkImportMagicMembers {
        entries: Vec<(PathBuf, Vec<crate::symbol_index::MagicMemberEntry>)>,
        reply: oneshot::Sender<usize>,
    },
    /// Re-index a single file's symbols after an edit (instant per-file half of
    /// the incremental refresh): drop the file's prior keys, re-insert its
    /// literal symbols from the current pattern cache, then insert the freshly
    /// resolved magic members. Keeps find-references on the edited file current
    /// without a project-wide rebuild.
    ReindexFileMagic {
        path: PathBuf,
        entries: Vec<crate::symbol_index::MagicMemberEntry>,
        reply: oneshot::Sender<()>,
    },
    /// find-references for the magic member under the cursor (M4): resolve the
    /// `member_access` site at `(line, column)` and return its indexed usages.
    FindMemberReferences {
        path: PathBuf,
        line: u32,
        column: u32,
        reply: oneshot::Sender<Vec<ReferenceLocationData>>,
    },

    /// Resolve + classify the magic member at a cursor position for a hover
    /// card (M6). Returns the classification, not references.
    ResolveMagicMemberAt {
        path: PathBuf,
        line: u32,
        column: u32,
        reply: oneshot::Sender<Option<MagicMemberHoverData>>,
    },

    /// Resolve the magic member at a cursor for rename (M7) — method-backed
    /// kinds only; returns the declaring method to rewrite.
    ResolveMagicMemberRenameAt {
        path: PathBuf,
        line: u32,
        column: u32,
        reply: oneshot::Sender<Option<MagicMemberRenameData>>,
    },

    // === Service Provider Management ===
    /// Register the service provider registry from the existing analyzer
    RegisterServiceProviderRegistry {
        middleware_aliases: std::collections::HashMap<String, MiddlewareRegistrationData>,
        bindings: std::collections::HashMap<String, BindingRegistrationData>,
        singletons: std::collections::HashMap<String, BindingRegistrationData>,
        reply: oneshot::Sender<()>,
    },
    /// Get middleware by alias
    GetMiddlewareByAlias {
        alias: String,
        reply: oneshot::Sender<Option<MiddlewareRegistrationData>>,
    },
    /// Get binding by name
    GetBindingByName {
        name: String,
        reply: oneshot::Sender<Option<BindingRegistrationData>>,
    },
    /// Get view namespace by name (e.g., "courier" -> view path)
    GetViewNamespace {
        namespace: String,
        reply: oneshot::Sender<Option<ViewNamespaceData>>,
    },
    /// Get all view namespaces (for autocomplete)
    GetAllViewNamespaces {
        reply: oneshot::Sender<Vec<ViewNamespaceData>>,
    },
    /// Get a Blade component by tag name (e.g., "package-alert")
    GetBladeComponentReg {
        tag_name: String,
        reply: oneshot::Sender<Option<BladeComponentRegData>>,
    },
    /// Get all registered Blade components
    GetAllBladeComponentRegs {
        reply: oneshot::Sender<Vec<BladeComponentRegData>>,
    },
    /// Get component namespace by prefix (e.g., "nightshade")
    GetComponentNamespace {
        prefix: String,
        reply: oneshot::Sender<Option<ComponentNamespaceData>>,
    },
    /// Get all component namespaces
    GetAllComponentNamespaces {
        reply: oneshot::Sender<Vec<ComponentNamespaceData>>,
    },

    // === Environment Variable Management ===
    /// Register environment variables from the env cache
    RegisterEnvVariables {
        variables: std::collections::HashMap<String, EnvVariableData>,
        reply: oneshot::Sender<()>,
    },
    /// Get an environment variable by name
    GetEnvVariable {
        name: String,
        reply: oneshot::Sender<Option<EnvVariableData>>,
    },
    /// Get all environment variable names (for autocomplete)
    GetEnvVariableNames { reply: oneshot::Sender<Vec<String>> },

    // === Salsa-based Environment Variable Management (New) ===
    /// Register a raw .env file for Salsa to parse
    RegisterEnvSource {
        path: PathBuf,
        text: String,
        priority: u8, // 0=.env.example, 1=.env.local, 2=.env
        reply: oneshot::Sender<()>,
    },
    /// Get a parsed env variable from Salsa
    GetParsedEnvVar {
        name: String,
        reply: oneshot::Sender<Option<ParsedEnvVarData>>,
    },
    /// Get all parsed env variables from Salsa
    GetAllParsedEnvVars {
        reply: oneshot::Sender<Vec<ParsedEnvVarData>>,
    },

    // === Salsa-based Service Provider Management (New) ===
    /// Register a raw service provider file for Salsa to parse
    RegisterServiceProviderSource {
        path: PathBuf,
        text: String,
        priority: u8, // 0=framework, 1=package, 2=app
        root_path: PathBuf,
        reply: oneshot::Sender<()>,
    },
    /// Get middleware from Salsa-parsed service providers
    GetParsedMiddleware {
        alias: String,
        reply: oneshot::Sender<Option<ParsedMiddlewareData>>,
    },
    /// Get all parsed middleware from Salsa
    GetAllParsedMiddleware {
        reply: oneshot::Sender<Vec<ParsedMiddlewareData>>,
    },
    /// Get binding from Salsa-parsed service providers
    GetParsedBinding {
        name: String,
        reply: oneshot::Sender<Option<ParsedBindingData>>,
    },
    /// Get all parsed bindings from Salsa
    GetAllParsedBindings {
        reply: oneshot::Sender<Vec<ParsedBindingData>>,
    },

    // === Cache-based Registration ===
    /// Register a middleware entry from disk cache
    RegisterCachedMiddleware {
        alias: String,
        class: String,
        class_file: Option<String>,
        source_file: Option<String>,
        line: u32,
        reply: oneshot::Sender<()>,
    },
    /// Register a binding entry from disk cache
    RegisterCachedBinding {
        name: String,
        class: String,
        binding_type: String,
        class_file: Option<String>,
        source_file: Option<String>,
        line: u32,
        reply: oneshot::Sender<()>,
    },

    /// Register multiple middleware entries from disk cache (batch)
    RegisterCachedMiddlewareBatch {
        entries: Vec<(String, String, Option<String>, Option<String>, u32)>, // (alias, class, class_file, source_file, line)
        reply: oneshot::Sender<()>,
    },
    /// Register multiple binding entries from disk cache (batch)
    RegisterCachedBindingBatch {
        entries: Vec<(String, String, String, Option<String>, Option<String>, u32)>, // (name, class, binding_type, class_file, source_file, line)
        reply: oneshot::Sender<()>,
    },

    /// Register Laravel config from disk cache (bypasses parsing).
    /// Boxed because `LaravelConfigData` is by far the largest payload of any
    /// `SalsaRequest` variant; keeping it inline bloats every message (see
    /// clippy::large_enum_variant).
    RegisterCachedConfig {
        config: Box<LaravelConfigData>,
        reply: oneshot::Sender<()>,
    },

    /// Register env variables from disk cache (bypasses parsing)
    RegisterCachedEnvVars {
        variables: std::collections::HashMap<String, String>,
        reply: oneshot::Sender<()>,
    },

    /// Shutdown the actor
    Shutdown,
}

/// Handle to communicate with the Salsa actor
#[derive(Clone)]
pub struct SalsaHandle {
    sender: mpsc::Sender<SalsaRequest>,
    /// Shared concurrent pattern cache — same `Arc<DashMap>` the actor
    /// holds. Reads and writes from here NEVER go through the actor's
    /// mpsc channel, which means they're never blocked behind a slow
    /// handler. See the comment on `SalsaActor::pattern_cache` for why
    /// this exists.
    pattern_cache: Arc<dashmap::DashMap<PathBuf, (i32, Arc<ParsedPatternsData>)>>,
}

impl SalsaHandle {
    /// Borrow the shared pattern cache directly. The on-disk cache module
    /// uses this to pre-load entries before warming starts, and to read
    /// them back out after warming completes for persistence. Returned
    /// `Arc` is cheap to clone — the underlying `DashMap` is the same
    /// instance the actor reads from in `handle_get_patterns`.
    pub fn pattern_cache(&self) -> Arc<dashmap::DashMap<PathBuf, (i32, Arc<ParsedPatternsData>)>> {
        self.pattern_cache.clone()
    }

    /// Update or create a file in the database
    pub async fn update_file(
        &self,
        path: PathBuf,
        version: i32,
        text: String,
    ) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::UpdateFile {
                path,
                version,
                text,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get parsed patterns for a file
    /// Returns Arc for efficient sharing without cloning the entire data structure
    pub async fn get_patterns(
        &self,
        path: PathBuf,
    ) -> Result<Option<Arc<ParsedPatternsData>>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetPatterns {
                path,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get parsed Blade loop blocks for a file.
    /// Memoized — returns the same Arc on repeated calls until the file version changes.
    pub async fn get_loop_blocks(
        &self,
        path: PathBuf,
    ) -> Result<Option<Arc<Vec<crate::blade_loops::BladeLoopBlock>>>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetLoopBlocks {
                path,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get parsed `@php` block assignments for a Blade file.
    /// Memoized — returns the same Arc on repeated calls until the file version changes.
    pub async fn get_php_assignments(
        &self,
        path: PathBuf,
    ) -> Result<Option<Arc<Vec<(String, String)>>>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetPhpAssignments {
                path,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get the document-symbol tree for a file. Powers textDocument/documentSymbol.
    /// Memoized — returns the same Arc on repeated calls until the file version changes.
    pub async fn get_document_symbols(
        &self,
        path: PathBuf,
    ) -> Result<Option<Arc<Vec<crate::document_symbols::SymbolEntry>>>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetDocumentSymbols {
                path,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Resolve a `$this->X` member access in a Livewire component PHP file.
    /// Auto-registers the file as a Salsa input on first access, invalidates on mtime change.
    pub async fn resolve_livewire_member(
        &self,
        path: PathBuf,
        member: String,
    ) -> Result<Option<String>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::ResolveLivewireMember {
                path,
                member,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Remove a file from the database
    pub async fn remove_file(&self, path: PathBuf) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::RemoveFile {
                path,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Build (or rebuild) the inverted symbol index from the current
    /// pattern cache. Called by the warming task once warming finishes
    /// so the first `find-references` query is O(1) rather than
    /// O(N files). Returns the total entry count for logging.
    pub async fn build_symbol_index(&self) -> Result<usize, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::BuildSymbolIndex { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Update the per-category project file lists in response to a
    /// filesystem event. `Add` is for `Created` notifications, `Remove`
    /// for `Deleted`. Returns the category label the actor classified
    /// the path under (useful for logging), or `None` if the path
    /// didn't match any indexed project root.
    pub async fn update_project_file_list(
        &self,
        path: PathBuf,
        op: FileListOp,
    ) -> Result<Option<&'static str>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::UpdateProjectFileList {
                path,
                op,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Shutdown the actor gracefully
    pub async fn shutdown(&self) -> Result<(), &'static str> {
        self.sender
            .send(SalsaRequest::Shutdown)
            .await
            .map_err(|_| "Salsa actor already disconnected")
    }

    // === Config Methods ===

    /// Register configuration files for the project
    pub async fn register_config_files(
        &self,
        root_path: PathBuf,
        composer_json: Option<String>,
        view_config: Option<String>,
        livewire_config: Option<String>,
    ) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::RegisterConfigFiles {
                root_path,
                composer_json,
                view_config,
                livewire_config,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Update a specific configuration file
    pub async fn update_config_file(
        &self,
        path: PathBuf,
        text: String,
    ) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::UpdateConfigFile {
                path,
                text,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get the current Laravel configuration
    pub async fn get_laravel_config(&self) -> Result<Option<LaravelConfigData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetLaravelConfig { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    // === Reference Finding Methods ===

    /// Register project files for reference finding
    /// Scans the provided directories and registers all PHP/Blade files with Salsa
    pub async fn register_project_files(
        &self,
        root_path: PathBuf,
        controller_paths: Vec<PathBuf>,
        view_paths: Vec<PathBuf>,
        livewire_path: Option<PathBuf>,
        routes_path: PathBuf,
    ) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::RegisterProjectFiles {
                root_path,
                controller_paths,
                view_paths,
                livewire_path,
                routes_path,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Find all references to a specific view across the project
    /// Returns cached results when possible, only scanning changed files
    pub async fn find_view_references(
        &self,
        view_name: String,
    ) -> Result<Vec<ViewReferenceLocationData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::FindViewReferences {
                view_name,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Find all parser-classified references to a symbol across the project.
    pub async fn find_references(
        &self,
        symbol: SymbolRefData,
        include_declaration: bool,
    ) -> Result<Vec<ReferenceLocationData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::FindReferences {
                symbol,
                include_declaration,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Count references for `symbol` directly from the inverted index (cheap;
    /// no project walk). Backs code-lens `resolve`.
    pub async fn count_symbol_references(
        &self,
        symbol: SymbolRefData,
    ) -> Result<usize, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::CountSymbolReferences {
                symbol,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Return every project file path the actor currently has registered.
    pub async fn list_project_files(&self) -> Result<Vec<PathBuf>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::ListProjectFiles { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Bulk-import pre-parsed patterns into the shared pattern cache.
    ///
    /// **Does NOT go through the actor mpsc channel.** Earlier revisions
    /// routed this through the actor and we observed a 65-second stall
    /// per cold start on a 40k-file project — the actor's `blocking_recv`
    /// thread was not waking up when the warming task sent its message,
    /// and only un-stalled when an unrelated `did_open` arrived. We never
    /// fully pinned down the wake-up failure, but the architectural fix
    /// is correct regardless: pattern_cache writes are pure data ops and
    /// have no Salsa-mutable-db requirements, so they shouldn't be
    /// serialized through the actor's single-threaded request queue.
    ///
    /// This is now a tight synchronous loop of `DashMap::insert` calls.
    /// Real-world cost: ~7ms for 40,589 entries (per earlier bench).
    /// The `async fn` and `Result` shape is preserved for source
    /// compatibility with the existing call sites.
    pub async fn bulk_import_patterns(
        &self,
        entries: Vec<(PathBuf, Arc<ParsedPatternsData>)>,
    ) -> Result<usize, &'static str> {
        let total = entries.len();
        for (path, data) in entries {
            self.pattern_cache.insert(path, (0, data));
        }
        Ok(total)
    }

    /// Bulk-import class-hierarchy nodes into the actor-owned index. Unlike
    /// `bulk_import_patterns` (which writes the shared cache directly), the
    /// hierarchy index lives inside the actor, so this round-trips through
    /// the request queue. Replies with the total class count after import.
    pub async fn bulk_import_hierarchy(
        &self,
        entries: Vec<(PathBuf, Vec<crate::class_hierarchy_index::ClassNode>)>,
    ) -> Result<usize, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::BulkImportHierarchy {
                entries,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Snapshot the actor's `fqcn → declaring file` map. The magic-member
    /// index build (M4) runs in a parallel pass outside the actor and uses
    /// this owned copy to resolve receivers without borrowing the index.
    pub async fn snapshot_class_files(
        &self,
    ) -> Result<Arc<std::collections::HashMap<String, PathBuf>>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::SnapshotClassFiles { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Snapshot every indexed class grouped by declaring file, so warming can
    /// persist the hierarchy to the disk cache (it survives a warm restart
    /// only if persisted — fresh parses are the sole other populator).
    /// Surface signatures for every class `path` currently declares (empty
    /// if the file is unknown to the hierarchy). Snapshot side of the
    /// save-time surface diff (#80).
    pub async fn file_class_surfaces(
        &self,
        path: PathBuf,
    ) -> Result<std::collections::HashMap<String, u64>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::FileClassSurfaces {
                path,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// `seeds` plus every transitive descendant — the class-level blast
    /// radius of a surface change (#80).
    pub async fn expand_class_descendants(
        &self,
        seeds: Vec<String>,
    ) -> Result<std::collections::HashSet<String>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::ExpandClassDescendants {
                seeds,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Export every magic-member entry grouped by usage file — the live
    /// index contents, for the incremental magic-cache re-save (#80).
    pub async fn export_magic_members(
        &self,
    ) -> Result<
        std::collections::HashMap<PathBuf, Vec<crate::symbol_index::MagicMemberEntry>>,
        &'static str,
    > {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::ExportMagicMembers { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    pub async fn snapshot_hierarchy_nodes(
        &self,
    ) -> Result<
        std::collections::HashMap<PathBuf, Vec<crate::class_hierarchy_index::ClassNode>>,
        &'static str,
    > {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::SnapshotHierarchyNodes { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Bulk-import resolved magic-member occurrences into the actor-owned
    /// symbol index (M4). Mirrors `bulk_import_hierarchy`; replies with the
    /// total magic-member entry count ingested.
    pub async fn bulk_import_magic_members(
        &self,
        entries: Vec<(PathBuf, Vec<crate::symbol_index::MagicMemberEntry>)>,
    ) -> Result<usize, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::BulkImportMagicMembers {
                entries,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Re-index a single edited file's symbols (instant per-file refresh):
    /// evict its prior keys, re-add literals from the pattern cache, then insert
    /// the freshly resolved magic members.
    pub async fn reindex_file_magic(
        &self,
        path: PathBuf,
        entries: Vec<crate::symbol_index::MagicMemberEntry>,
    ) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::ReindexFileMagic {
                path,
                entries,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// find-references for the magic member under the cursor (M4). The actor
    /// resolves the `member_access` site at `(line, column)` to its declaring
    /// class + member, then returns every indexed usage of that key. Empty
    /// when the cursor isn't on a resolvable magic member.
    pub async fn find_member_references(
        &self,
        path: PathBuf,
        line: u32,
        column: u32,
    ) -> Result<Vec<ReferenceLocationData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::FindMemberReferences {
                path,
                line,
                column,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Resolve + classify the magic member at `(line, column)` for a hover card
    /// (M6). `Ok(None)` when the position isn't a resolvable magic member.
    pub async fn resolve_magic_member_at(
        &self,
        path: PathBuf,
        line: u32,
        column: u32,
    ) -> Result<Option<MagicMemberHoverData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::ResolveMagicMemberAt {
                path,
                line,
                column,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Resolve the magic member at `(line, column)` for rename (M7). `Ok(None)`
    /// unless it's a method-backed magic member (relationship / scope /
    /// accessor / dynamic finder).
    pub async fn resolve_magic_member_rename_at(
        &self,
        path: PathBuf,
        line: u32,
        column: u32,
    ) -> Result<Option<MagicMemberRenameData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::ResolveMagicMemberRenameAt {
                path,
                line,
                column,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    // === Service Provider Methods ===

    /// Register the service provider registry from the existing analyzer
    pub async fn register_service_provider_registry(
        &self,
        middleware_aliases: std::collections::HashMap<String, MiddlewareRegistrationData>,
        bindings: std::collections::HashMap<String, BindingRegistrationData>,
        singletons: std::collections::HashMap<String, BindingRegistrationData>,
    ) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::RegisterServiceProviderRegistry {
                middleware_aliases,
                bindings,
                singletons,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get middleware by alias
    pub async fn get_middleware_by_alias(
        &self,
        alias: String,
    ) -> Result<Option<MiddlewareRegistrationData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetMiddlewareByAlias {
                alias,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get binding by name
    pub async fn get_binding_by_name(
        &self,
        name: String,
    ) -> Result<Option<BindingRegistrationData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetBindingByName {
                name,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get view namespace by name (for resolving package::view syntax)
    pub async fn get_view_namespace(
        &self,
        namespace: String,
    ) -> Result<Option<ViewNamespaceData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetViewNamespace {
                namespace,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get all view namespaces (for autocomplete)
    pub async fn get_all_view_namespaces(&self) -> Result<Vec<ViewNamespaceData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetAllViewNamespaces { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get a Blade component registration by tag name
    pub async fn get_blade_component_reg(
        &self,
        tag_name: String,
    ) -> Result<Option<BladeComponentRegData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetBladeComponentReg {
                tag_name,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get all registered Blade components
    pub async fn get_all_blade_component_regs(
        &self,
    ) -> Result<Vec<BladeComponentRegData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetAllBladeComponentRegs { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get component namespace by prefix (for resolving <x-package::component>)
    pub async fn get_component_namespace(
        &self,
        prefix: String,
    ) -> Result<Option<ComponentNamespaceData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetComponentNamespace {
                prefix,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get all component namespaces
    pub async fn get_all_component_namespaces(
        &self,
    ) -> Result<Vec<ComponentNamespaceData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetAllComponentNamespaces { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    // === Environment Variable Methods ===

    /// Register environment variables from the env cache
    pub async fn register_env_variables(
        &self,
        variables: std::collections::HashMap<String, EnvVariableData>,
    ) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::RegisterEnvVariables {
                variables,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get an environment variable by name
    pub async fn get_env_variable(
        &self,
        name: String,
    ) -> Result<Option<EnvVariableData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetEnvVariable {
                name,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get all environment variable names (for autocomplete)
    pub async fn get_env_variable_names(&self) -> Result<Vec<String>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetEnvVariableNames { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    // === Salsa-based Environment Variable Methods (New - Phase 1) ===

    /// Register a raw .env file for Salsa to parse
    /// This replaces the old EnvFileCache by having Salsa do the parsing
    pub async fn register_env_source(
        &self,
        path: PathBuf,
        text: String,
        priority: u8,
    ) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::RegisterEnvSource {
                path,
                text,
                priority,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get a parsed env variable from Salsa
    /// Returns the highest-priority variable if multiple files define the same var
    pub async fn get_parsed_env_var(
        &self,
        name: String,
    ) -> Result<Option<ParsedEnvVarData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetParsedEnvVar {
                name,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get all parsed env variables from Salsa (merged by priority)
    pub async fn get_all_parsed_env_vars(&self) -> Result<Vec<ParsedEnvVarData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetAllParsedEnvVars { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    // === Salsa-based Service Provider Methods (New - Phase 1) ===

    /// Register a raw service provider file for Salsa to parse
    /// This replaces the old ServiceProviderRegistry by having Salsa do the parsing
    pub async fn register_service_provider_source(
        &self,
        path: PathBuf,
        text: String,
        priority: u8,
        root_path: PathBuf,
    ) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::RegisterServiceProviderSource {
                path,
                text,
                priority,
                root_path,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get middleware by alias from Salsa-parsed service providers
    /// Returns the highest-priority middleware if multiple providers define the same alias
    pub async fn get_parsed_middleware(
        &self,
        alias: String,
    ) -> Result<Option<ParsedMiddlewareData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetParsedMiddleware {
                alias,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get all parsed middleware from Salsa (merged by priority)
    pub async fn get_all_parsed_middleware(
        &self,
    ) -> Result<Vec<ParsedMiddlewareData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetAllParsedMiddleware { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get binding by name from Salsa-parsed service providers
    /// Returns the highest-priority binding if multiple providers define the same name
    pub async fn get_parsed_binding(
        &self,
        name: String,
    ) -> Result<Option<ParsedBindingData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetParsedBinding {
                name,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get all parsed bindings from Salsa (merged by priority)
    pub async fn get_all_parsed_bindings(&self) -> Result<Vec<ParsedBindingData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetAllParsedBindings { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    // === Cache-based Registration Methods ===

    /// Register a middleware entry from disk cache
    pub async fn register_cached_middleware(
        &self,
        alias: String,
        class: String,
        class_file: Option<String>,
        source_file: Option<String>,
        line: u32,
    ) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::RegisterCachedMiddleware {
                alias,
                class,
                class_file,
                source_file,
                line,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Register a binding entry from disk cache
    pub async fn register_cached_binding(
        &self,
        name: String,
        class: String,
        binding_type: String,
        class_file: Option<String>,
        source_file: Option<String>,
        line: u32,
    ) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::RegisterCachedBinding {
                name,
                class,
                binding_type,
                class_file,
                source_file,
                line,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Register multiple middleware entries from disk cache (batch - single round-trip)
    pub async fn register_cached_middleware_batch(
        &self,
        entries: Vec<(String, String, Option<String>, Option<String>, u32)>, // (alias, class, class_file, source_file, line)
    ) -> Result<(), &'static str> {
        if entries.is_empty() {
            return Ok(());
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::RegisterCachedMiddlewareBatch {
                entries,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Register multiple binding entries from disk cache (batch - single round-trip)
    pub async fn register_cached_binding_batch(
        &self,
        entries: Vec<(String, String, String, Option<String>, Option<String>, u32)>, // (name, class, binding_type, class_file, source_file, line)
    ) -> Result<(), &'static str> {
        if entries.is_empty() {
            return Ok(());
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::RegisterCachedBindingBatch {
                entries,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Register Laravel config from disk cache (bypasses parsing)
    pub async fn register_cached_config(
        &self,
        config: LaravelConfigData,
    ) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::RegisterCachedConfig {
                config: Box::new(config),
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Register env variables from disk cache (bypasses parsing)
    pub async fn register_cached_env_vars(
        &self,
        variables: std::collections::HashMap<String, String>,
    ) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::RegisterCachedEnvVars {
                variables,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }
}

/// Absolute root directories captured at `register_project_files` time.
/// Used by the file-watcher handler to classify newly-created paths
/// into the right per-category list without re-doing the directory
/// walk. Each `*_roots` field is a list of absolute paths because some
/// project layouts have multiple roots (e.g. multi-themed view paths).
///
/// Stored on `SalsaActor` (not passed in per request) because watcher
/// notifications arrive asynchronously and need consistent state to
/// classify against.
#[derive(Default, Debug, Clone)]
struct ProjectRootPaths {
    controller_roots: Vec<PathBuf>,
    view_roots: Vec<PathBuf>,
    livewire_root: Option<PathBuf>,
    routes_root: Option<PathBuf>,
    vendor_root: Option<PathBuf>,
}

impl ProjectRootPaths {
    /// Classify an absolute path into the file-category list it
    /// belongs to. Order matters here: vendor wins over views even
    /// though a published `vendor/<pkg>/resources/views/foo.blade.php`
    /// technically lives under both `vendor/` AND a view path; we
    /// treat it as vendor because that's where its source-of-truth
    /// content lives. Returns `None` for paths outside every known
    /// root (build artifacts, .git, dotfiles, etc.).
    fn classify(&self, path: &Path) -> Option<FileCategory> {
        if let Some(root) = &self.vendor_root {
            if path.starts_with(root) {
                return Some(FileCategory::Vendor);
            }
        }
        if let Some(root) = &self.livewire_root {
            if path.starts_with(root) {
                return Some(FileCategory::Livewire);
            }
        }
        for root in &self.controller_roots {
            if path.starts_with(root) {
                return Some(FileCategory::Controller);
            }
        }
        if let Some(root) = &self.routes_root {
            if path.starts_with(root) {
                return Some(FileCategory::Route);
            }
        }
        for root in &self.view_roots {
            if path.starts_with(root) {
                return Some(FileCategory::View);
            }
        }
        None
    }
}

/// Discriminant returned by `ProjectRootPaths::classify` so the
/// watcher-update path can pick the right `Vec<PathBuf>` to mutate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileCategory {
    Controller,
    View,
    Livewire,
    Route,
    Vendor,
}

impl FileCategory {
    /// Short label for logging — keeps log lines readable without
    /// pulling in a full Debug derive at the call site.
    fn label(self) -> &'static str {
        match self {
            FileCategory::Controller => "controller",
            FileCategory::View => "view",
            FileCategory::Livewire => "livewire",
            FileCategory::Route => "route",
            FileCategory::Vendor => "vendor",
        }
    }
}

/// Operation for `SalsaRequest::UpdateProjectFileList`. `Add` is sent
/// on a `Created` filesystem event; `Remove` on `Deleted`. There's no
/// "Change" variant because a change to an already-listed file
/// doesn't affect the list — only its contents change, and those flow
/// through `update_file` separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileListOp {
    Add,
    Remove,
}

/// The Salsa actor that owns the database and runs on a dedicated thread
pub struct SalsaActor {
    db: LaravelDatabase,
    receiver: mpsc::Receiver<SalsaRequest>,
    /// Map from path to SourceFile for efficient lookups and updates
    files: HashMap<PathBuf, SourceFile>,
    /// Concurrent map of converted pattern data, SHARED with the SalsaHandle.
    /// Key: file path, Value: (file version, cached patterns wrapped in Arc).
    ///
    /// **Architectural note:** this is intentionally NOT routed through the
    /// actor's mpsc channel. The previous LRU-inside-actor design had a real
    /// production-pathological behaviour: warming would send a single
    /// `BulkImportPatterns` message and the actor's `blocking_recv()` thread
    /// would not get woken up until some unrelated LSP request (typically a
    /// `did_open`) arrived. On a 40k-file project the result was a 65-second
    /// stall every cold start. We never fully tracked down the wake-up
    /// failure (looked like a tokio mpsc + blocking_recv interaction), but
    /// bypassing the actor for cache writes side-steps the problem entirely
    /// AND yields a more correct architecture: pattern_cache reads/writes
    /// are pure data ops with no Salsa-mutable-db requirements, so there's
    /// no reason they should serialize through the actor.
    ///
    /// DashMap is lock-free for reads and uses per-shard locks for writes
    /// (16 shards by default), so contention between the actor's read path
    /// and warming's bulk insert is negligible. ~5KB per entry × 65k cap
    /// = ~320MB worst case. Cap enforced manually on insert.
    pattern_cache: Arc<dashmap::DashMap<PathBuf, (i32, Arc<ParsedPatternsData>)>>,
    /// LRU cache of parsed Blade loop blocks, keyed by file path + version.
    /// Salsa already memoizes the underlying query, but caching the Arc avoids
    /// re-walking the query graph on every diagnostic / completion request.
    loop_blocks_cache: LruCache<PathBuf, (i32, Arc<Vec<crate::blade_loops::BladeLoopBlock>>)>,
    /// LRU cache of parsed `@php ... @endphp` block assignments, keyed by file path + version.
    php_assignments_cache: LruCache<PathBuf, (i32, Arc<Vec<(String, String)>>)>,
    /// LRU cache of document-symbol trees keyed by file path. Stores file
    /// version alongside the cached Arc so a version mismatch triggers a
    /// recompute via the memoized Salsa query.
    document_symbols_cache:
        LruCache<PathBuf, (i32, Arc<Vec<crate::document_symbols::SymbolEntry>>)>,
    /// Tracks the on-disk mtime of Livewire component PHP files registered as Salsa inputs.
    /// These files are not opened in the editor (no `did_open`/`did_change` events), so we
    /// invalidate by comparing filesystem mtime on each access.
    livewire_mtimes: HashMap<PathBuf, std::time::SystemTime>,
    /// Monotonic version counter for Livewire component SourceFiles (incremented per disk re-read).
    livewire_version_counter: i32,

    // === Config Management ===
    /// Project root path
    config_root: Option<PathBuf>,
    /// Configuration files tracked by Salsa
    config_files: HashMap<PathBuf, ConfigFile>,
    /// Config file version counter (incremented on changes)
    config_version: i32,
    /// Cached Laravel config data (version, data)
    config_cache: Option<(i32, LaravelConfigData)>,

    // === Reference Finding ===
    /// Project files input for reference finding
    project_files: Option<ProjectFiles>,
    /// Version counter for project files
    project_files_version: i32,
    /// Categorized file lists for quick lookup
    controller_files: Vec<PathBuf>,
    view_files: Vec<PathBuf>,
    livewire_files: Vec<PathBuf>,
    route_files: Vec<PathBuf>,
    /// Vendor `*.php` and `*.blade.php` files. Composer packages can ship
    /// Livewire components, routes, controllers, views, and translations
    /// just like user code — find-references and goto-definition both
    /// need to see them. We index everything under `vendor/` and rely on
    /// the warming-stage filters (`.json.php` skip, 256KB size cap) to
    /// drop the auto-generated noise.
    vendor_files: Vec<PathBuf>,
    /// Every non-vendor `*.php` / `*.blade.php` in the project (app/, database/,
    /// tests/, config/, resources/, routes/, …). The categorized lists above
    /// only cover controllers + Blade views + routes, which is enough for the
    /// view/route/livewire navigation features but misses the broad source the
    /// magic-member reverse index needs — a `$user->email` usage can live in any
    /// model, service, job, action, or Volt `.php` page. This bucket feeds the
    /// warm parse so those usages are indexed, not just files the user happens
    /// to open. Excludes vendor (covered separately) and noise dirs.
    source_files: Vec<PathBuf>,

    /// Root directories captured from the most recent
    /// `register_project_files` call. We retain them so the file-watcher
    /// handler can classify newly-created paths into the right
    /// per-category list (controllers, views, livewire, routes,
    /// vendor) by checking which root the path falls under.
    ///
    /// All paths here are absolute, so prefix matching against an
    /// incoming absolute event path is a straightforward
    /// `path.starts_with(prefix)`.
    project_root_paths: ProjectRootPaths,

    /// Inverted symbol index — turns find-references from O(N files)
    /// into O(1) hash lookup. Built at warming completion via a
    /// `BuildSymbolIndex` message; kept fresh thereafter via the
    /// `mark_dirty` / `take_dirty` pattern (see `symbol_index.rs`).
    symbol_index: crate::symbol_index::SymbolIndex,

    /// Project-wide class-hierarchy + member index. Populated at warming from
    /// the same parse that feeds the pattern cache; powers structural code
    /// lenses (implementations / usages / overrides / parent) and cross-file
    /// inheritance resolution. See `class_hierarchy_index.rs`.
    class_hierarchy_index: crate::class_hierarchy_index::ClassHierarchyIndex,
    /// Cached class→file map handed to `snapshot_class_files`, shared by `Arc`
    /// so the hot edit path and the debounced rebuild don't re-clone the whole
    /// map every call. Set to `None` whenever the hierarchy's FQCN→file mapping
    /// actually changes (see the invalidation at each mutation site); a typical
    /// method-body edit leaves it intact so the next snapshot is O(1).
    class_files_snapshot: Option<Arc<HashMap<String, PathBuf>>>,

    // === Service Provider Registry ===
    /// Cached middleware aliases from service provider analysis
    sp_middleware_aliases: HashMap<String, MiddlewareRegistrationData>,
    /// Cached bindings from service provider analysis
    sp_bindings: HashMap<String, BindingRegistrationData>,
    /// Cached singletons from service provider analysis
    sp_singletons: HashMap<String, BindingRegistrationData>,
    /// Cached view namespaces from loadViewsFrom() calls
    sp_view_namespaces: HashMap<String, ViewNamespaceData>,
    /// Cached Blade component registrations from Blade::component() calls
    sp_blade_components: HashMap<String, BladeComponentRegData>,
    /// Cached component namespace registrations from Blade::componentNamespace() calls
    sp_component_namespaces: HashMap<String, ComponentNamespaceData>,

    // === Environment Variables ===
    /// Cached environment variables
    env_variables: HashMap<String, EnvVariableData>,

    // === Salsa-based Environment Variable Tracking (New) ===
    /// Env files registered with Salsa for incremental parsing
    salsa_env_files: HashMap<PathBuf, EnvFile>,
    /// Version counter for env files
    salsa_env_version: i32,

    // === Salsa-based Service Provider Tracking (New) ===
    /// Service provider files registered with Salsa for incremental parsing
    salsa_sp_files: HashMap<PathBuf, ServiceProviderFile>,
    /// Version counter for service provider files
    salsa_sp_version: i32,
    /// Project root for service provider resolution
    salsa_sp_root: Option<PathBuf>,
}

impl SalsaActor {
    /// Spawn the actor on a dedicated thread and return a handle for communication
    pub fn spawn() -> SalsaHandle {
        let (tx, rx) = mpsc::channel(256);

        // Shared pattern cache: created here, cloned into both the actor
        // and the SalsaHandle. Both ends use the SAME map so writes from
        // either side are immediately visible on the other.
        let pattern_cache: Arc<dashmap::DashMap<PathBuf, (i32, Arc<ParsedPatternsData>)>> =
            Arc::new(dashmap::DashMap::with_capacity(65536));
        let pattern_cache_for_actor = pattern_cache.clone();

        std::thread::spawn(move || {
            let mut actor = SalsaActor {
                db: LaravelDatabase::new(),
                receiver: rx,
                // Pre-allocate with reasonable capacity to avoid early reallocations
                files: HashMap::with_capacity(64),
                pattern_cache: pattern_cache_for_actor,
                loop_blocks_cache: LruCache::new(NonZeroUsize::new(256).unwrap()),
                php_assignments_cache: LruCache::new(NonZeroUsize::new(256).unwrap()),
                document_symbols_cache: LruCache::new(NonZeroUsize::new(256).unwrap()),
                livewire_mtimes: HashMap::with_capacity(64),
                livewire_version_counter: 0,
                // Config management
                config_root: None,
                config_files: HashMap::with_capacity(4),
                config_version: 0,
                config_cache: None,
                // Reference finding
                project_files: None,
                project_files_version: 0,
                controller_files: Vec::new(),
                view_files: Vec::new(),
                livewire_files: Vec::new(),
                route_files: Vec::new(),
                vendor_files: Vec::new(),
                source_files: Vec::new(),
                project_root_paths: ProjectRootPaths::default(),
                symbol_index: crate::symbol_index::SymbolIndex::default(),
                class_hierarchy_index: crate::class_hierarchy_index::ClassHierarchyIndex::default(),
                class_files_snapshot: None,
                // Service provider registry
                sp_middleware_aliases: HashMap::new(),
                sp_bindings: HashMap::new(),
                sp_singletons: HashMap::new(),
                sp_view_namespaces: HashMap::new(),
                sp_blade_components: HashMap::new(),
                sp_component_namespaces: HashMap::new(),
                // Environment variables
                env_variables: HashMap::new(),
                // Salsa-based env tracking
                salsa_env_files: HashMap::with_capacity(4),
                salsa_env_version: 0,
                // Salsa-based service provider tracking
                salsa_sp_files: HashMap::with_capacity(32),
                salsa_sp_version: 0,
                salsa_sp_root: None,
            };

            // Pre-warm query cache on actor thread (background)
            // This runs before any file parsing requests arrive,
            // moving the ~200ms compilation cost to startup
            crate::queries::prewarm_query_cache();

            actor.run();
        });

        SalsaHandle {
            sender: tx,
            pattern_cache,
        }
    }

    /// Main event loop - process requests until shutdown
    fn run(&mut self) {
        while let Some(request) = self.receiver.blocking_recv() {
            match request {
                SalsaRequest::UpdateFile {
                    path,
                    version,
                    text,
                    reply,
                } => {
                    self.handle_update_file(path, version, text);
                    let _ = reply.send(());
                }
                SalsaRequest::GetPatterns { path, reply } => {
                    let result = self.handle_get_patterns(&path);
                    let _ = reply.send(result);
                }
                SalsaRequest::GetLoopBlocks { path, reply } => {
                    let result = self.handle_get_loop_blocks(&path);
                    let _ = reply.send(result);
                }
                SalsaRequest::GetPhpAssignments { path, reply } => {
                    let result = self.handle_get_php_assignments(&path);
                    let _ = reply.send(result);
                }
                SalsaRequest::GetDocumentSymbols { path, reply } => {
                    let result = self.handle_get_document_symbols(&path);
                    let _ = reply.send(result);
                }
                SalsaRequest::ResolveLivewireMember {
                    path,
                    member,
                    reply,
                } => {
                    let result = self.handle_resolve_livewire_member(&path, &member);
                    let _ = reply.send(result);
                }
                SalsaRequest::RemoveFile { path, reply } => {
                    self.files.remove(&path);
                    self.pattern_cache.remove(&path);
                    self.loop_blocks_cache.pop(&path);
                    self.php_assignments_cache.pop(&path);
                    self.document_symbols_cache.pop(&path);
                    // Drop from the inverted index too. Doing this
                    // synchronously (rather than via mark_dirty) is
                    // correct: there's no future state to refresh
                    // to — the file is gone.
                    self.symbol_index.remove_file(&path);
                    if self.class_hierarchy_index.contains_file(&path) {
                        self.class_hierarchy_index.remove_file(&path);
                        self.class_files_snapshot = None; // hierarchy changed
                    }
                    let _ = reply.send(());
                }

                SalsaRequest::UpdateProjectFileList { path, op, reply } => {
                    let result = self.handle_update_project_file_list(path, op);
                    let _ = reply.send(result);
                }

                SalsaRequest::BuildSymbolIndex { reply } => {
                    // Full rebuild from current pattern_cache. Cheap on
                    // a freshly-warmed project (~50ms for 60k entries
                    // because we're just iterating a DashMap and
                    // pushing into HashMaps — no parsing). Clear first
                    // so we start from a known state.
                    self.symbol_index.clear();
                    let cache = self.pattern_cache.clone();
                    for entry in cache.iter() {
                        let path = entry.key();
                        let (_, ref patterns) = *entry.value();
                        self.symbol_index.insert_file(path, patterns);
                    }
                    let count = self.symbol_index.entry_count();
                    let _ = reply.send(count);
                }

                // === Config Handlers ===
                SalsaRequest::RegisterConfigFiles {
                    root_path,
                    composer_json,
                    view_config,
                    livewire_config,
                    reply,
                } => {
                    self.handle_register_config_files(
                        root_path,
                        composer_json,
                        view_config,
                        livewire_config,
                    );
                    let _ = reply.send(());
                }
                SalsaRequest::UpdateConfigFile { path, text, reply } => {
                    self.handle_update_config_file(path, text);
                    let _ = reply.send(());
                }
                SalsaRequest::GetLaravelConfig { reply } => {
                    let result = self.handle_get_laravel_config();
                    let _ = reply.send(result);
                }

                // === Reference Finding Handlers ===
                SalsaRequest::RegisterProjectFiles {
                    root_path,
                    controller_paths,
                    view_paths,
                    livewire_path,
                    routes_path,
                    reply,
                } => {
                    self.handle_register_project_files(
                        root_path,
                        controller_paths,
                        view_paths,
                        livewire_path,
                        routes_path,
                    );
                    let _ = reply.send(());
                }
                SalsaRequest::FindViewReferences { view_name, reply } => {
                    let result = self.handle_find_view_references(&view_name);
                    let _ = reply.send(result);
                }
                SalsaRequest::FindReferences {
                    symbol,
                    include_declaration,
                    reply,
                } => {
                    let result = self.handle_find_references(&symbol, include_declaration);
                    let _ = reply.send(result);
                }
                SalsaRequest::CountSymbolReferences { symbol, reply } => {
                    // Direct inverted-index lookup — no dirty-refresh or project
                    // walk (code-lens resolve must stay cheap on large files).
                    let count = self.symbol_index.find(&symbol).len();
                    let _ = reply.send(count);
                }
                SalsaRequest::ListProjectFiles { reply } => {
                    // User code (the whole non-vendor source bucket, which
                    // supersets the categorized controller/view/livewire/route
                    // lists) is chained first so it parses first when the
                    // semaphore frees up; vendor is tailed last. Deduplicated —
                    // the categorized lists overlap `source_files`, and an
                    // absolute view path could fall outside the project root.
                    let mut seen = std::collections::HashSet::new();
                    let paths: Vec<PathBuf> = self
                        .source_files
                        .iter()
                        .chain(self.controller_files.iter())
                        .chain(self.view_files.iter())
                        .chain(self.livewire_files.iter())
                        .chain(self.route_files.iter())
                        .chain(self.vendor_files.iter())
                        .filter(|p| seen.insert((*p).clone()))
                        .cloned()
                        .collect();
                    let _ = reply.send(paths);
                }
                // NOTE: BulkImportPatterns is intentionally kept as a no-op
                // fallback in case any code path still sends it. The real
                // bulk import now writes directly to the shared
                // pattern_cache via SalsaHandle::bulk_import_patterns
                // (which does NOT round-trip through this actor channel).
                // See SalsaActor::pattern_cache for the architectural why.
                SalsaRequest::BulkImportPatterns { entries, reply } => {
                    let total = entries.len();
                    for (path, data) in entries {
                        self.pattern_cache.insert(path, (0, data));
                    }
                    let _ = reply.send(total);
                }
                SalsaRequest::BulkImportHierarchy { entries, reply } => {
                    for (path, nodes) in entries {
                        // remove_file first so a re-warm refreshes cleanly.
                        self.class_hierarchy_index.remove_file(&path);
                        self.class_hierarchy_index.insert_file(&path, nodes);
                    }
                    // Bulk restore always changes the mapping — drop the cache.
                    self.class_files_snapshot = None;
                    let _ = reply.send(self.class_hierarchy_index.class_count());
                }
                SalsaRequest::SnapshotClassFiles { reply } => {
                    // Build once, then hand out cheap `Arc` clones until the
                    // hierarchy's FQCN→file mapping changes (invalidated below).
                    if self.class_files_snapshot.is_none() {
                        let map = self.class_hierarchy_index.fqcn_file_map();
                        self.class_files_snapshot = Some(Arc::new(map));
                    }
                    let snapshot = self.class_files_snapshot.clone().unwrap_or_default();
                    let _ = reply.send(snapshot);
                }
                SalsaRequest::SnapshotHierarchyNodes { reply } => {
                    let _ = reply.send(self.class_hierarchy_index.nodes_by_file());
                }
                SalsaRequest::FileClassSurfaces { path, reply } => {
                    let _ = reply.send(self.class_hierarchy_index.file_surfaces(&path));
                }
                SalsaRequest::ExpandClassDescendants { seeds, reply } => {
                    let _ = reply.send(self.class_hierarchy_index.expand_with_descendants(&seeds));
                }
                SalsaRequest::ExportMagicMembers { reply } => {
                    let _ = reply.send(self.symbol_index.magic_members_by_file());
                }
                SalsaRequest::BulkImportMagicMembers { entries, reply } => {
                    // Append-only: `build_symbol_index` already inserted this
                    // path's literal-symbol keys, and `insert_magic_members`
                    // extends `by_file` rather than overwriting, so the two
                    // coexist and evict together via `remove_file`.
                    let mut count = 0usize;
                    for (path, members) in entries {
                        count += members.len();
                        self.symbol_index.insert_magic_members(&path, &members);
                    }
                    let _ = reply.send(count);
                }
                SalsaRequest::ReindexFileMagic {
                    path,
                    entries,
                    reply,
                } => {
                    // Evict the file's prior keys (literals + magic), then
                    // rebuild: literals from the current pattern cache + the
                    // freshly resolved magic members. `remove_file` clears both
                    // kinds, so re-inserting literals here keeps them alive.
                    self.symbol_index.remove_file(&path);
                    if let Some(cached) = self.pattern_cache.get(&path) {
                        let (_, ref patterns) = *cached;
                        self.symbol_index.insert_file(&path, patterns);
                    }
                    self.symbol_index.insert_magic_members(&path, &entries);
                    let _ = reply.send(());
                }
                SalsaRequest::FindMemberReferences {
                    path,
                    line,
                    column,
                    reply,
                } => {
                    let result = self.handle_find_member_references(&path, line, column);
                    let _ = reply.send(result);
                }
                SalsaRequest::ResolveMagicMemberAt {
                    path,
                    line,
                    column,
                    reply,
                } => {
                    let result = self.handle_resolve_magic_member_at(&path, line, column);
                    let _ = reply.send(result);
                }
                SalsaRequest::ResolveMagicMemberRenameAt {
                    path,
                    line,
                    column,
                    reply,
                } => {
                    let result = self.handle_resolve_magic_member_rename_at(&path, line, column);
                    let _ = reply.send(result);
                }

                // === Service Provider Handlers ===
                SalsaRequest::RegisterServiceProviderRegistry {
                    middleware_aliases,
                    bindings,
                    singletons,
                    reply,
                } => {
                    self.handle_register_service_provider_registry(
                        middleware_aliases,
                        bindings,
                        singletons,
                    );
                    let _ = reply.send(());
                }
                SalsaRequest::GetMiddlewareByAlias { alias, reply } => {
                    let result = self.handle_get_middleware_by_alias(&alias);
                    let _ = reply.send(result);
                }
                SalsaRequest::GetBindingByName { name, reply } => {
                    let result = self.handle_get_binding_by_name(&name);
                    let _ = reply.send(result);
                }
                SalsaRequest::GetViewNamespace { namespace, reply } => {
                    let result = self.handle_get_view_namespace(&namespace);
                    let _ = reply.send(result);
                }
                SalsaRequest::GetAllViewNamespaces { reply } => {
                    let result = self.handle_get_all_view_namespaces();
                    let _ = reply.send(result);
                }
                SalsaRequest::GetBladeComponentReg { tag_name, reply } => {
                    let result = self.handle_get_blade_component_reg(&tag_name);
                    let _ = reply.send(result);
                }
                SalsaRequest::GetAllBladeComponentRegs { reply } => {
                    let result = self.handle_get_all_blade_component_regs();
                    let _ = reply.send(result);
                }
                SalsaRequest::GetComponentNamespace { prefix, reply } => {
                    let result = self.handle_get_component_namespace(&prefix);
                    let _ = reply.send(result);
                }
                SalsaRequest::GetAllComponentNamespaces { reply } => {
                    let result = self.handle_get_all_component_namespaces();
                    let _ = reply.send(result);
                }

                // === Environment Variable Handlers ===
                SalsaRequest::RegisterEnvVariables { variables, reply } => {
                    self.handle_register_env_variables(variables);
                    let _ = reply.send(());
                }
                SalsaRequest::GetEnvVariable { name, reply } => {
                    let result = self.handle_get_env_variable(&name);
                    let _ = reply.send(result);
                }
                SalsaRequest::GetEnvVariableNames { reply } => {
                    let result = self.handle_get_env_variable_names();
                    let _ = reply.send(result);
                }

                // === Salsa-based Environment Variable Handlers (New) ===
                SalsaRequest::RegisterEnvSource {
                    path,
                    text,
                    priority,
                    reply,
                } => {
                    self.handle_register_env_source(path, text, priority);
                    let _ = reply.send(());
                }
                SalsaRequest::GetParsedEnvVar { name, reply } => {
                    let result = self.handle_get_parsed_env_var(&name);
                    let _ = reply.send(result);
                }
                SalsaRequest::GetAllParsedEnvVars { reply } => {
                    let result = self.handle_get_all_parsed_env_vars();
                    let _ = reply.send(result);
                }

                // === Salsa-based Service Provider Handlers (New) ===
                SalsaRequest::RegisterServiceProviderSource {
                    path,
                    text,
                    priority,
                    root_path,
                    reply,
                } => {
                    self.handle_register_service_provider_source(path, text, priority, root_path);
                    let _ = reply.send(());
                }
                SalsaRequest::GetParsedMiddleware { alias, reply } => {
                    let result = self.handle_get_parsed_middleware(&alias);
                    let _ = reply.send(result);
                }
                SalsaRequest::GetAllParsedMiddleware { reply } => {
                    let result = self.handle_get_all_parsed_middleware();
                    let _ = reply.send(result);
                }
                SalsaRequest::GetParsedBinding { name, reply } => {
                    let result = self.handle_get_parsed_binding(&name);
                    let _ = reply.send(result);
                }
                SalsaRequest::GetAllParsedBindings { reply } => {
                    let result = self.handle_get_all_parsed_bindings();
                    let _ = reply.send(result);
                }

                // === Cache-based Registration Handlers ===
                SalsaRequest::RegisterCachedMiddleware {
                    alias,
                    class,
                    class_file,
                    source_file,
                    line,
                    reply,
                } => {
                    self.handle_register_cached_middleware(
                        alias,
                        class,
                        class_file,
                        source_file,
                        line,
                    );
                    let _ = reply.send(());
                }
                SalsaRequest::RegisterCachedBinding {
                    name,
                    class,
                    binding_type,
                    class_file,
                    source_file,
                    line,
                    reply,
                } => {
                    self.handle_register_cached_binding(
                        name,
                        class,
                        binding_type,
                        class_file,
                        source_file,
                        line,
                    );
                    let _ = reply.send(());
                }
                SalsaRequest::RegisterCachedMiddlewareBatch { entries, reply } => {
                    for (alias, class, class_file, source_file, line) in entries {
                        self.handle_register_cached_middleware(
                            alias,
                            class,
                            class_file,
                            source_file,
                            line,
                        );
                    }
                    let _ = reply.send(());
                }
                SalsaRequest::RegisterCachedBindingBatch { entries, reply } => {
                    for (name, class, binding_type, class_file, source_file, line) in entries {
                        self.handle_register_cached_binding(
                            name,
                            class,
                            binding_type,
                            class_file,
                            source_file,
                            line,
                        );
                    }
                    let _ = reply.send(());
                }
                SalsaRequest::RegisterCachedConfig { config, reply } => {
                    // Set config directly from cache, bypassing parsing
                    self.config_root = Some(config.root.clone());
                    self.config_cache = Some((self.config_version, *config));
                    tracing::info!("📋 Registered cached Laravel config");
                    let _ = reply.send(());
                }
                SalsaRequest::RegisterCachedEnvVars { variables, reply } => {
                    // Set env vars directly from cache
                    let count = variables.len();
                    for (name, value) in variables {
                        self.env_variables.insert(
                            name.clone(),
                            EnvVariableData {
                                name,
                                value,
                                file_path: PathBuf::from(".env"), // Placeholder
                                line: 0,
                                column: 0,
                                value_column: 0,
                                is_commented: false,
                            },
                        );
                    }
                    tracing::debug!("Registered {} cached env variables", count);
                    let _ = reply.send(());
                }

                SalsaRequest::Shutdown => {
                    break;
                }
            }
        }
    }

    /// Handle file update - create or update the SourceFile
    fn handle_update_file(&mut self, path: PathBuf, version: i32, text: String) {
        // Invalidate caches for this file - will be recomputed on next request
        self.pattern_cache.remove(&path);
        self.loop_blocks_cache.pop(&path);
        self.php_assignments_cache.pop(&path);
        self.document_symbols_cache.pop(&path);
        // Mark for re-indexing on next find-references query. We don't
        // re-index eagerly here because (a) most file edits are
        // followed by more edits before any query runs, and (b) the
        // new patterns aren't parsed until something asks for them
        // via get_patterns anyway. Lazy refresh amortizes both costs.
        self.symbol_index.mark_dirty(&path);
        self.class_hierarchy_index.mark_dirty(&path);

        if let Some(file) = self.files.get(&path) {
            // Update existing file
            file.set_version(&mut self.db).to(version);
            file.set_text(&mut self.db).to(text);
        } else {
            // Create new file
            let file = SourceFile::new(&self.db, path.clone(), version, text);
            self.files.insert(path, file);
        }
    }

    /// Handle a Blade loop-blocks query. Memoized via Salsa + actor LRU.
    fn handle_get_loop_blocks(
        &mut self,
        path: &PathBuf,
    ) -> Option<Arc<Vec<crate::blade_loops::BladeLoopBlock>>> {
        let file = self.files.get(path)?;
        let version = file.version(&self.db);

        // Cache hit on matching version
        if let Some((cached_version, cached)) = self.loop_blocks_cache.get(path) {
            if *cached_version == version {
                return Some(Arc::clone(cached));
            }
        }

        // Cache miss / stale - call Salsa tracked query (memoized at the Salsa layer too)
        let blocks = parse_blade_loop_blocks(&self.db, *file);
        let arc = Arc::new(blocks);
        self.loop_blocks_cache
            .put(path.clone(), (version, Arc::clone(&arc)));
        Some(arc)
    }

    /// Handle resolving a `$this->X` member access in a Livewire component PHP file.
    /// Auto-registers the file in Salsa, invalidates on mtime change.
    fn handle_resolve_livewire_member(&mut self, path: &PathBuf, member: &str) -> Option<String> {
        let file = self.ensure_livewire_source_loaded(path)?;
        resolve_livewire_member_type(&self.db, file, member.to_string())
    }

    /// Register an external component PHP file as a Salsa input, reloading from
    /// disk whenever its mtime advances. Returns the cached `SourceFile` handle.
    fn ensure_livewire_source_loaded(&mut self, path: &PathBuf) -> Option<SourceFile> {
        let current_mtime = std::fs::metadata(path).ok()?.modified().ok()?;

        let needs_reload = match self.livewire_mtimes.get(path) {
            Some(prev_mtime) => *prev_mtime != current_mtime || !self.files.contains_key(path),
            None => true,
        };

        if needs_reload {
            let text = std::fs::read_to_string(path).ok()?;
            self.livewire_version_counter = self.livewire_version_counter.wrapping_add(1);
            let version = self.livewire_version_counter;

            if let Some(existing) = self.files.get(path) {
                existing.set_version(&mut self.db).to(version);
                existing.set_text(&mut self.db).to(text);
            } else {
                let file = SourceFile::new(&self.db, path.clone(), version, text);
                self.files.insert(path.clone(), file);
            }
            self.livewire_mtimes.insert(path.clone(), current_mtime);
        }

        self.files.get(path).copied()
    }

    /// Handle a Blade @php-assignments query. Memoized via Salsa + actor LRU.
    fn handle_get_php_assignments(&mut self, path: &PathBuf) -> Option<Arc<Vec<(String, String)>>> {
        let file = self.files.get(path)?;
        let version = file.version(&self.db);

        if let Some((cached_version, cached)) = self.php_assignments_cache.get(path) {
            if *cached_version == version {
                return Some(Arc::clone(cached));
            }
        }

        let assignments = parse_blade_php_assignments(&self.db, *file);
        let arc = Arc::new(assignments);
        self.php_assignments_cache
            .put(path.clone(), (version, Arc::clone(&arc)));
        Some(arc)
    }

    /// Handle a document-symbol query. Memoized via Salsa + actor LRU.
    fn handle_get_document_symbols(
        &mut self,
        path: &PathBuf,
    ) -> Option<Arc<Vec<crate::document_symbols::SymbolEntry>>> {
        let file = self.files.get(path)?;
        let version = file.version(&self.db);

        if let Some((cached_version, cached)) = self.document_symbols_cache.get(path) {
            if *cached_version == version {
                return Some(Arc::clone(cached));
            }
        }

        let symbols = extract_document_symbols(&self.db, *file);
        let arc = Arc::new(symbols);
        self.document_symbols_cache
            .put(path.clone(), (version, Arc::clone(&arc)));
        Some(arc)
    }

    /// Handle pattern query - parse file and extract patterns
    /// Uses cached data if version matches, otherwise converts and caches
    /// Returns Arc for efficient sharing without cloning the entire data structure
    fn handle_get_patterns(&mut self, path: &PathBuf) -> Option<Arc<ParsedPatternsData>> {
        let start = Instant::now();
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        // CHECK pattern_cache FIRST — before touching `self.files`. The
        // cache is the fast path for the vast majority of queries:
        // warming + disk cache populate it for every indexed file, and
        // `handle_update_file` removes the entry when content changes.
        // So a cache hit always means the entry is current (no version
        // check needed). This is the lookup that lets us skip
        // `ensure_file_registered` during project-file registration —
        // we don't need a Salsa SourceFile input to serve cached
        // patterns.
        //
        // DashMap::get returns a Ref guard; clone the Arc out and drop
        // the guard so we don't hold a shard lock across the return.
        if let Some(entry) = self.pattern_cache.get(path) {
            let (_, cached_data) = entry.value();
            let data = Arc::clone(cached_data);
            drop(entry);
            debug!("✅ Cache HIT for {} ({:?})", file_name, start.elapsed());
            return Some(data);
        }

        // Cache miss. We need to parse via the Salsa-tracked function,
        // which requires a `SourceFile` input. Lazily create it now if
        // it doesn't exist yet — this is where the file-read cost we
        // skipped at registration time finally lands. The cost is paid
        // once per file, only when something queries it past the cache.
        self.ensure_file_registered(path);

        let file = self.files.get(path)?;
        let version = file.version(&self.db);

        let parse_start = Instant::now();
        let patterns = parse_file_patterns(&self.db, *file);
        let parse_time = parse_start.elapsed();

        // Convert Salsa types to plain data types for transfer
        // Note: Cache intermediate interned values to avoid double lookups
        // Wrap in Rc for cheap cloning when building position index
        let views = patterns
            .views(&self.db)
            .iter()
            .map(|v| {
                let name = v.name(&self.db);
                Arc::new(ViewReferenceData {
                    name: name.name(&self.db).clone(),
                    line: v.line(&self.db),
                    column: v.column(&self.db),
                    end_column: v.end_column(&self.db),
                    is_route_view: v.is_route_view(&self.db),
                })
            })
            .collect();

        let components = patterns
            .components(&self.db)
            .iter()
            .map(|c| {
                let name = c.name(&self.db);
                let tag = c.tag_name(&self.db);
                Arc::new(ComponentReferenceData {
                    name: name.name(&self.db).clone(),
                    tag_name: tag.name(&self.db).clone(),
                    line: c.line(&self.db),
                    column: c.column(&self.db),
                    end_column: c.end_column(&self.db),
                })
            })
            .collect();

        let directives = patterns
            .directives(&self.db)
            .iter()
            .map(|d| {
                let name = d.name(&self.db);
                Arc::new(DirectiveReferenceData {
                    name: name.name(&self.db).clone(),
                    arguments: d.arguments(&self.db).clone(),
                    line: d.line(&self.db),
                    column: d.column(&self.db),
                    end_column: d.end_column(&self.db),
                    string_column: d.string_column(&self.db),
                    string_end_column: d.string_end_column(&self.db),
                })
            })
            .collect();

        let env_refs = patterns
            .env_refs(&self.db)
            .iter()
            .map(|e| {
                let name = e.name(&self.db);
                Arc::new(EnvReferenceData {
                    name: name.name(&self.db).clone(),
                    has_fallback: e.has_fallback(&self.db),
                    line: e.line(&self.db),
                    column: e.column(&self.db),
                    end_column: e.end_column(&self.db),
                })
            })
            .collect();

        let config_refs = patterns
            .config_refs(&self.db)
            .iter()
            .map(|c| {
                let key = c.key(&self.db);
                Arc::new(ConfigReferenceData {
                    key: key.key(&self.db).clone(),
                    line: c.line(&self.db),
                    column: c.column(&self.db),
                    end_column: c.end_column(&self.db),
                })
            })
            .collect();

        let livewire_refs = patterns
            .livewire_refs(&self.db)
            .iter()
            .map(|lw| {
                let name = lw.name(&self.db);
                Arc::new(LivewireReferenceData {
                    name: name.name(&self.db).clone(),
                    line: lw.line(&self.db),
                    column: lw.column(&self.db),
                    end_column: lw.end_column(&self.db),
                })
            })
            .collect();

        let middleware_refs = patterns
            .middleware_refs(&self.db)
            .iter()
            .map(|mw| {
                let name = mw.name(&self.db);
                Arc::new(MiddlewareReferenceData {
                    name: name.name(&self.db).clone(),
                    line: mw.line(&self.db),
                    column: mw.column(&self.db),
                    end_column: mw.end_column(&self.db),
                })
            })
            .collect();

        let translation_refs = patterns
            .translation_refs(&self.db)
            .iter()
            .map(|t| {
                let key = t.key(&self.db);
                Arc::new(TranslationReferenceData {
                    key: key.key(&self.db).clone(),
                    line: t.line(&self.db),
                    column: t.column(&self.db),
                    end_column: t.end_column(&self.db),
                })
            })
            .collect();

        let asset_refs = patterns
            .asset_refs(&self.db)
            .iter()
            .map(|a| {
                let path = a.path(&self.db);
                Arc::new(AssetReferenceData {
                    path: path.path(&self.db).clone(),
                    helper_type: a.helper_type(&self.db),
                    line: a.line(&self.db),
                    column: a.column(&self.db),
                    end_column: a.end_column(&self.db),
                })
            })
            .collect();

        let binding_refs = patterns
            .binding_refs(&self.db)
            .iter()
            .map(|b| {
                let name = b.name(&self.db);
                Arc::new(BindingReferenceData {
                    name: name.name(&self.db).clone(),
                    is_class_reference: b.is_class_reference(&self.db),
                    line: b.line(&self.db),
                    column: b.column(&self.db),
                    end_column: b.end_column(&self.db),
                })
            })
            .collect();

        // Parse route, url, action patterns directly (not cached in Salsa to keep field count under 12)
        // Uses single-pass extraction - query is cached globally so this is fast
        use crate::parser::{language_php, parse_php};
        use crate::queries::extract_all_php_patterns;

        let text = file.text(&self.db);
        let mut route_refs = Vec::new();
        let mut url_refs = Vec::new();
        let mut action_refs = Vec::new();
        let mut feature_refs = Vec::new();
        let mut member_access_refs: Vec<Arc<MemberAccessReferenceData>> = Vec::new();
        let mut chains: Vec<Arc<crate::query_chain::BuilderChain>> = Vec::new();

        // Skip the full-file PHP parse for Blade files — same rationale as
        // in parse_file_patterns above. Blade-embedded route/url/action/
        // feature extraction is handled by the `is_blade` block below.
        let path_is_blade = file
            .path(&self.db)
            .to_string_lossy()
            .ends_with(".blade.php");
        if !path_is_blade {
            if let Ok(tree) = parse_php(text) {
                let lang = language_php();

                if let Ok(php_patterns) = extract_all_php_patterns(&tree, text, &lang) {
                    for r in php_patterns.route_calls {
                        route_refs.push(Arc::new(RouteReferenceData {
                            name: r.route_name.to_string(),
                            line: r.row as u32,
                            column: r.column as u32,
                            end_column: r.end_column as u32,
                        }));
                    }

                    for u in php_patterns.url_calls {
                        url_refs.push(Arc::new(UrlReferenceData {
                            path: u.url_path.to_string(),
                            line: u.row as u32,
                            column: u.column as u32,
                            end_column: u.end_column as u32,
                        }));
                    }

                    for a in php_patterns.action_calls {
                        action_refs.push(Arc::new(ActionReferenceData {
                            action: a.action_name.to_string(),
                            line: a.row as u32,
                            column: a.column as u32,
                            end_column: a.end_column as u32,
                        }));
                    }

                    for f in php_patterns.feature_calls {
                        feature_refs.push(Arc::new(FeatureReferenceData {
                            feature_name: f.feature_name.to_string(),
                            method_name: f.method_name.to_string(),
                            is_class_reference: f.is_class_reference,
                            line: f.row as u32,
                            column: f.column as u32,
                            end_column: f.end_column as u32,
                        }));
                    }

                    // Property-form member-access sites (`$user->email`).
                    // Captured raw here (M2); the receiver-resolution fields
                    // stay at their `None`/`Unresolved` defaults until M3.
                    // Blade-embedded member access is intentionally deferred —
                    // resolving Blade-scope receivers is M3 work.
                    for m in php_patterns.member_accesses {
                        member_access_refs.push(Arc::new(MemberAccessReferenceData {
                            member: m.member.to_string(),
                            receiver: m.receiver.to_string(),
                            receiver_byte_start: m.receiver_byte_start,
                            receiver_byte_end: m.receiver_byte_end,
                            is_nullsafe: m.is_nullsafe,
                            form: m.form,
                            line: m.row as u32,
                            column: m.column as u32,
                            end_column: m.end_column as u32,
                            declaring_fqcn: None,
                            kind: None,
                            confidence: Confidence::Unresolved,
                        }));
                    }
                }

                // Extract Eloquent / DB query builder chains from the same
                // parsed tree. No second parse — we reuse the `tree` already
                // produced above for route/url/action/feature extraction.
                for chain in crate::query_chain::extract_chains(&tree, text) {
                    chains.push(Arc::new(chain));
                }

                // Keep the class-hierarchy index current for this file. The
                // on-demand parse path (did_open / edits / cache misses) is the
                // ONLY populator for files warming skipped because they were
                // already cached — without this, an open/edited model's own
                // class is absent from the hierarchy, so magic-member
                // resolution (`$this->email` → its declaring class) fails.
                let nodes = crate::class_hierarchy_index::classes_from_tree(path, &tree, text);
                // Invalidate the cached class→file snapshot only when this
                // file's set of declared FQCNs actually changed — a method-body
                // edit leaves it intact, keeping the next snapshot O(1).
                let mapping_changed = self.class_hierarchy_index.fqcns_changed(path, &nodes);
                self.class_hierarchy_index.remove_file(path);
                if !nodes.is_empty() {
                    self.class_hierarchy_index.insert_file(path, nodes);
                }
                if mapping_changed {
                    self.class_files_snapshot = None;
                }
            }
        } // end if !path_is_blade

        // Blade-embedded PHP: extract route/url/action/feature from every
        // `{{ }}` / `{!! !!}` / `@php` region. Mirrors the Salsa-cached
        // extraction in parse_file_patterns for the kinds that aren't
        // stored in ParsedPatterns. Without this, route('home') inside a
        // Blade nav menu is invisible to find-references.
        if path_is_blade {
            use crate::blade_embedded_php::{adjust_inner_position, extract_php_regions};
            let lang_php = language_php();
            for region in extract_php_regions(text) {
                let wrapped = format!("<?php {}", region.content);
                let Ok(snippet_tree) = parse_php(&wrapped) else {
                    continue;
                };
                let Ok(snippet_patterns) =
                    extract_all_php_patterns(&snippet_tree, &wrapped, &lang_php)
                else {
                    continue;
                };
                for r in snippet_patterns.route_calls {
                    let (line, col) = adjust_inner_position(
                        r.row as u32,
                        r.column as u32,
                        region.row,
                        region.column,
                    );
                    let (_, end_col) = adjust_inner_position(
                        r.row as u32,
                        r.end_column as u32,
                        region.row,
                        region.column,
                    );
                    route_refs.push(Arc::new(RouteReferenceData {
                        name: r.route_name.to_string(),
                        line,
                        column: col,
                        end_column: end_col,
                    }));
                }
                for u in snippet_patterns.url_calls {
                    let (line, col) = adjust_inner_position(
                        u.row as u32,
                        u.column as u32,
                        region.row,
                        region.column,
                    );
                    let (_, end_col) = adjust_inner_position(
                        u.row as u32,
                        u.end_column as u32,
                        region.row,
                        region.column,
                    );
                    url_refs.push(Arc::new(UrlReferenceData {
                        path: u.url_path.to_string(),
                        line,
                        column: col,
                        end_column: end_col,
                    }));
                }
                for a in snippet_patterns.action_calls {
                    let (line, col) = adjust_inner_position(
                        a.row as u32,
                        a.column as u32,
                        region.row,
                        region.column,
                    );
                    let (_, end_col) = adjust_inner_position(
                        a.row as u32,
                        a.end_column as u32,
                        region.row,
                        region.column,
                    );
                    action_refs.push(Arc::new(ActionReferenceData {
                        action: a.action_name.to_string(),
                        line,
                        column: col,
                        end_column: end_col,
                    }));
                }
                for f in snippet_patterns.feature_calls {
                    let (line, col) = adjust_inner_position(
                        f.row as u32,
                        f.column as u32,
                        region.row,
                        region.column,
                    );
                    let (_, end_col) = adjust_inner_position(
                        f.row as u32,
                        f.end_column as u32,
                        region.row,
                        region.column,
                    );
                    feature_refs.push(Arc::new(FeatureReferenceData {
                        feature_name: f.feature_name.to_string(),
                        method_name: f.method_name.to_string(),
                        is_class_reference: f.is_class_reference,
                        line,
                        column: col,
                        end_column: end_col,
                    }));
                }

                // Property-form member accesses inside this Blade region
                // (`{{ $user->email }}`). Positions are mapped to outer-file
                // coords; byte ranges stay snippet-local (Blade resolution uses
                // the receiver text + view-variable inference, not a whole-file
                // PHP parse).
                for m in snippet_patterns.member_accesses {
                    let (line, col) = adjust_inner_position(
                        m.row as u32,
                        m.column as u32,
                        region.row,
                        region.column,
                    );
                    let (_, end_col) = adjust_inner_position(
                        m.row as u32,
                        m.end_column as u32,
                        region.row,
                        region.column,
                    );
                    member_access_refs.push(Arc::new(MemberAccessReferenceData {
                        member: m.member.to_string(),
                        receiver: m.receiver.to_string(),
                        receiver_byte_start: m.receiver_byte_start,
                        receiver_byte_end: m.receiver_byte_end,
                        is_nullsafe: m.is_nullsafe,
                        form: m.form,
                        line,
                        column: col,
                        end_column: end_col,
                        declaring_fqcn: None,
                        kind: None,
                        confidence: Confidence::Unresolved,
                    }));
                }

                // Eloquent / DB query builder chains inside this Blade-
                // embedded PHP region. Snippet-local byte ranges produced by
                // the extractor reference the `<?php `-wrapped source; shift
                // each range back into outer-file coordinates so the cursor
                // resolver can find them by LSP byte offset.
                use crate::blade_embedded_php::PHP_WRAPPER_PREFIX_LEN;
                for mut chain in crate::query_chain::extract_chains(&snippet_tree, &wrapped) {
                    crate::query_chain::extractor::shift_chain_byte_ranges(
                        &mut chain,
                        region.byte_offset,
                        PHP_WRAPPER_PREFIX_LEN as usize,
                    );
                    chains.push(Arc::new(chain));
                }
            }
        }

        // Capture member accesses inside `@foreach` iterables (`$this->entities`)
        // — directive args the region loop above doesn't reach.
        if path_is_blade {
            for m in blade_loop_iterable_accesses(text) {
                member_access_refs.push(Arc::new(m));
            }
        }

        let mut data = ParsedPatternsData {
            views,
            components,
            directives,
            env_refs,
            config_refs,
            livewire_refs,
            middleware_refs,
            translation_refs,
            asset_refs,
            binding_refs,
            route_refs,
            url_refs,
            action_refs,
            feature_refs,
            chains,
            member_access_refs,
            // Captured here (source in hand) so the magic-build Blade pass
            // routes Volt vs. controller-rendered resolution without re-reading.
            is_volt: path_is_blade
                && crate::livewire_resolver::source_contains_volt_signature(text),
            blade_loops: if path_is_blade {
                blade_loop_vars(text)
            } else {
                Vec::new()
            },
            sorted_positions: Vec::new(),
        };

        // Build the sorted position index for O(log n) lookups
        data.build_position_index();

        // Wrap in Arc for efficient sharing
        let data = Arc::new(data);

        // Cache the Arc for future requests (cheap Arc::clone on cache hit)
        self.pattern_cache
            .insert(path.clone(), (version, Arc::clone(&data)));

        let total_time = start.elapsed();
        debug!(
            "🔄 Cache MISS for {} - parse: {:?}, total: {:?}, middleware_count: {}",
            file_name,
            parse_time,
            total_time,
            data.middleware_refs.len()
        );

        Some(data)
    }

    // === Config Handlers ===

    /// Handle config file registration
    fn handle_register_config_files(
        &mut self,
        root_path: PathBuf,
        composer_json: Option<String>,
        view_config: Option<String>,
        livewire_config: Option<String>,
    ) {
        self.config_root = Some(root_path.clone());
        self.config_version += 1;
        self.config_cache = None; // Invalidate cache

        // Register composer.json
        if let Some(text) = composer_json {
            let path = root_path.join("composer.json");
            let file = ConfigFile::new(&self.db, path.clone(), self.config_version, text);
            self.config_files.insert(path, file);
        }

        // Register config/view.php
        if let Some(text) = view_config {
            let path = root_path.join("config/view.php");
            let file = ConfigFile::new(&self.db, path.clone(), self.config_version, text);
            self.config_files.insert(path, file);
        }

        // Register config/livewire.php
        if let Some(text) = livewire_config {
            let path = root_path.join("config/livewire.php");
            let file = ConfigFile::new(&self.db, path.clone(), self.config_version, text);
            self.config_files.insert(path, file);
        }
    }

    /// Handle config file update
    fn handle_update_config_file(&mut self, path: PathBuf, text: String) {
        self.config_version += 1;
        self.config_cache = None; // Invalidate cache

        if let Some(file) = self.config_files.get(&path) {
            // Update existing file
            file.set_version(&mut self.db).to(self.config_version);
            file.set_text(&mut self.db).to(text);
        } else {
            // Create new file
            let file = ConfigFile::new(&self.db, path.clone(), self.config_version, text);
            self.config_files.insert(path, file);
        }
    }

    /// Handle get Laravel config request
    fn handle_get_laravel_config(&mut self) -> Option<LaravelConfigData> {
        let root = self.config_root.clone()?;

        // Check cache first
        if let Some((cached_version, ref cached_data)) = self.config_cache {
            if cached_version == self.config_version {
                return Some(cached_data.clone());
            }
        }

        // Get config files
        let composer = self.config_files.get(&root.join("composer.json")).copied();
        let view_config = self
            .config_files
            .get(&root.join("config/view.php"))
            .copied();
        let livewire_config = self
            .config_files
            .get(&root.join("config/livewire.php"))
            .copied();

        // Use Salsa query to build config
        let config_ref = build_laravel_config(
            &self.db,
            root.clone(),
            composer,
            view_config,
            livewire_config,
        );

        // Collect view namespaces from all parsed service providers
        let mut view_namespaces: HashMap<String, PathBuf> = HashMap::new();
        let mut component_namespaces: HashMap<String, String> = HashMap::new();
        let mut anonymous_component_paths: HashMap<String, PathBuf> = HashMap::new();
        let mut anonymous_component_namespaces: HashMap<String, String> = HashMap::new();
        // tag → (priority, class file); higher priority (app > package >
        // framework) wins since sp-file iteration order is arbitrary.
        let mut class_component_files: HashMap<String, (u8, PathBuf)> = HashMap::new();

        if let Some(sp_root) = self.salsa_sp_root.as_ref() {
            for sp_file in self.salsa_sp_files.values() {
                let parsed = parse_service_provider_source(&self.db, *sp_file, sp_root.clone());

                // Collect view namespaces
                for vn in parsed.view_namespaces(&self.db) {
                    let ns = vn.namespace(&self.db).namespace(&self.db).clone();
                    if let Some(path) = vn.view_path(&self.db).clone() {
                        // Higher priority wins
                        match view_namespaces.get(&ns) {
                            Some(_) => {} // Keep existing (first wins for now)
                            None => {
                                view_namespaces.insert(ns, path);
                            }
                        }
                    }
                }

                // Collect component namespaces
                for cn in parsed.component_namespaces(&self.db) {
                    let prefix = cn.prefix(&self.db).namespace(&self.db).clone();
                    let php_ns = cn.php_namespace(&self.db).clone();
                    match component_namespaces.get(&prefix) {
                        Some(_) => {} // Keep existing (first wins for now)
                        None => {
                            component_namespaces.insert(prefix, php_ns);
                        }
                    }
                }

                // Collect anonymous component paths (Blade::anonymousComponentPath)
                for acp in parsed.anonymous_component_paths(&self.db) {
                    let prefix = acp.prefix(&self.db).namespace(&self.db).clone();
                    let directory = acp.directory(&self.db).clone();
                    anonymous_component_paths.entry(prefix).or_insert(directory);
                }

                // Collect anonymous component namespaces (Blade::anonymousComponentNamespace)
                for acn in parsed.anonymous_component_namespaces(&self.db) {
                    let prefix = acn.prefix(&self.db).namespace(&self.db).clone();
                    let directory = acn.directory(&self.db).clone();
                    anonymous_component_namespaces
                        .entry(prefix)
                        .or_insert(directory);
                }

                // Collect class-backed component registrations
                // (Blade::component('tag', Class::class), either form/order)
                for bc in parsed.blade_components(&self.db) {
                    let Some(file) = bc.file_path(&self.db).clone() else {
                        continue;
                    };
                    let tag = bc.tag_name(&self.db).name(&self.db).clone();
                    let prio = bc.priority(&self.db);
                    match class_component_files.get(&tag) {
                        Some((existing_prio, _)) if *existing_prio >= prio => {}
                        _ => {
                            class_component_files.insert(tag, (prio, file));
                        }
                    }
                }
            }
        }

        // Also include any from the legacy cache
        for (ns, data) in &self.sp_view_namespaces {
            if let Some(path) = &data.view_path {
                view_namespaces
                    .entry(ns.clone())
                    .or_insert_with(|| path.clone());
            }
        }
        for (prefix, data) in &self.sp_component_namespaces {
            component_namespaces
                .entry(prefix.clone())
                .or_insert_with(|| data.php_namespace.clone());
        }
        for (tag, data) in &self.sp_blade_components {
            if let Some(file) = &data.file_path {
                class_component_files
                    .entry(tag.clone())
                    .or_insert_with(|| (data.priority, file.clone()));
            }
        }

        // Livewire v4 registers an anonymous component path for every entry
        // of config('livewire.component_namespaces') at boot (`layouts` and
        // `pages` by default) — a config-driven loop no provider parse can
        // see. Merged last so explicit Blade::anonymousComponentPath
        // registrations win. Not gated on `has_livewire`: that flag only
        // sees direct composer.json requires, while Livewire commonly
        // arrives transitively (Flux, Filament, MaryUI); the loader
        // self-gates on the config files existing.
        for (ns, dir) in crate::config::livewire_component_namespaces(&root) {
            anonymous_component_paths.entry(ns).or_insert(dir);
        }

        // Convert to data transfer type
        let root = config_ref.root(&self.db).clone();
        let component_aliases = crate::config::load_component_aliases(&root);
        let icon_aliases = crate::config::scan_vendor_for_icon_sets(&root);
        let data = LaravelConfigData {
            root,
            view_paths: config_ref.view_paths(&self.db).clone(),
            component_paths: config_ref.component_paths(&self.db).clone(),
            livewire_path: config_ref.livewire_path(&self.db).clone(),
            has_livewire: config_ref.has_livewire(&self.db),
            view_namespaces,
            component_namespaces,
            anonymous_component_paths,
            anonymous_component_namespaces,
            component_aliases,
            icon_aliases,
            class_component_files: class_component_files
                .into_iter()
                .map(|(tag, (_prio, file))| (tag, file))
                .collect(),
        };

        // Cache the result
        self.config_cache = Some((self.config_version, data.clone()));

        Some(data)
    }

    // === Reference Finding Handlers ===

    /// Add or remove a path in the appropriate per-category file list
    /// based on the classification of its absolute path against the
    /// roots captured at `register_project_files` time. Returns the
    /// category label that was mutated, or `None` if the path didn't
    /// match any project root (silently dropped).
    ///
    /// Idempotency: `Add` is a no-op if the path is already in the
    /// list; `Remove` is a no-op if it isn't. This matters because
    /// LSP filesystem-event delivery isn't deduplicated — an atomic
    /// write can produce two `Created` events for the same final
    /// path, and we shouldn't end up with duplicates in `vendor_files`.
    fn handle_update_project_file_list(
        &mut self,
        path: PathBuf,
        op: FileListOp,
    ) -> Option<&'static str> {
        let category = self.project_root_paths.classify(&path)?;
        let list = match category {
            FileCategory::Controller => &mut self.controller_files,
            FileCategory::View => &mut self.view_files,
            FileCategory::Livewire => &mut self.livewire_files,
            FileCategory::Route => &mut self.route_files,
            FileCategory::Vendor => &mut self.vendor_files,
        };
        match op {
            FileListOp::Add => {
                if !list.contains(&path) {
                    list.push(path);
                }
            }
            FileListOp::Remove => {
                list.retain(|p| p != &path);
            }
        }
        Some(category.label())
    }

    /// Handle project files registration
    /// Scans directories and registers all PHP/Blade files with Salsa
    fn handle_register_project_files(
        &mut self,
        root_path: PathBuf,
        controller_paths: Vec<PathBuf>,
        view_paths: Vec<PathBuf>,
        livewire_path: Option<PathBuf>,
        routes_path: PathBuf,
    ) {
        use walkdir::WalkDir;

        self.project_files_version += 1;

        // Clear existing file lists
        self.controller_files.clear();
        self.view_files.clear();
        self.livewire_files.clear();
        self.route_files.clear();
        self.vendor_files.clear();
        self.source_files.clear();

        // Capture the absolute roots we're about to walk so the file-
        // watcher handler can classify Created/Deleted events back into
        // the right category list without re-walking. View paths are
        // already absolute (config layer resolves them); the others are
        // relative and need joining to `root_path`.
        let vendor_root = root_path.join("vendor");
        self.project_root_paths = ProjectRootPaths {
            controller_roots: controller_paths.iter().map(|p| root_path.join(p)).collect(),
            view_roots: view_paths.clone(),
            livewire_root: livewire_path.clone(),
            routes_root: Some(root_path.join(&routes_path)),
            vendor_root: if vendor_root.is_dir() {
                Some(vendor_root)
            } else {
                None
            },
        };

        // Scan controller directories
        for controller_path in &controller_paths {
            let full_path = root_path.join(controller_path);
            if full_path.exists() {
                for entry in WalkDir::new(&full_path)
                    .into_iter()
                    .filter_entry(|e| e.file_name().to_str().map(|s| s != ".git").unwrap_or(true))
                    .filter_map(|e| e.ok())
                {
                    if entry.file_type().is_file() {
                        if let Some(ext) = entry.path().extension() {
                            if ext == "php" {
                                let path = entry.path().to_path_buf();
                                self.controller_files.push(path);
                                // No ensure_file_registered: deferred to
                                // first cache miss in handle_get_patterns.
                                // See the comment on handle_get_patterns
                                // for the architectural why.
                            }
                        }
                    }
                }
            }
        }

        // Scan view directories (for Blade files)
        for view_path in &view_paths {
            let full_path = root_path.join(view_path);
            if full_path.exists() {
                for entry in WalkDir::new(&full_path)
                    .into_iter()
                    .filter_entry(|e| e.file_name().to_str().map(|s| s != ".git").unwrap_or(true))
                    .filter_map(|e| e.ok())
                {
                    if entry.file_type().is_file() {
                        if let Some(file_name) = entry.path().file_name() {
                            if file_name.to_string_lossy().ends_with(".blade.php") {
                                let path = entry.path().to_path_buf();
                                self.view_files.push(path);
                                // Salsa input deferred to first cache miss.
                            }
                        }
                    }
                }
            }
        }

        // Scan Livewire directory
        if let Some(lw_path) = &livewire_path {
            let full_path = root_path.join(lw_path);
            if full_path.exists() {
                for entry in WalkDir::new(&full_path)
                    .into_iter()
                    .filter_entry(|e| e.file_name().to_str().map(|s| s != ".git").unwrap_or(true))
                    .filter_map(|e| e.ok())
                {
                    if entry.file_type().is_file() {
                        if let Some(ext) = entry.path().extension() {
                            if ext == "php" {
                                let path = entry.path().to_path_buf();
                                self.livewire_files.push(path);
                                // Salsa input deferred to first cache miss.
                            }
                        }
                    }
                }
            }
        }

        // Scan routes directory
        let full_routes_path = root_path.join(&routes_path);
        if full_routes_path.exists() {
            for entry in WalkDir::new(&full_routes_path)
                .into_iter()
                .filter_entry(|e| e.file_name().to_str().map(|s| s != ".git").unwrap_or(true))
                .filter_map(|e| e.ok())
            {
                if entry.file_type().is_file() {
                    if let Some(ext) = entry.path().extension() {
                        if ext == "php" {
                            let path = entry.path().to_path_buf();
                            self.route_files.push(path);
                            // Salsa input deferred to first cache miss.
                        }
                    }
                }
            }
        }

        // Scan vendor/ — Composer packages can declare Livewire,
        // routes, controllers, views, translations. We index every
        // `*.php` and `*.blade.php` under vendor/ and rely on the
        // warming-stage filters (skip `*.json.php` data files, drop
        // anything >256KB) to keep tree-sitter away from pathological
        // auto-generated content.
        //
        // Yes, this reads ~21k file contents on a real-world project,
        // which adds a few seconds to first registration. Subsequent
        // startups load most of those entries from the disk cache
        // (see pattern_disk_cache.rs), so the cost is bounded and
        // one-time per `composer install`.
        let vendor_dir = root_path.join("vendor");
        if vendor_dir.is_dir() {
            for entry in WalkDir::new(&vendor_dir)
                .into_iter()
                .filter_entry(|e| e.file_name().to_str().map(|s| s != ".git").unwrap_or(true))
                .filter_map(|e| e.ok())
            {
                if !entry.file_type().is_file() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy();
                // Match both `*.php` and `*.blade.php` in one pass. The
                // `.blade.php` test must come first because a file
                // ending in `.blade.php` also satisfies `.php`.
                let is_blade = name.ends_with(".blade.php");
                let is_php = !is_blade && name.ends_with(".php");
                if !(is_php || is_blade) {
                    continue;
                }
                let path = entry.path().to_path_buf();
                self.vendor_files.push(path);
                // Salsa input deferred to first cache miss — see
                // handle_get_patterns for the architectural why.
            }
        }

        // Scan the whole project (minus vendor + noise dirs) for every
        // `*.php` / `*.blade.php`. The categorized scans above cover the
        // navigation features; this broad bucket feeds the magic-member reverse
        // index, whose usages can live in any model / service / job / action /
        // Volt page — not just controllers and Blade views.
        self.source_files = collect_source_files(&root_path);

        // Create the ProjectFiles input
        self.project_files = Some(ProjectFiles::new(
            &self.db,
            self.project_files_version,
            self.controller_files.clone(),
            self.view_files.clone(),
            self.livewire_files.clone(),
            self.route_files.clone(),
        ));
    }

    /// Ensure a file is registered with Salsa (read from disk if needed)
    fn ensure_file_registered(&mut self, path: &PathBuf) {
        use std::collections::hash_map::Entry;
        // Use entry API to avoid double lookup
        if let Entry::Vacant(entry) = self.files.entry(path.clone()) {
            if let Ok(text) = std::fs::read_to_string(path) {
                let file = SourceFile::new(&self.db, path.clone(), 0, text);
                entry.insert(file);
            }
        }
    }

    /// Handle find view references request
    fn handle_find_view_references(&mut self, view_name: &str) -> Vec<ViewReferenceLocationData> {
        let mut references = Vec::new();

        // Search controller files
        for path in &self.controller_files.clone() {
            if let Some(patterns) = self.handle_get_patterns(path) {
                for view_ref in &patterns.views {
                    if view_ref.name == view_name {
                        references.push(ViewReferenceLocationData {
                            file_path: path.clone(),
                            line: view_ref.line,
                            character: view_ref.column,
                            reference_type: FileReferenceType::Controller,
                            view_name: view_ref.name.clone(),
                            is_route_view: view_ref.is_route_view,
                        });
                    }
                }
            }
        }

        // Search view files (for @extends, @include directives)
        for path in &self.view_files.clone() {
            if let Some(patterns) = self.handle_get_patterns(path) {
                for directive in &patterns.directives {
                    if directive.name == "extends" || directive.name == "include" {
                        // Extract view name from directive arguments
                        if let Some(ref args) = directive.arguments {
                            let extracted = extract_view_from_args(args);
                            if extracted.as_deref() == Some(view_name) {
                                references.push(ViewReferenceLocationData {
                                    file_path: path.clone(),
                                    line: directive.line,
                                    character: directive.column,
                                    reference_type: FileReferenceType::BladeTemplate,
                                    view_name: view_name.to_string(),
                                    is_route_view: false,
                                });
                            }
                        }
                    }
                }
            }
        }

        // Search Livewire files
        for path in &self.livewire_files.clone() {
            if let Some(patterns) = self.handle_get_patterns(path) {
                for view_ref in &patterns.views {
                    if view_ref.name == view_name {
                        references.push(ViewReferenceLocationData {
                            file_path: path.clone(),
                            line: view_ref.line,
                            character: view_ref.column,
                            reference_type: FileReferenceType::LivewireComponent,
                            view_name: view_ref.name.clone(),
                            is_route_view: view_ref.is_route_view,
                        });
                    }
                }
            }
        }

        // Search route files
        for path in &self.route_files.clone() {
            if let Some(patterns) = self.handle_get_patterns(path) {
                for view_ref in &patterns.views {
                    if view_ref.name == view_name {
                        references.push(ViewReferenceLocationData {
                            file_path: path.clone(),
                            line: view_ref.line,
                            character: view_ref.column,
                            reference_type: FileReferenceType::Route,
                            view_name: view_ref.name.clone(),
                            is_route_view: view_ref.is_route_view,
                        });
                    }
                }
            }
        }

        // Search vendor files — package controllers/views often call
        // `view(...)` against published view namespaces, and find-view-
        // references should surface those alongside user code.
        // FileReferenceType::Controller is used as a catch-all
        // category: there's no `Vendor` variant on the existing enum,
        // and adding one would ripple through every consumer for a
        // cosmetic distinction we don't actually use. The file_path is
        // what matters for navigation.
        for path in &self.vendor_files.clone() {
            if let Some(patterns) = self.handle_get_patterns(path) {
                for view_ref in &patterns.views {
                    if view_ref.name == view_name {
                        references.push(ViewReferenceLocationData {
                            file_path: path.clone(),
                            line: view_ref.line,
                            character: view_ref.column,
                            reference_type: FileReferenceType::Controller,
                            view_name: view_ref.name.clone(),
                            is_route_view: view_ref.is_route_view,
                        });
                    }
                }
            }
        }

        references
    }

    // Cap on the number of dirty files we'll synchronously re-parse
    // inside a single `find_references` call. Past this threshold the
    // actor would block long enough to cause Zed to time out and reset
    // the LSP connection (observed crossing into the tens of seconds at
    // 10k+ dirty entries). When we cross the cap we drop the dirty set
    // on the floor and serve the current index — slightly stale, but
    // alive. Affected files re-index naturally on next save or on the
    // next warming pass.
    //
    // Sized to be comfortably larger than any single bulk import (full
    // vendor parse is ~120 files; a hot edit session typically has tens
    // of dirty files) but small enough that the worst case fits in a
    // tower-lsp request budget.
    const DIRTY_REFRESH_CAP: usize = 1000;

    /// Generic find-references engine. Walks every registered project file and
    /// pulls parser-classified patterns from Salsa for each — matching only
    /// when both the kind and the name agree with `symbol`. The
    /// `include_declaration` flag is honoured for kinds where the parser
    /// distinguishes declaration from usage (currently a no-op since the
    /// parser doesn't tag declarations; reserved for future use).
    ///
    /// Defensive: if the dirty set has more than [`Self::DIRTY_REFRESH_CAP`]
    /// entries (it can blow up to 11k+ on `workspace/didChangeWatchedFiles`
    /// bursts at Zed startup), we skip the per-file re-parse entirely.
    /// Re-parsing thousands of files serially before a single query
    /// freezes the actor long enough that Zed times out the LSP and
    /// resets the connection — a stale-but-live answer beats a dead
    /// server every time.
    /// Resolve the magic member under the cursor and return its indexed usages
    /// (M4). The cursor-side resolution runs here, not in
    /// `classify_pattern_at_cursor`, because it needs the live parse tree plus
    /// the actor-owned class-hierarchy index.
    fn handle_find_member_references(
        &mut self,
        path: &PathBuf,
        line: u32,
        column: u32,
    ) -> Vec<ReferenceLocationData> {
        // Primary: the reverse index is already a position→symbol map. If the
        // click lands on a resolved usage (PHP `$this->status`, Blade
        // `$post->status`, Volt `$this->entities`, …) the index knows which
        // symbol that position belongs to — return its references directly. No
        // receiver re-resolution, and (unlike the fallback below) this works in
        // Blade, where re-parsing the whole template as PHP can't locate nodes.
        let indexed = self.symbol_index.references_at(path, line, column);
        if !indexed.is_empty() {
            return indexed;
        }

        // Fallback: live resolution for usages not yet in the index — e.g. a
        // usage typed since the last save-time magic refresh. Resolves the
        // receiver from the cursor file's own PHP AST.
        let Some(patterns) = self.handle_get_patterns(path) else {
            return Vec::new();
        };
        let member_ref = match patterns.find_at_position(line, column) {
            Some(PatternAtPosition::MemberAccess(m)) => m,
            _ => return Vec::new(),
        };
        let Some(project_root) = self.config_root.clone() else {
            return Vec::new();
        };

        // In-memory source for the cursor file (reflects unsaved edits).
        self.ensure_file_registered(path);
        let Some(file) = self.files.get(path) else {
            return Vec::new();
        };
        let text = file.text(&self.db).clone();

        let Ok(tree) = crate::parser::parse_php(&text) else {
            return Vec::new();
        };
        let bytes = text.as_bytes();
        let aliases = crate::query_chain::use_aliases::extract_use_aliases(&tree, &text);

        // Model magic member: resolve the receiver node to its class and key on
        // that. Needs the receiver node (located by byte range — valid for PHP;
        // Blade-embedded refs may not locate, which is fine — the component
        // fallback below is text-based).
        let mut classviews = crate::member_resolver::ClassViewCache::new();
        if let Some(receiver) = tree
            .root_node()
            .descendant_for_byte_range(member_ref.receiver_byte_start, member_ref.receiver_byte_end)
        {
            if let Some(resolved) = crate::member_resolver::resolve_and_classify(
                receiver,
                &member_ref.member,
                member_ref.form,
                bytes,
                &aliases,
                &self.class_hierarchy_index,
                &mut classviews,
                &project_root,
                None, // query-time path — no dependency recording
            ) {
                // find-references threshold: HIGH + MEDIUM.
                if matches!(resolved.confidence, Confidence::High | Confidence::Medium) {
                    return self.symbol_index.find(&SymbolRefData::MagicMember {
                        fqcn: resolved.declaring_fqcn,
                        member: member_ref.member.clone(),
                    });
                }
            }
        }

        // Component-member fallback: `$this->member` in a Livewire/Volt
        // component. The component is often an anonymous class (no FQCN), so it's
        // keyed under a synthetic per-component id shared across its `.php` and
        // `.blade.php`. Text-based, so it works even when the receiver node above
        // didn't locate (Blade template clicks).
        if member_ref.receiver.trim() == "$this" {
            if let Some(key) = crate::view_var_index::volt_component_key(path, &text) {
                return self.symbol_index.find(&SymbolRefData::MagicMember {
                    fqcn: key,
                    member: member_ref.member.clone(),
                });
            }
        }
        Vec::new()
    }

    /// Resolve + classify the magic member at a position for a hover card (M6).
    /// Mirrors the live-resolution path of `handle_find_member_references`, but
    /// returns the classification (kind + declaring class + a declaration link)
    /// rather than references. Gated to HIGH/MEDIUM confidence — we never guess.
    /// Scoped to Eloquent-model magic members (a resolvable declaring FQCN);
    /// component `$this->` members are out of scope for M6.1.
    fn handle_resolve_magic_member_at(
        &mut self,
        path: &PathBuf,
        line: u32,
        column: u32,
    ) -> Option<MagicMemberHoverData> {
        let patterns = self.handle_get_patterns(path)?;
        let member_ref = match patterns.find_at_position(line, column) {
            Some(PatternAtPosition::MemberAccess(m)) => m,
            _ => return None,
        };
        let project_root = self.config_root.clone()?;

        self.ensure_file_registered(path);
        let file = self.files.get(path)?;
        let text = file.text(&self.db).clone();

        let tree = crate::parser::parse_php(&text).ok()?;
        let bytes = text.as_bytes();
        let aliases = crate::query_chain::use_aliases::extract_use_aliases(&tree, &text);

        let mut classviews = crate::member_resolver::ClassViewCache::new();
        let receiver = tree.root_node().descendant_for_byte_range(
            member_ref.receiver_byte_start,
            member_ref.receiver_byte_end,
        )?;
        // Classify the member; HIGH/MEDIUM only (mirrors find-references). If the
        // member doesn't classify but the receiver still resolves to a model, it
        // may be a plain DB column the source-only ClassView can't see (not in
        // `$casts`) — mark it tentative and let the main side confirm it against
        // migrations/DB.
        let (declaring_fqcn, kind, confidence, tentative) =
            match crate::member_resolver::resolve_and_classify(
                receiver,
                &member_ref.member,
                member_ref.form,
                bytes,
                &aliases,
                &self.class_hierarchy_index,
                &mut classviews,
                &project_root,
                None, // query-time path — no dependency recording
            ) {
                Some(r) if matches!(r.confidence, Confidence::High | Confidence::Medium) => {
                    (r.declaring_fqcn, r.kind, r.confidence, false)
                }
                Some(_) => return None,
                // An unclassified CALL can't be a column — the tentative-column
                // fallback below is a property-read concept.
                None if member_ref.form.is_call() => return None,
                None => {
                    let (fqcn, confidence) = crate::member_resolver::resolve_expression_type(
                        receiver,
                        bytes,
                        &aliases,
                        &self.class_hierarchy_index,
                        &mut classviews,
                        &project_root,
                    )?;
                    if !matches!(confidence, Confidence::High | Confidence::Medium) {
                        return None;
                    }
                    (fqcn, MagicMemberKind::Column, confidence, true)
                }
            };

        // Locate the declaration in the declaring class. A method-backed member
        // (relationship / scope / accessor / finder) yields both start+end lines
        // so the hover can show its source; a property (column / plain) yields
        // just the start line for the link; otherwise fall back to the class's
        // own start line.
        let (decl_file, decl_line, decl_end_line) =
            match self.class_hierarchy_index.get(&declaring_fqcn) {
                Some(node) => {
                    let candidates = crate::hover::candidate_method_names(kind, &member_ref.member);
                    if let Some(m) = node.methods.iter().find(|m| candidates.contains(&m.name)) {
                        (
                            Some(node.file_path.clone()),
                            Some(m.start_line),
                            Some(m.end_line),
                        )
                    } else if let Some(p) =
                        node.properties.iter().find(|p| p.name == member_ref.member)
                    {
                        (Some(node.file_path.clone()), Some(p.start_line), None)
                    } else {
                        (Some(node.file_path.clone()), Some(node.start_line), None)
                    }
                }
                None => (None, None, None),
            };

        Some(MagicMemberHoverData {
            declaring_fqcn,
            member: member_ref.member.clone(),
            kind,
            confidence,
            decl_file,
            decl_line,
            decl_end_line,
            tentative,
        })
    }

    /// Resolve the magic member at a position for rename (M7). Only
    /// method-backed kinds (relationship / scope / accessor / dynamic finder)
    /// qualify — a column/plain member returns `None` (renaming a DB column is a
    /// migration concern). Returns the declaring method name + file so the
    /// caller can rewrite the declaration (transformed) alongside the call
    /// sites. HIGH/MEDIUM confidence only.
    fn handle_resolve_magic_member_rename_at(
        &mut self,
        path: &PathBuf,
        line: u32,
        column: u32,
    ) -> Option<MagicMemberRenameData> {
        let patterns = self.handle_get_patterns(path)?;
        let member_ref = match patterns.find_at_position(line, column) {
            Some(PatternAtPosition::MemberAccess(m)) => m,
            _ => return None,
        };
        let project_root = self.config_root.clone()?;

        self.ensure_file_registered(path);
        let file = self.files.get(path)?;
        let text = file.text(&self.db).clone();

        let tree = crate::parser::parse_php(&text).ok()?;
        let bytes = text.as_bytes();
        let aliases = crate::query_chain::use_aliases::extract_use_aliases(&tree, &text);

        let mut classviews = crate::member_resolver::ClassViewCache::new();
        let receiver = tree.root_node().descendant_for_byte_range(
            member_ref.receiver_byte_start,
            member_ref.receiver_byte_end,
        )?;
        let resolved = crate::member_resolver::resolve_and_classify(
            receiver,
            &member_ref.member,
            member_ref.form,
            bytes,
            &aliases,
            &self.class_hierarchy_index,
            &mut classviews,
            &project_root,
            None, // query-time path — no dependency recording
        )?;
        if !matches!(resolved.confidence, Confidence::High | Confidence::Medium) {
            return None;
        }
        // Only method-backed kinds rename. A column/plain member can't, and a
        // dynamic finder is EXPLICITLY excluded — `whereEmail` has no declared
        // method to rewrite (it's `__call` sugar over the column; renaming the
        // column is the real operation). Relying on the candidate-method
        // lookup below to miss would make finder renameability an accident of
        // `candidate_method_names`' behavior.
        if !matches!(
            resolved.kind,
            MagicMemberKind::Relationship | MagicMemberKind::Scope | MagicMemberKind::Accessor
        ) {
            return None;
        }

        // Find the declaring method (its real name + file) via the kind-aware
        // candidate names — the same mapping the hover uses.
        let node = self.class_hierarchy_index.get(&resolved.declaring_fqcn)?;
        let candidates = crate::hover::candidate_method_names(resolved.kind, &member_ref.member);
        let method = node.methods.iter().find(|m| candidates.contains(&m.name))?;

        Some(MagicMemberRenameData {
            fqcn: resolved.declaring_fqcn,
            member: member_ref.member.clone(),
            kind: resolved.kind,
            method_name: method.name.clone(),
            decl_file: node.file_path.clone(),
        })
    }

    fn handle_find_references(
        &mut self,
        symbol: &SymbolRefData,
        _include_declaration: bool,
    ) -> Vec<ReferenceLocationData> {
        // Refresh any files whose patterns may have drifted since
        // their entries were last indexed (edits via `didChange`,
        // watcher Created/Changed events, etc.). The dirty set is
        // populated by `handle_update_file` and any other mutator;
        // here we drain it and re-index the affected paths exactly
        // once per query.
        //
        // Borrow note: `handle_get_patterns` takes `&mut self`, and
        // we can't hold a `&mut` on `self.symbol_index` across that
        // call. So `take_dirty` clones the paths out FIRST (releasing
        // the borrow), then we iterate them serially.
        // Magic members are never refreshed by this drain — `insert_file`
        // re-adds only *literal* patterns; magic entries come from the separate
        // resolution pass (warm / save). So the (potentially multi-second)
        // re-parse below can't change a magic-member result — skip straight to
        // the O(1) lookup for them.
        if matches!(symbol, SymbolRefData::MagicMember { .. }) {
            return self.symbol_index.find(symbol);
        }

        let start = std::time::Instant::now();
        let dirty = self.symbol_index.take_dirty();
        let dirty_count = dirty.len();
        if dirty_count > Self::DIRTY_REFRESH_CAP {
            // Safety valve. The dirty set has historically blown up to
            // 11k+ entries during a single warm session (likely from
            // bulk `workspace/didChangeWatchedFiles` events on Zed
            // startup), and re-parsing all of them serially before a
            // single find-references query freezes the actor for tens
            // of seconds — long enough that Zed gives up and resets the
            // connection. When we cross this threshold we skip the
            // refresh entirely and serve the cached index as-is. The
            // result may be slightly stale (entries from files that
            // were edited but not yet reflected in the index), but a
            // partially-stale rename UI is dramatically better than
            // a hung server. The dirty paths are dropped (not
            // re-queued), so the staleness is bounded to "until the
            // affected file is re-saved or re-indexed by warming".
            tracing::warn!(
                "⚠️  find_references: dirty set has {} entries (cap {}), \
                 SKIPPING refresh for {:?} — results may be stale. \
                 This typically means a watched-files burst (e.g. Zed \
                 startup) flooded the index; affected files re-index \
                 on next save.",
                dirty_count,
                Self::DIRTY_REFRESH_CAP,
                symbol
            );
            // Intentional: do NOT re-queue. Re-queuing would just hit
            // this branch again on the next query.
        } else if !dirty.is_empty() {
            tracing::debug!(
                "find_references: refreshing {} dirty file(s) before query for {:?}",
                dirty_count,
                symbol
            );
            for path in dirty {
                // Literal-only eviction: re-parsing restores literals via
                // `insert_file`, but magic members are resolved only by the
                // warm/save passes. A full `remove_file` here would drop this
                // file's magic entries with nothing to restore them until the
                // next save — silently zeroing magic-member counts. Preserve
                // them.
                self.symbol_index.remove_literal_entries(&path);
                if let Some(patterns) = self.handle_get_patterns(&path) {
                    self.symbol_index.insert_file(&path, &patterns);
                }
            }
        }
        let refresh_elapsed = start.elapsed();

        // O(1) lookup — the hot path the whole index exists for.
        let find_start = std::time::Instant::now();
        let results = self.symbol_index.find(symbol);
        let find_elapsed = find_start.elapsed();
        tracing::debug!(
            "find_references: {:?} → {} result(s) (refresh {} dirty in {:?}, lookup {:?})",
            symbol,
            results.len(),
            dirty_count,
            refresh_elapsed,
            find_elapsed,
        );
        results
    }

    // === Service Provider Handlers ===

    /// Handle service provider registry registration
    fn handle_register_service_provider_registry(
        &mut self,
        middleware_aliases: HashMap<String, MiddlewareRegistrationData>,
        bindings: HashMap<String, BindingRegistrationData>,
        singletons: HashMap<String, BindingRegistrationData>,
    ) {
        self.sp_middleware_aliases = middleware_aliases;
        self.sp_bindings = bindings;
        self.sp_singletons = singletons;
    }

    /// Handle get middleware by alias
    fn handle_get_middleware_by_alias(&self, alias: &str) -> Option<MiddlewareRegistrationData> {
        self.sp_middleware_aliases
            .get(middleware_base_alias(alias))
            .cloned()
    }

    /// Handle get binding by name
    fn handle_get_binding_by_name(&self, name: &str) -> Option<BindingRegistrationData> {
        // Check bindings first, then singletons
        self.sp_bindings
            .get(name)
            .cloned()
            .or_else(|| self.sp_singletons.get(name).cloned())
    }

    /// Handle get view namespace by name (queries Salsa-parsed service providers)
    fn handle_get_view_namespace(&self, namespace: &str) -> Option<ViewNamespaceData> {
        // First check the legacy cache
        if let Some(data) = self.sp_view_namespaces.get(namespace) {
            return Some(data.clone());
        }

        // Then query Salsa-parsed service providers
        let root = self.salsa_sp_root.as_ref()?;
        let mut best: Option<ViewNamespaceData> = None;

        for sp_file in self.salsa_sp_files.values() {
            let parsed = parse_service_provider_source(&self.db, *sp_file, root.clone());
            for vn in parsed.view_namespaces(&self.db) {
                if vn.namespace(&self.db).namespace(&self.db) == namespace {
                    let data = ViewNamespaceData {
                        namespace: vn.namespace(&self.db).namespace(&self.db).clone(),
                        view_path: vn.view_path(&self.db).clone(),
                        source_file: vn.source_file(&self.db).clone(),
                        source_line: vn.source_line(&self.db),
                        priority: vn.priority(&self.db),
                    };
                    match &best {
                        Some(existing) if existing.priority >= data.priority => {}
                        _ => best = Some(data),
                    }
                }
            }
        }

        best
    }

    /// Handle get all view namespaces
    fn handle_get_all_view_namespaces(&self) -> Vec<ViewNamespaceData> {
        let mut merged: HashMap<String, ViewNamespaceData> = self.sp_view_namespaces.clone();

        if let Some(root) = self.salsa_sp_root.as_ref() {
            for sp_file in self.salsa_sp_files.values() {
                let parsed = parse_service_provider_source(&self.db, *sp_file, root.clone());
                for vn in parsed.view_namespaces(&self.db) {
                    let ns = vn.namespace(&self.db).namespace(&self.db).clone();
                    let data = ViewNamespaceData {
                        namespace: ns.clone(),
                        view_path: vn.view_path(&self.db).clone(),
                        source_file: vn.source_file(&self.db).clone(),
                        source_line: vn.source_line(&self.db),
                        priority: vn.priority(&self.db),
                    };

                    match merged.get(&ns) {
                        Some(existing) if existing.priority >= data.priority => {}
                        _ => {
                            merged.insert(ns, data);
                        }
                    }
                }
            }
        }

        merged.into_values().collect()
    }

    /// Handle get Blade component registration by tag name
    fn handle_get_blade_component_reg(&self, tag_name: &str) -> Option<BladeComponentRegData> {
        // First check the legacy cache
        if let Some(data) = self.sp_blade_components.get(tag_name) {
            return Some(data.clone());
        }

        // Then query Salsa-parsed service providers
        let root = self.salsa_sp_root.as_ref()?;
        let mut best: Option<BladeComponentRegData> = None;

        for sp_file in self.salsa_sp_files.values() {
            let parsed = parse_service_provider_source(&self.db, *sp_file, root.clone());
            for bc in parsed.blade_components(&self.db) {
                if bc.tag_name(&self.db).name(&self.db) == tag_name {
                    let data = BladeComponentRegData {
                        tag_name: bc.tag_name(&self.db).name(&self.db).clone(),
                        class_name: bc.class_name(&self.db).clone(),
                        file_path: bc.file_path(&self.db).clone(),
                        source_file: bc.source_file(&self.db).clone(),
                        source_line: bc.source_line(&self.db),
                        priority: bc.priority(&self.db),
                    };
                    match &best {
                        Some(existing) if existing.priority >= data.priority => {}
                        _ => best = Some(data),
                    }
                }
            }
        }

        best
    }

    /// Handle get all Blade component registrations
    fn handle_get_all_blade_component_regs(&self) -> Vec<BladeComponentRegData> {
        let mut merged: HashMap<String, BladeComponentRegData> = self.sp_blade_components.clone();

        if let Some(root) = self.salsa_sp_root.as_ref() {
            for sp_file in self.salsa_sp_files.values() {
                let parsed = parse_service_provider_source(&self.db, *sp_file, root.clone());
                for bc in parsed.blade_components(&self.db) {
                    let tag = bc.tag_name(&self.db).name(&self.db).clone();
                    let data = BladeComponentRegData {
                        tag_name: tag.clone(),
                        class_name: bc.class_name(&self.db).clone(),
                        file_path: bc.file_path(&self.db).clone(),
                        source_file: bc.source_file(&self.db).clone(),
                        source_line: bc.source_line(&self.db),
                        priority: bc.priority(&self.db),
                    };

                    match merged.get(&tag) {
                        Some(existing) if existing.priority >= data.priority => {}
                        _ => {
                            merged.insert(tag, data);
                        }
                    }
                }
            }
        }

        merged.into_values().collect()
    }

    /// Handle get component namespace by prefix
    fn handle_get_component_namespace(&self, prefix: &str) -> Option<ComponentNamespaceData> {
        // First check the legacy cache
        if let Some(data) = self.sp_component_namespaces.get(prefix) {
            return Some(data.clone());
        }

        // Then query Salsa-parsed service providers
        let root = self.salsa_sp_root.as_ref()?;
        let mut best: Option<ComponentNamespaceData> = None;

        for sp_file in self.salsa_sp_files.values() {
            let parsed = parse_service_provider_source(&self.db, *sp_file, root.clone());
            for cn in parsed.component_namespaces(&self.db) {
                if cn.prefix(&self.db).namespace(&self.db) == prefix {
                    let data = ComponentNamespaceData {
                        prefix: cn.prefix(&self.db).namespace(&self.db).clone(),
                        php_namespace: cn.php_namespace(&self.db).clone(),
                        source_file: cn.source_file(&self.db).clone(),
                        source_line: cn.source_line(&self.db),
                        priority: cn.priority(&self.db),
                    };
                    match &best {
                        Some(existing) if existing.priority >= data.priority => {}
                        _ => best = Some(data),
                    }
                }
            }
        }

        best
    }

    /// Handle get all component namespaces
    fn handle_get_all_component_namespaces(&self) -> Vec<ComponentNamespaceData> {
        let mut merged: HashMap<String, ComponentNamespaceData> =
            self.sp_component_namespaces.clone();

        if let Some(root) = self.salsa_sp_root.as_ref() {
            for sp_file in self.salsa_sp_files.values() {
                let parsed = parse_service_provider_source(&self.db, *sp_file, root.clone());
                for cn in parsed.component_namespaces(&self.db) {
                    let pfx = cn.prefix(&self.db).namespace(&self.db).clone();
                    let data = ComponentNamespaceData {
                        prefix: pfx.clone(),
                        php_namespace: cn.php_namespace(&self.db).clone(),
                        source_file: cn.source_file(&self.db).clone(),
                        source_line: cn.source_line(&self.db),
                        priority: cn.priority(&self.db),
                    };

                    match merged.get(&pfx) {
                        Some(existing) if existing.priority >= data.priority => {}
                        _ => {
                            merged.insert(pfx, data);
                        }
                    }
                }
            }
        }

        merged.into_values().collect()
    }

    // === Environment Variable Handlers ===

    /// Handle env variables registration
    fn handle_register_env_variables(&mut self, variables: HashMap<String, EnvVariableData>) {
        self.env_variables = variables;
    }

    /// Handle get env variable by name
    fn handle_get_env_variable(&self, name: &str) -> Option<EnvVariableData> {
        self.env_variables.get(name).cloned()
    }

    /// Handle get all env variable names
    fn handle_get_env_variable_names(&self) -> Vec<String> {
        self.env_variables.keys().cloned().collect()
    }

    // === Salsa-based Environment Variable Handlers (New) ===

    /// Handle registering a raw env file for Salsa to parse
    fn handle_register_env_source(&mut self, path: PathBuf, text: String, priority: u8) {
        use salsa::Setter;
        self.salsa_env_version += 1;

        if let Some(file) = self.salsa_env_files.get(&path) {
            // Update existing file
            file.set_version(&mut self.db).to(self.salsa_env_version);
            file.set_text(&mut self.db).to(text);
            file.set_priority(&mut self.db).to(priority);
        } else {
            // Create new file
            let file = EnvFile::new(
                &self.db,
                path.clone(),
                self.salsa_env_version,
                text,
                priority,
            );
            self.salsa_env_files.insert(path, file);
        }
    }

    /// Handle getting a parsed env variable by name from Salsa
    fn handle_get_parsed_env_var(&self, name: &str) -> Option<ParsedEnvVarData> {
        // Find the variable with the highest priority
        let mut best: Option<ParsedEnvVarData> = None;

        for env_file in self.salsa_env_files.values() {
            let parsed_vars = parse_env_source(&self.db, *env_file);
            for var in parsed_vars {
                if var.name(&self.db).name(&self.db) == name {
                    let data = ParsedEnvVarData {
                        name: var.name(&self.db).name(&self.db).clone(),
                        value: var.value(&self.db).clone(),
                        line: var.line(&self.db),
                        column: var.column(&self.db),
                        value_column: var.value_column(&self.db),
                        is_commented: var.is_commented(&self.db),
                        priority: var.priority(&self.db),
                        source_file: var.source_file(&self.db).clone(),
                    };
                    // Keep the one with highest priority
                    match &best {
                        Some(existing) if existing.priority >= data.priority => {}
                        _ => best = Some(data),
                    }
                }
            }
        }

        best
    }

    /// Handle getting all parsed env variables from Salsa
    fn handle_get_all_parsed_env_vars(&self) -> Vec<ParsedEnvVarData> {
        use std::collections::HashMap;

        // Merge variables by name, higher priority wins
        let mut merged: HashMap<String, ParsedEnvVarData> = HashMap::new();

        for env_file in self.salsa_env_files.values() {
            let parsed_vars = parse_env_source(&self.db, *env_file);
            for var in parsed_vars {
                let name = var.name(&self.db).name(&self.db).clone();
                let data = ParsedEnvVarData {
                    name: name.clone(),
                    value: var.value(&self.db).clone(),
                    line: var.line(&self.db),
                    column: var.column(&self.db),
                    value_column: var.value_column(&self.db),
                    is_commented: var.is_commented(&self.db),
                    priority: var.priority(&self.db),
                    source_file: var.source_file(&self.db).clone(),
                };

                match merged.get(&name) {
                    Some(existing) if existing.priority >= data.priority => {}
                    _ => {
                        merged.insert(name, data);
                    }
                }
            }
        }

        merged.into_values().collect()
    }

    // === Salsa-based Service Provider Handlers (New) ===

    /// Handle registering a raw service provider file for Salsa to parse
    fn handle_register_service_provider_source(
        &mut self,
        path: PathBuf,
        text: String,
        priority: u8,
        root_path: PathBuf,
    ) {
        use salsa::Setter;
        self.salsa_sp_version += 1;
        self.salsa_sp_root = Some(root_path);

        // The Laravel config's namespace maps (view namespaces, component
        // namespaces, and anonymous-component paths/namespaces) are derived by
        // parsing these service-provider files. Registering or updating one can
        // therefore change the config, so the memoized config_cache must be
        // dropped — otherwise `get_laravel_config` keeps serving the config that
        // was built before this provider was known, and namespaced components
        // resolve as "not found". Bumping salsa_sp_version alone is not enough;
        // config_cache is keyed on config_version, which this doesn't touch.
        self.config_cache = None;

        if let Some(file) = self.salsa_sp_files.get(&path) {
            // Update existing file
            file.set_version(&mut self.db).to(self.salsa_sp_version);
            file.set_text(&mut self.db).to(text);
            file.set_priority(&mut self.db).to(priority);
        } else {
            // Create new file
            let file = ServiceProviderFile::new(
                &self.db,
                path.clone(),
                self.salsa_sp_version,
                text,
                priority,
            );
            self.salsa_sp_files.insert(path, file);
        }
    }

    /// Handle getting middleware by alias from Salsa-parsed service providers
    ///
    /// Strips parameters from the alias before matching — `auth:sanctum` and
    /// `throttle:60,1` resolve to the `auth` and `throttle` aliases respectively.
    fn handle_get_parsed_middleware(&self, alias: &str) -> Option<ParsedMiddlewareData> {
        let base_alias = middleware_base_alias(alias);
        let root = self.salsa_sp_root.as_ref()?;
        let mut best: Option<ParsedMiddlewareData> = None;

        for sp_file in self.salsa_sp_files.values() {
            let parsed = parse_service_provider_source(&self.db, *sp_file, root.clone());
            for mw in parsed.middleware(&self.db) {
                if mw.alias(&self.db).name(&self.db) == base_alias {
                    let data = ParsedMiddlewareData {
                        alias: mw.alias(&self.db).name(&self.db).clone(),
                        class_name: mw.class_name(&self.db).clone(),
                        file_path: mw.file_path(&self.db).clone(),
                        source_line: mw.source_line(&self.db),
                        priority: mw.priority(&self.db),
                        source_file: mw.source_file(&self.db).clone(),
                    };
                    // Keep the one with highest priority
                    match &best {
                        Some(existing) if existing.priority >= data.priority => {}
                        _ => best = Some(data),
                    }
                }
            }
        }

        best
    }

    /// Handle getting all parsed middleware from Salsa
    fn handle_get_all_parsed_middleware(&self) -> Vec<ParsedMiddlewareData> {
        let root = match self.salsa_sp_root.as_ref() {
            Some(r) => r,
            None => return Vec::new(),
        };

        let mut merged: HashMap<String, ParsedMiddlewareData> = HashMap::new();

        for sp_file in self.salsa_sp_files.values() {
            let parsed = parse_service_provider_source(&self.db, *sp_file, root.clone());
            for mw in parsed.middleware(&self.db) {
                let alias = mw.alias(&self.db).name(&self.db).clone();
                let data = ParsedMiddlewareData {
                    alias: alias.clone(),
                    class_name: mw.class_name(&self.db).clone(),
                    file_path: mw.file_path(&self.db).clone(),
                    source_line: mw.source_line(&self.db),
                    priority: mw.priority(&self.db),
                    source_file: mw.source_file(&self.db).clone(),
                };

                match merged.get(&alias) {
                    Some(existing) if existing.priority >= data.priority => {}
                    _ => {
                        merged.insert(alias, data);
                    }
                }
            }
        }

        merged.into_values().collect()
    }

    /// Handle getting a binding by name from Salsa-parsed service providers
    fn handle_get_parsed_binding(&self, name: &str) -> Option<ParsedBindingData> {
        let root = self.salsa_sp_root.as_ref()?;
        let mut best: Option<ParsedBindingData> = None;

        for sp_file in self.salsa_sp_files.values() {
            let parsed = parse_service_provider_source(&self.db, *sp_file, root.clone());
            for binding in parsed.bindings(&self.db) {
                if binding.abstract_name(&self.db).name(&self.db) == name {
                    let data = ParsedBindingData {
                        abstract_name: binding.abstract_name(&self.db).name(&self.db).clone(),
                        concrete_class: binding.concrete_class(&self.db).clone(),
                        file_path: binding.file_path(&self.db).clone(),
                        binding_type: binding.binding_type(&self.db),
                        source_line: binding.source_line(&self.db),
                        priority: binding.priority(&self.db),
                        source_file: binding.source_file(&self.db).clone(),
                    };
                    // Keep the one with highest priority
                    match &best {
                        Some(existing) if existing.priority >= data.priority => {}
                        _ => best = Some(data),
                    }
                }
            }
        }

        best
    }

    /// Handle getting all parsed bindings from Salsa
    fn handle_get_all_parsed_bindings(&self) -> Vec<ParsedBindingData> {
        let root = match self.salsa_sp_root.as_ref() {
            Some(r) => r,
            None => return Vec::new(),
        };

        let mut merged: HashMap<String, ParsedBindingData> = HashMap::new();

        for sp_file in self.salsa_sp_files.values() {
            let parsed = parse_service_provider_source(&self.db, *sp_file, root.clone());
            for binding in parsed.bindings(&self.db) {
                let name = binding.abstract_name(&self.db).name(&self.db).clone();
                let data = ParsedBindingData {
                    abstract_name: name.clone(),
                    concrete_class: binding.concrete_class(&self.db).clone(),
                    file_path: binding.file_path(&self.db).clone(),
                    binding_type: binding.binding_type(&self.db),
                    source_line: binding.source_line(&self.db),
                    priority: binding.priority(&self.db),
                    source_file: binding.source_file(&self.db).clone(),
                };

                match merged.get(&name) {
                    Some(existing) if existing.priority >= data.priority => {}
                    _ => {
                        merged.insert(name, data);
                    }
                }
            }
        }

        merged.into_values().collect()
    }

    /// Handle registering a middleware entry from disk cache
    fn handle_register_cached_middleware(
        &mut self,
        alias: String,
        class: String,
        class_file: Option<String>,
        source_file: Option<String>,
        line: u32,
    ) {
        // Store in the simple registry (same as register_service_provider_registry)
        self.sp_middleware_aliases.insert(
            alias.clone(),
            MiddlewareRegistrationData {
                alias,
                class_name: class,
                file_path: class_file.map(PathBuf::from),
                source_file: source_file.map(PathBuf::from),
                source_line: Some(line as usize),
                priority: 2, // Cache entries have highest priority (app level)
            },
        );
    }

    /// Handle registering a binding entry from disk cache
    fn handle_register_cached_binding(
        &mut self,
        name: String,
        class: String,
        binding_type: String,
        class_file: Option<String>,
        source_file: Option<String>,
        line: u32,
    ) {
        let bt = match binding_type.as_str() {
            "singleton" => BindingTypeData::Singleton,
            "scoped" => BindingTypeData::Scoped,
            "alias" => BindingTypeData::Alias,
            _ => BindingTypeData::Bind,
        };

        // Store in the simple registry
        self.sp_bindings.insert(
            name.clone(),
            BindingRegistrationData {
                abstract_name: name,
                concrete_class: class,
                file_path: class_file.map(PathBuf::from),
                binding_type: bt,
                source_file: source_file.map(PathBuf::from),
                source_line: Some(line as usize),
                priority: 2, // Cache entries have highest priority (app level)
            },
        );
    }
}

/// Extract view name from directive arguments (e.g., "('layouts.app')" -> "layouts.app")
fn extract_view_from_args(args: &str) -> Option<String> {
    let trimmed = args.trim().trim_matches('(').trim_matches(')').trim();
    let unquoted = trimmed.trim_matches('\'').trim_matches('"');
    if !unquoted.is_empty() && !unquoted.contains(',') {
        Some(unquoted.to_string())
    } else {
        None
    }
}

/// Directories never worth walking for project source: dependency trees, VCS
/// metadata, and runtime/cache output. `vendor` is excluded here because it's
/// scanned separately (with its own size/noise filters at warm time).
const SKIP_SCAN_DIRS: &[&str] = &["vendor", "node_modules", ".git", "storage", ".cache"];

/// Collect every non-vendor `*.php` / `*.blade.php` under `root`, skipping
/// dependency and runtime dirs. Feeds the magic-member reverse index, whose
/// usages can live in any source file — not just controllers and Blade views.
/// (`.blade.php` is included because it also ends with `.php`.)
pub fn collect_source_files(root: &Path) -> Vec<PathBuf> {
    use walkdir::WalkDir;
    let mut out = Vec::new();
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| {
            e.file_name()
                .to_str()
                .map(|s| !SKIP_SCAN_DIRS.contains(&s))
                .unwrap_or(true)
        })
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file() && entry.file_name().to_string_lossy().ends_with(".php") {
            out.push(entry.path().to_path_buf());
        }
    }
    out
}

/// Append every parser-classified reference in `patterns` that matches `symbol`
/// into `out`. Only the pattern collection corresponding to the symbol kind is
/// scanned — a `SymbolRef::Route` never matches a coincidental config-key
/// string, etc. This is the "instance chain" enforcement point.
fn collect_matches_for_symbol(
    path: &Path,
    patterns: &ParsedPatternsData,
    symbol: &SymbolRefData,
    out: &mut Vec<ReferenceLocationData>,
) {
    let push = |out: &mut Vec<ReferenceLocationData>, line, column, end_column| {
        out.push(ReferenceLocationData {
            file_path: path.to_path_buf(),
            line,
            column,
            end_column,
        });
    };

    match symbol {
        SymbolRefData::View(name) => {
            for v in &patterns.views {
                if v.name == *name {
                    push(out, v.line, v.column, v.end_column);
                }
            }
            // Blade directives can reference views too (@include, @extends,
            // @component, @each). The parser stores the raw argument string;
            // unwrap to the contained view name before comparing.
            for d in &patterns.directives {
                if matches!(
                    d.name.as_str(),
                    "include" | "extends" | "component" | "each" | "includeIf" | "includeWhen"
                ) {
                    if let Some(args) = d.arguments.as_deref() {
                        if extract_view_from_args(args).as_deref() == Some(name.as_str()) {
                            push(out, d.line, d.string_column, d.string_end_column);
                        }
                    }
                }
            }
        }
        SymbolRefData::Route(name) => {
            for r in &patterns.route_refs {
                if r.name == *name {
                    push(out, r.line, r.column, r.end_column);
                }
            }
        }
        SymbolRefData::Config(key) => {
            for c in &patterns.config_refs {
                if c.key == *key {
                    push(out, c.line, c.column, c.end_column);
                }
            }
        }
        SymbolRefData::Translation(key) => {
            for t in &patterns.translation_refs {
                if t.key == *key {
                    push(out, t.line, t.column, t.end_column);
                }
            }
        }
        SymbolRefData::Env(name) => {
            for e in &patterns.env_refs {
                if e.name == *name {
                    push(out, e.line, e.column, e.end_column);
                }
            }
        }
        SymbolRefData::Component(name) => {
            for c in &patterns.components {
                if c.name == *name {
                    push(out, c.line, c.column, c.end_column);
                }
            }
        }
        SymbolRefData::Livewire(name) => {
            for l in &patterns.livewire_refs {
                if l.name == *name {
                    push(out, l.line, l.column, l.end_column);
                }
            }
        }
        SymbolRefData::Middleware(name) => {
            for m in &patterns.middleware_refs {
                if m.name == *name {
                    push(out, m.line, m.column, m.end_column);
                }
            }
        }
        SymbolRefData::Binding(name) => {
            for b in &patterns.binding_refs {
                if b.name == *name {
                    push(out, b.line, b.column, b.end_column);
                }
            }
        }
        // Magic members can't be matched by raw pattern scanning — a
        // `member_access_ref` only resolves to a `(declaring_fqcn, member)` key
        // through the M3 resolver (which needs the class-hierarchy index). They
        // are served from the resolved inverted index (`insert_magic_members`),
        // so this per-file scanner contributes nothing for them.
        SymbolRefData::MagicMember { .. } => {}
    }
}

#[cfg(test)]
mod tests;
