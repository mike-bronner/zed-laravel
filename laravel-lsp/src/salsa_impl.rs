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
use tracing::info;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
fn extract_translation_from_echo(php_content: &str) -> Option<(String, usize, usize)> {
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
fn parse_vite_directive_assets(
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
            if !path.is_empty() {
                // Calculate column positions for the path content (excluding quotes)
                // directive_col is where @ starts
                // directive_len is length of @vite (5)
                // quote_start is position of opening quote within args (which includes the paren)
                // +1 to skip the opening quote itself and point to the path content
                // +1 more because LSP columns are 0-based but we need to account for the @ symbol position
                let col = (directive_col + directive_len + quote_start + 2) as u32;
                let end_col = col + path.len() as u32; // Just the path, no quotes

                results.push((path, directive_row as u32, col, end_col));
            }
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
                                info!(
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
                info!(
                    "🔍 Processing {} echo PHP snippets",
                    blade_patterns.echo_php.len()
                );
                for echo in blade_patterns.echo_php {
                    info!(
                        "🔍 Echo PHP content: {:?} at row {} col {}",
                        echo.php_content, echo.row, echo.column
                    );
                    if let Some((trans_key, start_offset, end_offset)) =
                        extract_translation_from_echo(echo.php_content)
                    {
                        info!(
                            "✅ Found translation '{}' at offsets {}-{}",
                            trans_key, start_offset, end_offset
                        );
                        let key = TranslationKey::new(db, trans_key.clone());
                        // Calculate column positions relative to the echo statement
                        let col = echo.column + start_offset;
                        let end_col = echo.column + end_offset;
                        info!(
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
                        info!("❌ No translation found in echo content");
                    }
                }
            }
        }
    }

    // Parse PHP (including Blade files for embedded PHP) - single pass extraction
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

        /// Matches Blade::component('tag-name', Class::class)
        static ref BLADE_COMPONENT_RE: Regex = Regex::new(
            r#"Blade::component\s*\(\s*['"]([^'"]+)['"]\s*,\s*\\?([A-Za-z0-9_\\]+)::class\s*\)"#
        ).unwrap();

        /// Matches Blade::componentNamespace('Namespace\\Path', 'prefix')
        static ref COMPONENT_NAMESPACE_RE: Regex = Regex::new(
            r#"Blade::componentNamespace\s*\(\s*['"]([^'"]+)['"]\s*,\s*['"]([^'"]+)['"]\s*\)"#
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
                    alias_def.row as u32,
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
                    group_def.row as u32,
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

    // Parse Blade::component() registrations
    // Example: Blade::component('package-alert', AlertComponent::class)
    for cap in BLADE_COMPONENT_RE.captures_iter(text) {
        if let (Some(tag_name), Some(class)) = (cap.get(1), cap.get(2)) {
            let tag_name_str = tag_name.as_str();
            let class_str = class.as_str().trim_start_matches('\\');

            let line = text[..tag_name.start()].lines().count() as u32;
            let file_path = resolve_class_to_file_internal(class_str, &root);

            let component_name = ComponentName::new(db, tag_name_str.to_string());
            blade_components.push(ParsedBladeComponentReg::new(
                db,
                component_name,
                class_str.to_string(),
                file_path,
                line,
                priority,
                path.clone(),
            ));
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

    ParsedServiceProvider::new(
        db,
        middleware,
        bindings,
        view_namespaces,
        blade_components,
        component_namespaces,
    )
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

/// Resolve a class name to a file path using PSR-4 conventions
fn resolve_class_to_file_internal(class_name: &str, root_path: &Path) -> Option<PathBuf> {
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
#[derive(Debug, Clone)]
pub struct ViewReferenceData {
    pub name: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
    pub is_route_view: bool,
}

/// Component reference data for transfer across async boundaries
#[derive(Debug, Clone)]
pub struct ComponentReferenceData {
    pub name: String,
    pub tag_name: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Directive reference data for transfer across async boundaries
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
pub struct EnvReferenceData {
    pub name: String,
    pub has_fallback: bool,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Config reference data for transfer across async boundaries
#[derive(Debug, Clone)]
pub struct ConfigReferenceData {
    pub key: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Livewire reference data for transfer across async boundaries
#[derive(Debug, Clone)]
pub struct LivewireReferenceData {
    pub name: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Middleware reference data for transfer across async boundaries
#[derive(Debug, Clone)]
pub struct MiddlewareReferenceData {
    pub name: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Translation reference data for transfer across async boundaries
#[derive(Debug, Clone)]
pub struct TranslationReferenceData {
    pub key: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Asset reference data for transfer across async boundaries
#[derive(Debug, Clone)]
pub struct AssetReferenceData {
    pub path: String,
    pub helper_type: AssetHelperType,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Binding reference data for transfer across async boundaries
#[derive(Debug, Clone)]
pub struct BindingReferenceData {
    pub name: String,
    pub is_class_reference: bool,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Route reference data for transfer across async boundaries
#[derive(Debug, Clone)]
pub struct RouteReferenceData {
    pub name: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// URL reference data for transfer across async boundaries
#[derive(Debug, Clone)]
pub struct UrlReferenceData {
    pub path: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Action reference data for transfer across async boundaries
#[derive(Debug, Clone)]
pub struct ActionReferenceData {
    pub action: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Feature reference data for transfer across async boundaries (Laravel Pennant)
#[derive(Debug, Clone)]
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

/// Laravel configuration data for transfer across async boundaries
#[derive(Debug, Clone)]
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
            // Package component - check package view path first
            if let Some(package_view_path) = self.view_namespaces.get(ns) {
                // Anonymous package component: {package_views}/components/{component}.blade.php
                let mut full_path = package_view_path.join("components").join(&component_path);
                full_path.set_extension("blade.php");
                paths.push(full_path);
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
            let mut vendor_path = self
                .root
                .join("resources/views/vendor")
                .join(ns)
                .join("components")
                .join(&component_path);
            vendor_path.set_extension("blade.php");
            paths.push(vendor_path);
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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
}

/// Location of a single parser-classified reference. Generic across pattern
/// kinds — `Backend::references` converts these into LSP `Location`s.
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
struct PositionEntry {
    line: u32,
    column: u32,
    end_column: u32,
    pattern: PatternAtPosition,
}

/// All parsed patterns for a file - plain data for transfer
/// Uses Rc for efficient cloning when building the position index
#[derive(Debug, Clone, Default)]
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
    /// Sorted index of all patterns by (line, column) for O(log n) lookup
    sorted_positions: Vec<PositionEntry>,
}

/// A pattern found at a specific cursor position
/// Uses Rc for cheap cloning (just increments reference count)
#[derive(Debug, Clone)]
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
    /// Return every project file path the actor currently has registered.
    /// Drives the cache-warming task: instead of a single giant sweep that
    /// blocks the actor for seconds, the task pulls the file list once and
    /// fires one `GetPatterns` request per file, so warming interleaves
    /// naturally with real user requests like `find_references`.
    ListProjectFiles {
        reply: oneshot::Sender<Vec<PathBuf>>,
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

    /// Register Laravel config from disk cache (bypasses parsing)
    RegisterCachedConfig {
        config: LaravelConfigData,
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
}

impl SalsaHandle {
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

    /// Return every project file path the actor currently has registered.
    /// The cache-warming task uses this to drive one `get_patterns` request
    /// per file (rather than blocking the actor on a single giant sweep).
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
                config,
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

/// The Salsa actor that owns the database and runs on a dedicated thread
pub struct SalsaActor {
    db: LaravelDatabase,
    receiver: mpsc::Receiver<SalsaRequest>,
    /// Map from path to SourceFile for efficient lookups and updates
    files: HashMap<PathBuf, SourceFile>,
    /// LRU cache of converted pattern data to avoid repeated conversion
    /// Key: file path, Value: (file version, cached patterns wrapped in Arc)
    /// Limited to 256 entries to prevent unbounded memory growth
    pattern_cache: LruCache<PathBuf, (i32, Arc<ParsedPatternsData>)>,
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

        std::thread::spawn(move || {
            let mut actor = SalsaActor {
                db: LaravelDatabase::new(),
                receiver: rx,
                // Pre-allocate with reasonable capacity to avoid early reallocations
                files: HashMap::with_capacity(64),
                // LRU cache with 256 entry limit to prevent unbounded memory growth
                pattern_cache: LruCache::new(NonZeroUsize::new(256).unwrap()),
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

        SalsaHandle { sender: tx }
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
                    self.pattern_cache.pop(&path);
                    self.loop_blocks_cache.pop(&path);
                    self.php_assignments_cache.pop(&path);
                    self.document_symbols_cache.pop(&path);
                    let _ = reply.send(());
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
                SalsaRequest::ListProjectFiles { reply } => {
                    let paths: Vec<PathBuf> = self
                        .controller_files
                        .iter()
                        .chain(self.view_files.iter())
                        .chain(self.livewire_files.iter())
                        .chain(self.route_files.iter())
                        .cloned()
                        .collect();
                    let _ = reply.send(paths);
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
                    self.config_cache = Some((self.config_version, config));
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
        self.pattern_cache.pop(&path);
        self.loop_blocks_cache.pop(&path);
        self.php_assignments_cache.pop(&path);
        self.document_symbols_cache.pop(&path);

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
        let file = self.files.get(path)?;
        let version = file.version(&self.db);
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        // Check cache first - return cached Arc if version matches (cheap clone)
        if let Some((cached_version, cached_data)) = self.pattern_cache.get(path) {
            if *cached_version == version {
                info!("✅ Cache HIT for {} ({:?})", file_name, start.elapsed());
                return Some(Arc::clone(cached_data));
            }
        }

        // Cache miss or version mismatch - need to convert
        // This call is memoized by Salsa - it only re-parses if the file content changed
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
            sorted_positions: Vec::new(),
        };

        // Build the sorted position index for O(log n) lookups
        data.build_position_index();

        // Wrap in Arc for efficient sharing
        let data = Arc::new(data);

        // Cache the Arc for future requests (cheap Arc::clone on cache hit)
        self.pattern_cache
            .put(path.clone(), (version, Arc::clone(&data)));

        let total_time = start.elapsed();
        info!(
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
            component_aliases,
            icon_aliases,
        };

        // Cache the result
        self.config_cache = Some((self.config_version, data.clone()));

        Some(data)
    }

    // === Reference Finding Handlers ===

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
                                self.controller_files.push(path.clone());
                                self.ensure_file_registered(&path);
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
                                self.view_files.push(path.clone());
                                self.ensure_file_registered(&path);
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
                                self.livewire_files.push(path.clone());
                                self.ensure_file_registered(&path);
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
                            self.route_files.push(path.clone());
                            self.ensure_file_registered(&path);
                        }
                    }
                }
            }
        }

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

        references
    }

    /// Generic find-references engine. Walks every registered project file and
    /// pulls parser-classified patterns from Salsa for each — matching only
    /// when both the kind and the name agree with `symbol`. The
    /// `include_declaration` flag is honoured for kinds where the parser
    /// distinguishes declaration from usage (currently a no-op since the
    /// parser doesn't tag declarations; reserved for future use).
    fn handle_find_references(
        &mut self,
        symbol: &SymbolRefData,
        _include_declaration: bool,
    ) -> Vec<ReferenceLocationData> {
        let mut references = Vec::new();

        // Walk every registered file. The filter inside the helper keeps the
        // cost cheap — only the relevant pattern collection on each file's
        // ParsedPatternsData is touched, and we never read non-cached files.
        let mut all_files: Vec<PathBuf> = Vec::new();
        all_files.extend(self.controller_files.iter().cloned());
        all_files.extend(self.view_files.iter().cloned());
        all_files.extend(self.livewire_files.iter().cloned());
        all_files.extend(self.route_files.iter().cloned());

        for path in &all_files {
            let patterns = match self.handle_get_patterns(path) {
                Some(p) => p,
                None => continue,
            };
            collect_matches_for_symbol(path, &patterns, symbol, &mut references);
        }

        references
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
    }
}

#[cfg(test)]
mod tests;
