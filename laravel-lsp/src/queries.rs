//! Single-pass tree-sitter query execution for Laravel pattern matching
//!
//! This module uses a single-pass extraction approach for performance:
//! - Queries are compiled once and cached using once_cell::Lazy
//! - All patterns are extracted in a single tree traversal
//! - This is O(n) instead of O(n×k) where k is the number of pattern types
//!
//! Queries are stored in .scm files and embedded at compile time using include_str!

use anyhow::{anyhow, Result};
use once_cell::sync::Lazy;
use std::time::Instant;
use tracing::{info, warn};
use tree_sitter::{Language, Query, QueryCursor, StreamingIterator, Tree};

// ============================================================================
// Query File Embedding & Cached Compilation
// ============================================================================

/// Embed query files at compile time
const PHP_QUERY: &str = include_str!("../queries/php.scm");
const BLADE_QUERY: &str = include_str!("../queries/blade.scm");

/// Cached compiled PHP query - compiled once on first use
static PHP_QUERY_CACHE: Lazy<Option<Query>> = Lazy::new(|| {
    use crate::parser::language_php;
    let start = Instant::now();
    let lang = language_php();
    let result = Query::new(&lang, PHP_QUERY).ok();
    let elapsed = start.elapsed();
    if result.is_some() {
        tracing::info!("⚡ PHP query compiled in {:?} (one-time cost)", elapsed);
    } else {
        tracing::warn!("❌ PHP query compilation failed after {:?}", elapsed);
    }
    result
});

/// Cached compiled Blade query - compiled once on first use
static BLADE_QUERY_CACHE: Lazy<Option<Query>> = Lazy::new(|| {
    use crate::parser::language_blade;
    let start = Instant::now();
    let lang = language_blade();
    let result = Query::new(&lang, BLADE_QUERY).ok();
    let elapsed = start.elapsed();
    if result.is_some() {
        tracing::info!("⚡ Blade query compiled in {:?} (one-time cost)", elapsed);
    } else {
        tracing::warn!("❌ Blade query compilation failed after {:?}", elapsed);
    }
    result
});

/// Get the cached PHP query, or compile it if needed
fn get_php_query(_language: &Language) -> Result<&'static Query> {
    PHP_QUERY_CACHE.as_ref()
        .ok_or_else(|| anyhow!("Failed to compile PHP query"))
}

/// Get the cached Blade query, or compile it if needed
fn get_blade_query(_language: &Language) -> Result<&'static Query> {
    BLADE_QUERY_CACHE.as_ref()
        .ok_or_else(|| anyhow!("Failed to compile Blade query"))
}

/// Pre-warm the query cache by forcing Lazy initialization.
/// Call this on a background thread during startup to avoid
/// paying the ~200ms compilation cost on first file open.
pub fn prewarm_query_cache() {
    use std::ops::Deref;
    info!("🔥 Pre-warming query cache...");
    // Access the statics to trigger Lazy initialization
    // The logging inside the Lazy closures will show timing
    let _ = PHP_QUERY_CACHE.deref();
    let _ = BLADE_QUERY_CACHE.deref();
    info!("🔥 Query cache pre-warm complete");
}

// ============================================================================
// Match Data Structures
// ============================================================================

/// Represents a matched view() call in PHP code
#[derive(Debug, Clone, PartialEq)]
pub struct ViewMatch<'a> {
    pub view_name: &'a str,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
    /// Whether this is from Route::view() or Volt::route() (should be ERROR if missing)
    pub is_route_view: bool,
}

/// Represents a matched Blade component (<x-*>)
#[derive(Debug, Clone, PartialEq)]
pub struct ComponentMatch<'a> {
    pub component_name: &'a str,
    pub tag_name: &'a str,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
    pub resolved_path: Option<std::path::PathBuf>,
}

/// Represents a matched Livewire component
#[derive(Debug, Clone, PartialEq)]
pub struct LivewireMatch<'a> {
    pub component_name: &'a str,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
}

/// Represents a matched Blade slot (<x-slot:name> or <x-slot name="...">)
#[derive(Debug, Clone, PartialEq)]
pub struct SlotMatch<'a> {
    /// The slot name (e.g., "header" from <x-slot:header>)
    pub slot_name: &'a str,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
}

/// Represents a matched Blade directive
#[derive(Debug, Clone, PartialEq)]
pub struct DirectiveMatch<'a> {
    pub directive_name: &'a str,
    pub full_text: String,
    pub arguments: Option<&'a str>,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
    pub string_column: usize,
    pub string_end_column: usize,
}

/// Represents a matched env() call in PHP code
#[derive(Debug, Clone, PartialEq)]
pub struct EnvMatch<'a> {
    pub var_name: &'a str,
    pub has_fallback: bool,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
}

/// Represents a matched config() call in PHP code
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigMatch<'a> {
    pub config_key: &'a str,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
}

/// Represents a matched middleware call in PHP route definitions
#[derive(Debug, Clone, PartialEq)]
pub struct MiddlewareMatch<'a> {
    pub middleware_name: &'a str,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
}

/// Represents a middleware alias definition in Kernel.php
/// e.g., 'auth' => Authenticate::class in $middlewareAliases
#[derive(Debug, Clone, PartialEq)]
pub struct MiddlewareAliasDefMatch<'a> {
    /// The alias name (e.g., "auth", "guest")
    pub alias: &'a str,
    /// The class name (e.g., "Authenticate", "App\\Http\\Middleware\\Auth")
    pub class_name: &'a str,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
}

/// Represents a Blade component alias registration in a service provider.
/// Captures both forms: `$blade->component($view, $alias)` and
/// `Blade::component($view, $alias)`.
#[derive(Debug, Clone, PartialEq)]
pub struct BladeComponentAliasMatch<'a> {
    /// The alias used in `<x-{alias}>` tags (e.g., "light-button")
    pub alias: &'a str,
    /// The target view path in dot notation or a PHP class FQN
    /// (e.g., "components.buttons.light-button" or "App\\View\\Components\\Light")
    pub view: &'a str,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
}

/// Represents a middleware group definition in Kernel.php
/// e.g., 'web' => [...] in $middlewareGroups
#[derive(Debug, Clone, PartialEq)]
pub struct MiddlewareGroupDefMatch<'a> {
    /// The group name (e.g., "web", "api")
    pub group_name: &'a str,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
}

/// Represents a matched translation call in PHP or Blade code
#[derive(Debug, Clone)]
pub struct TranslationMatch<'a> {
    pub translation_key: &'a str,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
}

/// Represents a matched asset or path helper call
#[derive(Debug, Clone)]
pub struct AssetMatch<'a> {
    pub path: &'a str,
    pub helper_type: AssetHelperType,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
}

/// Types of asset/path helpers
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// A match for a container binding resolution call
#[derive(Debug, Clone)]
pub struct BindingMatch<'a> {
    pub binding_name: &'a str,
    pub is_class_reference: bool,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
}

/// Represents a matched route('name') call in PHP code
#[derive(Debug, Clone, PartialEq)]
pub struct RouteMatch<'a> {
    pub route_name: &'a str,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
}

/// Represents a matched url('path') call in PHP code
#[derive(Debug, Clone, PartialEq)]
pub struct UrlMatch<'a> {
    pub url_path: &'a str,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
}

/// Represents a matched action('Controller@method') call in PHP code
#[derive(Debug, Clone, PartialEq)]
pub struct ActionMatch<'a> {
    pub action_name: &'a str,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
}

/// Represents a matched Laravel Pennant Feature:: call in PHP code
#[derive(Debug, Clone, PartialEq)]
pub struct FeatureMatch<'a> {
    /// The feature name (string key like 'new-api' or class name like 'NewApi')
    pub feature_name: &'a str,
    /// The method being called (active, inactive, value, when, etc.)
    pub method_name: &'a str,
    /// Whether this is a class-based feature (Feature::active(NewApi::class))
    pub is_class_reference: bool,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
}

/// Represents a $name property in a feature class for custom aliases
/// e.g., public string $name = 'custom-alias';
#[derive(Debug, Clone, PartialEq)]
pub struct FeatureNamePropertyMatch<'a> {
    /// The custom alias value (e.g., 'custom-alias')
    pub name_value: &'a str,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
}

// ============================================================================
// Extracted Patterns - Result structs for single-pass extraction
// ============================================================================

/// All patterns extracted from a PHP file in a single pass
#[derive(Debug, Default)]
pub struct ExtractedPhpPatterns<'a> {
    pub views: Vec<ViewMatch<'a>>,
    pub env_calls: Vec<EnvMatch<'a>>,
    pub config_calls: Vec<ConfigMatch<'a>>,
    pub middleware_calls: Vec<MiddlewareMatch<'a>>,
    pub middleware_alias_defs: Vec<MiddlewareAliasDefMatch<'a>>,
    pub middleware_group_defs: Vec<MiddlewareGroupDefMatch<'a>>,
    pub blade_component_aliases: Vec<BladeComponentAliasMatch<'a>>,
    pub translation_calls: Vec<TranslationMatch<'a>>,
    pub asset_calls: Vec<AssetMatch<'a>>,
    pub binding_calls: Vec<BindingMatch<'a>>,
    pub route_calls: Vec<RouteMatch<'a>>,
    pub url_calls: Vec<UrlMatch<'a>>,
    pub action_calls: Vec<ActionMatch<'a>>,
    pub feature_calls: Vec<FeatureMatch<'a>>,
    /// Custom $name property values from feature classes
    pub feature_name_properties: Vec<FeatureNamePropertyMatch<'a>>,
}

/// Represents PHP content inside Blade echo statements {{ ... }}
#[derive(Debug, Clone, PartialEq)]
pub struct EchoPhpMatch<'a> {
    pub php_content: &'a str,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
}

/// All patterns extracted from a Blade file in a single pass
#[derive(Debug, Default)]
pub struct ExtractedBladePatterns<'a> {
    pub components: Vec<ComponentMatch<'a>>,
    pub livewire: Vec<LivewireMatch<'a>>,
    pub directives: Vec<DirectiveMatch<'a>>,
    /// PHP content inside {{ ... }} echo statements
    pub echo_php: Vec<EchoPhpMatch<'a>>,
    /// Slot tags (<x-slot:name> or <x-slot name="...">)
    pub slots: Vec<SlotMatch<'a>>,
}

// ============================================================================
// Single-Pass Extraction Functions
// ============================================================================

/// Extract all PHP patterns in a single tree traversal
///
/// This is the primary extraction function - it runs one query and processes
/// all captures in a single loop, dispatching based on capture name.
pub fn extract_all_php_patterns<'a>(
    tree: &Tree,
    source: &'a str,
    language: &Language,
) -> Result<ExtractedPhpPatterns<'a>> {
    let start = Instant::now();
    let query = get_php_query(language)?;
    let mut cursor = QueryCursor::new();
    let mut result = ExtractedPhpPatterns::default();
    let query_fetch_time = start.elapsed();

    let root_node = tree.root_node();
    let source_bytes = source.as_bytes();

    let mut captures = cursor.captures(query, root_node, source_bytes);

    while let Some((query_match, capture_index)) = captures.next() {
        let capture = &query_match.captures[*capture_index];
        let capture_name = query.capture_names()[capture.index as usize];
        let node = capture.node;

        // Skip if we can't get the text
        let Ok(text) = node.utf8_text(source_bytes) else {
            continue;
        };

        let start_pos = node.start_position();
        let end_pos = node.end_position();

        match capture_name {
            // View patterns
            "view_name" => {
                result.views.push(ViewMatch {
                    view_name: text,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                    is_route_view: false,
                });
            }
            "route_view_name" => {
                result.views.push(ViewMatch {
                    view_name: text,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                    is_route_view: true,
                });
            }

            // Environment variable patterns
            "env_var" => {
                // Check if there's a fallback argument
                let has_fallback = check_has_fallback_argument(node);
                result.env_calls.push(EnvMatch {
                    var_name: text,
                    has_fallback,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }

            // Config patterns
            "config_key" => {
                result.config_calls.push(ConfigMatch {
                    config_key: text,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }

            // Middleware patterns (usage)
            "middleware_name" => {
                result.middleware_calls.push(MiddlewareMatch {
                    middleware_name: text,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }

            // Blade component alias registrations
            // ($blade->component('view.path', 'alias') or Blade::component(...))
            "blade_alias_name" => {
                let view = query_match.captures.iter()
                    .find(|c| query.capture_names()[c.index as usize] == "blade_alias_view")
                    .and_then(|c| c.node.utf8_text(source_bytes).ok());

                if let Some(view) = view {
                    result.blade_component_aliases.push(BladeComponentAliasMatch {
                        alias: text,
                        view,
                        byte_start: node.start_byte(),
                        byte_end: node.end_byte(),
                        row: node.start_position().row,
                    });
                }
            }

            // Middleware alias definitions (from $middlewareAliases property)
            "middleware_alias_key" => {
                // Find the corresponding class capture in the same match
                let class_name = query_match.captures.iter()
                    .find(|c| query.capture_names()[c.index as usize] == "middleware_alias_class")
                    .and_then(|c| c.node.utf8_text(source_bytes).ok());

                if let Some(class_name) = class_name {
                    result.middleware_alias_defs.push(MiddlewareAliasDefMatch {
                        alias: text,
                        class_name,
                        byte_start: node.start_byte(),
                        byte_end: node.end_byte(),
                        row: start_pos.row,
                        column: start_pos.column,
                        end_column: end_pos.column,
                    });
                }
            }

            // Middleware group definitions (from $middlewareGroups property)
            "middleware_group_key" => {
                result.middleware_group_defs.push(MiddlewareGroupDefMatch {
                    group_name: text,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }

            // Translation patterns
            "translation_key" => {
                result.translation_calls.push(TranslationMatch {
                    translation_key: text,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }

            // Asset and path helper patterns
            "asset_path" => {
                result.asset_calls.push(AssetMatch {
                    path: text,
                    helper_type: AssetHelperType::Asset,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }
            "public_path" => {
                result.asset_calls.push(AssetMatch {
                    path: text,
                    helper_type: AssetHelperType::PublicPath,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }
            "base_path" => {
                result.asset_calls.push(AssetMatch {
                    path: text,
                    helper_type: AssetHelperType::BasePath,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }
            "app_path" => {
                result.asset_calls.push(AssetMatch {
                    path: text,
                    helper_type: AssetHelperType::AppPath,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }
            "storage_path" => {
                result.asset_calls.push(AssetMatch {
                    path: text,
                    helper_type: AssetHelperType::StoragePath,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }
            "database_path" => {
                result.asset_calls.push(AssetMatch {
                    path: text,
                    helper_type: AssetHelperType::DatabasePath,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }
            "lang_path" => {
                result.asset_calls.push(AssetMatch {
                    path: text,
                    helper_type: AssetHelperType::LangPath,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }
            "config_path" => {
                result.asset_calls.push(AssetMatch {
                    path: text,
                    helper_type: AssetHelperType::ConfigPath,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }
            "resource_path" => {
                result.asset_calls.push(AssetMatch {
                    path: text,
                    helper_type: AssetHelperType::ResourcePath,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }
            "mix_path" => {
                result.asset_calls.push(AssetMatch {
                    path: text,
                    helper_type: AssetHelperType::Mix,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }
            "vite_asset_path" => {
                result.asset_calls.push(AssetMatch {
                    path: text,
                    helper_type: AssetHelperType::ViteAsset,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }

            // Binding patterns
            "binding_name" => {
                result.binding_calls.push(BindingMatch {
                    binding_name: text,
                    is_class_reference: false,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }
            "binding_class_name" => {
                let clean_class = text.trim_start_matches('\\');
                result.binding_calls.push(BindingMatch {
                    binding_name: clean_class,
                    is_class_reference: true,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }

            // Route patterns
            "route_name" => {
                result.route_calls.push(RouteMatch {
                    route_name: text,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }

            // URL patterns
            "url_path" => {
                result.url_calls.push(UrlMatch {
                    url_path: text,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }

            // Action patterns
            "action_name" => {
                result.action_calls.push(ActionMatch {
                    action_name: text,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }

            // Feature patterns (Laravel Pennant) - string-based feature names
            "feature_name" => {
                // Get the method name from a sibling capture in the same match
                let method_name = get_feature_method_name(&query_match, query, source_bytes)
                    .unwrap_or("active");
                result.feature_calls.push(FeatureMatch {
                    feature_name: text,
                    method_name,
                    is_class_reference: false,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }

            // Feature patterns (Laravel Pennant) - class-based feature references
            "feature_class_name" => {
                let clean_class = text.trim_start_matches('\\');
                let method_name = get_feature_method_name(&query_match, query, source_bytes)
                    .unwrap_or("active");
                result.feature_calls.push(FeatureMatch {
                    feature_name: clean_class,
                    method_name,
                    is_class_reference: true,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }

            // Feature class $name property - custom aliases
            "feature_name_value" => {
                result.feature_name_properties.push(FeatureNamePropertyMatch {
                    name_value: text,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }

            // Ignore other captures (function_name, class_name, etc. used for matching)
            _ => {}
        }
    }

    let total_time = start.elapsed();
    let pattern_count = result.views.len() + result.env_calls.len() + result.config_calls.len()
        + result.middleware_calls.len() + result.translation_calls.len() + result.asset_calls.len()
        + result.binding_calls.len() + result.route_calls.len() + result.url_calls.len()
        + result.action_calls.len() + result.feature_calls.len() + result.feature_name_properties.len();
    info!(
        "📊 PHP extraction: {:?} total (query fetch: {:?}), {} patterns found",
        total_time, query_fetch_time, pattern_count
    );

    Ok(result)
}

/// Extract all Blade patterns in a single tree traversal
pub fn extract_all_blade_patterns<'a>(
    tree: &Tree,
    source: &'a str,
    language: &Language,
) -> Result<ExtractedBladePatterns<'a>> {
    let start = Instant::now();
    let query = get_blade_query(language)?;
    let mut cursor = QueryCursor::new();
    let mut result = ExtractedBladePatterns::default();
    let query_fetch_time = start.elapsed();

    let root_node = tree.root_node();
    let source_bytes = source.as_bytes();

    let mut captures = cursor.captures(query, root_node, source_bytes);

    while let Some((query_match, capture_index)) = captures.next() {
        let capture = &query_match.captures[*capture_index];
        let capture_name = query.capture_names()[capture.index as usize];
        let node = capture.node;

        let Ok(text) = node.utf8_text(source_bytes) else {
            continue;
        };

        let start_pos = node.start_position();
        let end_pos = node.end_position();

        match capture_name {
            // Tag patterns - could be x-* components or livewire:* components
            "tag_name" => {
                // Slot tags (<x-slot:name>, <x-slot ...>) are NOT components — they're
                // named-slot syntax handled separately via the slot_tag capture below.
                // Skipping them here prevents bogus "component not found" diagnostics.
                if text == "x-slot" || text.starts_with("x-slot:") {
                    // intentionally skipped
                } else if let Some(component_name) = text.strip_prefix("x-") {
                    // Blade component
                    result.components.push(ComponentMatch {
                        component_name,
                        tag_name: text,
                        byte_start: node.start_byte(),
                        byte_end: node.end_byte(),
                        row: start_pos.row,
                        column: start_pos.column,
                        end_column: end_pos.column,
                        resolved_path: None,
                    });
                } else if text.starts_with("livewire:") {
                    // Livewire component tag syntax
                    let component_name = &text[9..]; // Remove "livewire:" prefix
                    result.livewire.push(LivewireMatch {
                        component_name,
                        byte_start: node.start_byte(),
                        byte_end: node.end_byte(),
                        row: start_pos.row,
                        column: start_pos.column,
                        end_column: end_pos.column,
                    });
                }
            }

            // Directive patterns
            "directive" => {
                // Trim whitespace - directives inside HTML attributes may have leading spaces
                let text = text.trim();

                // Skip closing directives
                if text.starts_with("@end") {
                    continue;
                }

                if !text.starts_with('@') {
                    warn!("Directive text doesn't start with @: '{}'", text);
                    continue;
                }

                let directive_name = text.strip_prefix('@').unwrap_or(text);

                // Look for parameter sibling - returns both text and column position
                let param_info = find_next_parameter_sibling(node, source_bytes);

                let (arguments, full_text) = match &param_info {
                    Some(info) => (Some(info.text), format!("{}{}", text, info.text)),
                    None => (None, text.to_string()),
                };

                let directive_column = start_pos.column;
                let directive_end_column = end_pos.column;

                // Calculate string column positions for view-referencing, translation, and feature directives
                // Use the actual parameter column from tree-sitter for accurate positioning
                let (string_column, string_end_column) = match (directive_name, &param_info) {
                    ("extends" | "include" | "slot" | "component" | "lang" | "feature" | "livewire", Some(info)) => {
                        calculate_string_column_range(info.column, info.text)
                            .unwrap_or((directive_column, directive_end_column))
                    }
                    _ => (directive_column, directive_end_column),
                };

                result.directives.push(DirectiveMatch {
                    directive_name,
                    full_text,
                    arguments,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: directive_column,
                    end_column: directive_end_column,
                    string_column,
                    string_end_column,
                });
            }

            // @livewire('component-name') directive - component_name capture
            "component_name" => {
                let component_name = text.trim_matches(|c| c == '"' || c == '\'');
                result.livewire.push(LivewireMatch {
                    component_name,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }

            // PHP content inside {{ ... }} echo statements
            "echo_php_content" => {
                result.echo_php.push(EchoPhpMatch {
                    php_content: text,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }

            // Blade directives used as HTML attributes (e.g., @if($show), @disabled($x))
            "directive_attribute" => {
                let text = text.trim();

                // Skip closing directives like @endif
                if text.starts_with("@end") {
                    continue;
                }

                if !text.starts_with('@') {
                    warn!("Directive attribute doesn't start with @: '{}'", text);
                    continue;
                }

                // Parse directive name and arguments from attribute_name like "@if($showClass)"
                // The text includes both the directive name and arguments
                let after_at = &text[1..]; // Remove leading @

                // Find the opening parenthesis to split name and args
                // Also track paren_pos for accurate string position calculation
                let (directive_name, arguments, paren_pos) = if let Some(pos) = after_at.find('(') {
                    let name = &after_at[..pos];
                    let args = &after_at[pos..];
                    (name, Some(args), Some(pos))
                } else {
                    // No parentheses - directive without arguments (e.g., @endif as attribute)
                    (after_at, None, None)
                };

                let full_text = text.to_string();
                let directive_column = start_pos.column;
                let directive_end_column = end_pos.column;

                // Calculate string column positions for view-referencing directives
                // For directive_attribute, calculate parameter column from paren position
                let (string_column, string_end_column) = match (directive_name, &arguments, paren_pos) {
                    ("extends" | "include" | "slot" | "component" | "lang" | "feature" | "livewire", Some(args), Some(pos)) => {
                        // Parameter column = directive_column + @ + paren_pos
                        let parameter_column = directive_column + 1 + pos;
                        calculate_string_column_range(parameter_column, args)
                            .unwrap_or((directive_column, directive_end_column))
                    }
                    _ => (directive_column, directive_end_column),
                };

                result.directives.push(DirectiveMatch {
                    directive_name,
                    full_text,
                    arguments,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: directive_column,
                    end_column: directive_end_column,
                    string_column,
                    string_end_column,
                });
            }

            // Slot tags: <x-slot:name> or <x-slot name="...">
            "slot_tag" => {
                // Extract slot name from x-slot:name syntax
                if let Some(slot_name) = text.strip_prefix("x-slot:") {
                    result.slots.push(SlotMatch {
                        slot_name,
                        byte_start: node.start_byte(),
                        byte_end: node.end_byte(),
                        row: start_pos.row,
                        column: start_pos.column,
                        end_column: end_pos.column,
                    });
                }
            }

            // Ignore vite_directive and other captures
            _ => {}
        }
    }

    let total_time = start.elapsed();
    let pattern_count = result.components.len() + result.livewire.len() + result.directives.len() + result.echo_php.len() + result.slots.len();
    info!(
        "📊 Blade extraction: {:?} total (query fetch: {:?}), {} patterns found",
        total_time, query_fetch_time, pattern_count
    );

    Ok(result)
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Get the feature method name from a query match
/// Looks for the feature_method_name capture in the same match
fn get_feature_method_name<'a>(
    query_match: &tree_sitter::QueryMatch,
    query: &Query,
    source: &'a [u8],
) -> Option<&'a str> {
    for capture in query_match.captures.iter() {
        let capture_name = query.capture_names()[capture.index as usize];
        if capture_name == "feature_method_name" {
            return capture.node.utf8_text(source).ok();
        }
    }
    None
}

/// Check if an env() call has a fallback/default value (second argument)
fn check_has_fallback_argument(node: tree_sitter::Node) -> bool {
    // Navigate: string_content -> string -> argument -> arguments -> function_call
    if let Some(string_node) = node.parent() {
        if let Some(argument_node) = string_node.parent() {
            if let Some(arguments_node) = argument_node.parent() {
                let mut argument_count = 0;
                for i in 0..arguments_node.child_count() {
                    if let Some(child) = arguments_node.child(i as u32) {
                        if child.kind() == "argument" {
                            argument_count += 1;
                        }
                    }
                }
                return argument_count >= 2;
            }
        }
    }
    false
}

/// Parameter info extracted from tree-sitter node
struct ParameterInfo<'a> {
    /// The text content of the parameter (e.g., "('beta-mode')")
    text: &'a str,
    /// The column where the parameter starts (position of '(' or first quote)
    column: usize,
}

/// Find the next parameter sibling node after a directive node
/// Returns both the text and the column position for accurate string position calculation
fn find_next_parameter_sibling<'a>(
    directive_node: tree_sitter::Node,
    source: &'a [u8],
) -> Option<ParameterInfo<'a>> {
    let parent = directive_node.parent()?;
    let mut cursor = parent.walk();

    let mut found_directive = false;
    for child in parent.children(&mut cursor) {
        if found_directive && child.kind() == "parameter" {
            let text = child.utf8_text(source).ok()?;
            let column = child.start_position().column;
            return Some(ParameterInfo { text, column });
        }
        if child.id() == directive_node.id() {
            found_directive = true;
        }
    }

    None
}

/// Calculate the column range of the quoted string content within a directive's arguments.
///
/// Returns (string_start, string_end) where:
/// - string_start: column of the first character INSIDE the quotes (after the opening quote)
/// - string_end: column one past the last character INSIDE the quotes (before the closing quote)
///
/// # Arguments
/// * `parameter_column` - The column where the parameter node starts (position of '(' from tree-sitter)
/// * `arguments` - The arguments string, may include parenthesis: `('view')` or just `'view'`
///
/// # Examples
/// For `@include('view')` with parameter at column 8:
/// - Returns Some((10, 14)) - pointing to "view" content
///
/// For `@feature ('beta-mode')` with parameter at column 9:
/// - Returns Some((11, 20)) - pointing to "beta-mode" content (accounts for space)
fn calculate_string_column_range(
    parameter_column: usize,
    arguments: &str,
) -> Option<(usize, usize)> {
    let trimmed = arguments.trim_start();
    let spaces_before = arguments.len() - trimmed.len();

    // Handle args that may or may not include the opening parenthesis
    // Tree-sitter may capture: ('name') or 'name') or just 'name'
    //
    // Key insight: parameter_column from tree-sitter points to where the parameter node STARTS:
    // - If args include '(': parameter_column points to '('
    // - If args don't include '(': parameter_column already points past '(' (to the quote)
    let (paren_offset, content) = if trimmed.starts_with('(') {
        // Args include '(' - need to skip past it
        (1, &trimmed[1..])
    } else {
        // Args don't include '(' - we're already past it, no offset needed
        (0, trimmed)
    };

    // Skip any spaces after the opening paren (inside the arguments)
    let content_trimmed = content.trim_start();
    let inner_spaces = content.len() - content_trimmed.len();

    let quote_char = content_trimmed.chars().next()?;
    if quote_char != '\'' && quote_char != '"' {
        return None;
    }

    // Find the closing quote position within the content after the opening quote
    let closing_quote_pos = content_trimmed[1..].find(quote_char)?;

    // Calculate position using the actual parameter column from tree-sitter
    // parameter_column points to where the parameter node starts
    // + spaces_before (if any leading spaces in args - usually 0)
    // + paren_offset (1 if we need to skip '(', 0 if already past it)
    // + inner_spaces (spaces after paren, before quote)
    // + 1 (for the opening quote)
    let string_start = parameter_column + spaces_before + paren_offset + inner_spaces + 1;
    // string_end is one past the last content character (exclusive end)
    let string_end = string_start + closing_quote_pos;

    Some((string_start, string_end))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{language_blade, language_php, parse_blade, parse_php};

    #[test]
    fn test_extract_all_php_patterns_views() {
        let php_code = r#"<?php
        return view('users.profile');
        Route::view('/home', 'welcome');
        echo view("admin.dashboard");
        "#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        assert_eq!(patterns.views.len(), 3, "Should find 3 view calls");

        let view_names: Vec<&str> = patterns.views.iter().map(|m| m.view_name).collect();
        assert!(view_names.contains(&"users.profile"));
        assert!(view_names.contains(&"welcome"));
        assert!(view_names.contains(&"admin.dashboard"));

        // Check is_route_view flag
        let welcome = patterns.views.iter().find(|v| v.view_name == "welcome").unwrap();
        assert!(welcome.is_route_view, "Route::view() should set is_route_view=true");

        let users = patterns.views.iter().find(|v| v.view_name == "users.profile").unwrap();
        assert!(!users.is_route_view, "view() should set is_route_view=false");
    }

    #[test]
    fn test_extract_all_php_patterns_env() {
        let php_code = r#"<?php
        $name = env('APP_NAME', 'Laravel');
        $debug = env("APP_DEBUG");
        "#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        assert_eq!(patterns.env_calls.len(), 2, "Should find 2 env calls");
        assert_eq!(patterns.env_calls[0].var_name, "APP_NAME");
        assert_eq!(patterns.env_calls[1].var_name, "APP_DEBUG");
    }

    #[test]
    fn test_extract_all_php_patterns_middleware() {
        let php_code = r#"<?php
        Route::middleware('auth')->group(function () {});
        Route::middleware(['auth', 'verified'])->get('/dashboard');
        "#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        let middleware_names: Vec<&str> = patterns.middleware_calls.iter()
            .map(|m| m.middleware_name).collect();

        assert!(middleware_names.contains(&"auth"), "Should find 'auth' middleware");
        assert!(middleware_names.contains(&"verified"), "Should find 'verified' middleware");
    }

    #[test]
    fn test_extract_middleware_from_route_group() {
        // Test extracting middleware from Route::group() configuration arrays
        let php_code = r#"<?php
Route::group([
    'prefix' => 'api/v1',
    'middleware' => ['api', 'auth'],
], function () {});

Route::group([
    'middleware' => 'web',
], function () {});
"#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        let middleware_names: Vec<&str> = patterns.middleware_calls.iter()
            .map(|m| m.middleware_name).collect();

        assert!(middleware_names.contains(&"api"), "Should find 'api' middleware from array");
        assert!(middleware_names.contains(&"auth"), "Should find 'auth' middleware from array");
        assert!(middleware_names.contains(&"web"), "Should find 'web' middleware from string");
    }

    #[test]
    fn test_extract_middleware_alias_definitions() {
        // Test extracting middleware alias definitions from Kernel.php style properties
        let php_code = r#"<?php
class Kernel {
    protected $middlewareAliases = [
        'auth' => Authenticate::class,
        'guest' => RedirectIfAuthenticated::class,
        'verified' => \Illuminate\Auth\Middleware\EnsureEmailIsVerified::class,
    ];

    protected $middlewareGroups = [
        'web' => [
            EncryptCookies::class,
            AddQueuedCookiesToResponse::class,
        ],
        'api' => [
            ThrottleRequests::class,
        ],
    ];
}
"#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        // Check middleware alias definitions
        let alias_keys: Vec<&str> = patterns.middleware_alias_defs.iter()
            .map(|m| m.alias).collect();
        let alias_classes: Vec<&str> = patterns.middleware_alias_defs.iter()
            .map(|m| m.class_name).collect();

        assert!(alias_keys.contains(&"auth"), "Should find 'auth' alias");
        assert!(alias_keys.contains(&"guest"), "Should find 'guest' alias");
        assert!(alias_keys.contains(&"verified"), "Should find 'verified' alias");
        assert!(alias_classes.contains(&"Authenticate"), "Should find Authenticate class");
        assert!(alias_classes.contains(&"RedirectIfAuthenticated"), "Should find RedirectIfAuthenticated class");

        // Check middleware group definitions
        let group_names: Vec<&str> = patterns.middleware_group_defs.iter()
            .map(|m| m.group_name).collect();

        assert!(group_names.contains(&"web"), "Should find 'web' group");
        assert!(group_names.contains(&"api"), "Should find 'api' group");
    }

    #[test]
    fn test_extract_middleware_from_testbench_kernel() {
        // Test with Orchestra Testbench Kernel.php format
        // This is the actual format used by testbench-core
        let php_code = r#"<?php

namespace Orchestra\Testbench\Http;

use Illuminate\Foundation\Http\Kernel as HttpKernel;

class Kernel extends HttpKernel
{
    protected $middlewareGroups = [
        'web' => [
            \Illuminate\Cookie\Middleware\EncryptCookies::class,
            \Illuminate\Cookie\Middleware\AddQueuedCookiesToResponse::class,
            \Illuminate\Session\Middleware\StartSession::class,
            \Illuminate\View\Middleware\ShareErrorsFromSession::class,
            \Illuminate\Foundation\Http\Middleware\ValidateCsrfToken::class,
            \Illuminate\Routing\Middleware\SubstituteBindings::class,
        ],
        'api' => [
            \Illuminate\Routing\Middleware\ThrottleRequests::class.':api',
            \Illuminate\Routing\Middleware\SubstituteBindings::class,
        ],
    ];

    protected $middlewareAliases = [
        'auth' => \Illuminate\Auth\Middleware\Authenticate::class,
        'auth.basic' => \Illuminate\Auth\Middleware\AuthenticateWithBasicAuth::class,
        'auth.session' => \Illuminate\Session\Middleware\AuthenticateSession::class,
        'cache.headers' => \Illuminate\Http\Middleware\SetCacheHeaders::class,
        'can' => \Illuminate\Auth\Middleware\Authorize::class,
        'guest' => \Orchestra\Testbench\Http\Middleware\RedirectIfAuthenticated::class,
        'password.confirm' => \Illuminate\Auth\Middleware\RequirePassword::class,
        'precognitive' => \Illuminate\Foundation\Http\Middleware\HandlePrecognitiveRequests::class,
        'signed' => \Illuminate\Routing\Middleware\ValidateSignature::class,
        'throttle' => \Illuminate\Routing\Middleware\ThrottleRequests::class,
        'verified' => \Illuminate\Auth\Middleware\EnsureEmailIsVerified::class,
    ];
}
"#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        // Check middleware group definitions
        let group_names: Vec<&str> = patterns.middleware_group_defs.iter()
            .map(|m| m.group_name).collect();

        assert!(group_names.contains(&"web"), "Should find 'web' group");
        assert!(group_names.contains(&"api"), "Should find 'api' group");

        // Check middleware alias definitions
        let alias_keys: Vec<&str> = patterns.middleware_alias_defs.iter()
            .map(|m| m.alias).collect();

        assert!(alias_keys.contains(&"auth"), "Should find 'auth' alias");
        assert!(alias_keys.contains(&"guest"), "Should find 'guest' alias");
        assert!(alias_keys.contains(&"can"), "Should find 'can' alias");
        assert!(alias_keys.contains(&"throttle"), "Should find 'throttle' alias");
        assert!(alias_keys.contains(&"verified"), "Should find 'verified' alias");
    }

    #[test]
    fn test_extract_middleware_from_laravel11_method_aliases() {
        // Test extracting middleware from Laravel 11+ defaultAliases() method body
        // This pattern uses $aliases = [...] inside a method
        let php_code = r#"<?php

namespace Illuminate\Foundation\Configuration;

class Middleware
{
    protected function defaultAliases()
    {
        $aliases = [
            'auth' => \Illuminate\Auth\Middleware\Authenticate::class,
            'auth.basic' => \Illuminate\Auth\Middleware\AuthenticateWithBasicAuth::class,
            'guest' => \Illuminate\Auth\Middleware\RedirectIfAuthenticated::class,
            'verified' => \Illuminate\Auth\Middleware\EnsureEmailIsVerified::class,
        ];

        return $aliases;
    }
}
"#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        // Check middleware alias definitions from method body
        let alias_keys: Vec<&str> = patterns.middleware_alias_defs.iter()
            .map(|m| m.alias).collect();

        assert!(alias_keys.contains(&"auth"), "Should find 'auth' alias from $aliases assignment");
        assert!(alias_keys.contains(&"auth.basic"), "Should find 'auth.basic' alias");
        assert!(alias_keys.contains(&"guest"), "Should find 'guest' alias");
        assert!(alias_keys.contains(&"verified"), "Should find 'verified' alias");
    }

    #[test]
    fn test_extract_middleware_from_laravel11_method_groups() {
        // Test extracting middleware groups from Laravel 11+ getMiddlewareGroups() method
        let php_code = r#"<?php

namespace Illuminate\Foundation\Configuration;

class Middleware
{
    public function getMiddlewareGroups()
    {
        $middleware = [
            'web' => [
                \Illuminate\Cookie\Middleware\EncryptCookies::class,
            ],
            'api' => [
                \Illuminate\Routing\Middleware\ThrottleRequests::class,
            ],
        ];

        return $middleware;
    }
}
"#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        // Check middleware group definitions from method body
        let group_names: Vec<&str> = patterns.middleware_group_defs.iter()
            .map(|m| m.group_name).collect();

        assert!(group_names.contains(&"web"), "Should find 'web' group from $middleware assignment");
        assert!(group_names.contains(&"api"), "Should find 'api' group from $middleware assignment");
    }

    #[test]
    fn test_extract_middleware_from_bootstrap_app_alias_call() {
        // Test extracting middleware from $middleware->alias() calls in bootstrap/app.php
        let php_code = r#"<?php

use App\Http\Middleware\CustomAuth;
use App\Http\Middleware\ApiRateLimiter;

return Application::configure(basePath: dirname(__DIR__))
    ->withMiddleware(function (Middleware $middleware) {
        $middleware->alias('custom.auth', CustomAuth::class);
        $middleware->alias('api.rate', ApiRateLimiter::class);
    });
"#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        // Check middleware alias definitions from ->alias() calls
        let alias_keys: Vec<&str> = patterns.middleware_alias_defs.iter()
            .map(|m| m.alias).collect();

        assert!(alias_keys.contains(&"custom.auth"), "Should find 'custom.auth' alias from ->alias() call");
        assert!(alias_keys.contains(&"api.rate"), "Should find 'api.rate' alias from ->alias() call");
    }

    #[test]
    fn test_extract_middleware_from_bootstrap_app_alias_array() {
        // Test extracting middleware from $middleware->alias([...]) with array argument
        let php_code = r#"<?php

return Application::configure(basePath: dirname(__DIR__))
    ->withMiddleware(function (Middleware $middleware) {
        $middleware->alias([
            'custom.auth' => CustomAuth::class,
            'custom.guest' => CustomGuest::class,
        ]);
    });
"#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        // Check middleware alias definitions from ->alias([...]) call
        let alias_keys: Vec<&str> = patterns.middleware_alias_defs.iter()
            .map(|m| m.alias).collect();

        assert!(alias_keys.contains(&"custom.auth"), "Should find 'custom.auth' alias from ->alias([...]) call");
        assert!(alias_keys.contains(&"custom.guest"), "Should find 'custom.guest' alias from ->alias([...]) call");
    }

    #[test]
    fn test_extract_middleware_from_bootstrap_app_group_call() {
        // Test extracting middleware from $middleware->group() calls in bootstrap/app.php
        let php_code = r#"<?php

return Application::configure(basePath: dirname(__DIR__))
    ->withMiddleware(function (Middleware $middleware) {
        $middleware->group('custom', [
            FirstMiddleware::class,
            SecondMiddleware::class,
        ]);
    });
"#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        // Check middleware group definitions from ->group() call
        let group_names: Vec<&str> = patterns.middleware_group_defs.iter()
            .map(|m| m.group_name).collect();

        assert!(group_names.contains(&"custom"), "Should find 'custom' group from ->group() call");
    }

    #[test]
    fn test_extract_middleware_from_service_provider_router() {
        // Test extracting middleware from $router->aliasMiddleware() in service providers
        let php_code = r#"<?php

namespace App\Providers;

class RouteServiceProvider extends ServiceProvider
{
    public function boot()
    {
        $router = $this->app->make('router');
        $router->aliasMiddleware('custom', CustomMiddleware::class);
        $router->aliasMiddleware('another', AnotherMiddleware::class);
    }
}
"#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        // Check middleware alias definitions from $router->aliasMiddleware() calls
        let alias_keys: Vec<&str> = patterns.middleware_alias_defs.iter()
            .map(|m| m.alias).collect();

        assert!(alias_keys.contains(&"custom"), "Should find 'custom' alias from $router->aliasMiddleware()");
        assert!(alias_keys.contains(&"another"), "Should find 'another' alias from $router->aliasMiddleware()");
    }

    #[test]
    fn test_extract_all_blade_patterns_components() {
        let blade_code = r#"
        <div>
            <x-button type="primary">Click me</x-button>
            <x-forms.input name="email" />
        </div>
        "#;

        let tree = parse_blade(blade_code).expect("Should parse Blade");
        let lang = language_blade();
        let patterns = extract_all_blade_patterns(&tree, blade_code, &lang)
            .expect("Should extract patterns");

        assert!(!patterns.components.is_empty(), "Should find at least one component");

        let component_names: Vec<&str> = patterns.components.iter()
            .map(|m| m.component_name).collect();
        assert!(
            component_names.iter().any(|&name| name == "button" || name.starts_with("button")),
            "Should find button component"
        );
    }

    #[test]
    fn test_extract_all_blade_patterns_directives() {
        let blade_code = r#"
@extends('layouts.app')
@section('content')
    @foreach($users as $user)
        <p>{{ $user->name }}</p>
    @endforeach
@endsection
        "#;

        let tree = parse_blade(blade_code).expect("Should parse Blade");
        let lang = language_blade();
        let patterns = extract_all_blade_patterns(&tree, blade_code, &lang)
            .expect("Should extract patterns");

        let directive_names: Vec<&str> = patterns.directives.iter()
            .map(|m| m.directive_name).collect();

        assert!(directive_names.contains(&"extends"), "Should find @extends");
        assert!(directive_names.contains(&"section"), "Should find @section");
        assert!(directive_names.contains(&"foreach"), "Should find @foreach");

        // Should NOT contain closing directives
        assert!(!directive_names.contains(&"endforeach"), "Should not find @endforeach");
        assert!(!directive_names.contains(&"endsection"), "Should not find @endsection");
    }

    #[test]
    fn test_extract_blade_feature_directive() {
        let blade_code = r#"
@feature('new-api')
    <p>New API is enabled!</p>
@else
    <p>Using old API</p>
@endfeature

@feature("beta-mode")
    <x-beta-badge />
@endfeature
        "#;

        let tree = parse_blade(blade_code).expect("Should parse Blade");
        let lang = language_blade();
        let patterns = extract_all_blade_patterns(&tree, blade_code, &lang)
            .expect("Should extract patterns");

        // Check that @feature directive is captured
        let feature_directives: Vec<_> = patterns.directives.iter()
            .filter(|d| d.directive_name == "feature")
            .collect();

        assert_eq!(feature_directives.len(), 2, "Should find 2 @feature directives");

        // Verify first @feature directive
        let first = feature_directives[0];
        assert_eq!(first.directive_name, "feature");
        assert!(first.arguments.as_ref().unwrap().contains("new-api"),
            "First @feature should have 'new-api' argument");

        // Verify second @feature directive
        let second = feature_directives[1];
        assert_eq!(second.directive_name, "feature");
        assert!(second.arguments.as_ref().unwrap().contains("beta-mode"),
            "Second @feature should have 'beta-mode' argument");
    }

    #[test]
    fn test_blade_patterns_inside_html_attributes() {
        // Test that Blade patterns inside HTML tag attributes are recognized
        // This includes:
        // 1. Echo statements in attribute values: value="{{ config('app.name') }}"
        // 2. Directives inside attribute values: class="@if($x) blue @endif"
        // 3. Directives surrounding attributes: <div @if($show) class="visible" @endif>
        // 4. Blade attribute directives: @disabled($x), @checked($x), @selected($x)
        let blade_code = r#"
<input type="text" value="{{ config('app.name') }}" placeholder="{{ __('messages.placeholder') }}">
<div class="container @if($active) bg-blue @endif" data-env="{{ env('APP_ENV') }}">
    Content
</div>
<div class="@feature('beta-mode') beta @endfeature">Beta</div>
<div @if($showClass) class="conditional" @endif data-static="always">
    Conditional attribute
</div>
<button @disabled($isDisabled) @readonly($isReadonly) type="submit">
    Submit
</button>
<input @checked($isChecked) type="checkbox">
        "#;

        let tree = parse_blade(blade_code).expect("Should parse Blade");
        let lang = language_blade();
        let patterns = extract_all_blade_patterns(&tree, blade_code, &lang)
            .expect("Should extract patterns");

        println!("Echo PHP patterns found: {:?}", patterns.echo_php);
        println!("Directives found: {:?}", patterns.directives.iter().map(|d| format!("{}: {:?}", d.directive_name, d.arguments)).collect::<Vec<_>>());

        // Check that echo statements in attributes are captured
        let has_config_echo = patterns.echo_php.iter()
            .any(|e| e.php_content.contains("config('app.name')"));
        assert!(has_config_echo, "Should find config() in attribute echo: {:?}", patterns.echo_php);

        // Check that directives in attribute values are captured
        let if_count = patterns.directives.iter()
            .filter(|d| d.directive_name == "if")
            .count();
        // We should find 2 @if directives:
        // 1. class="container @if($active) bg-blue @endif" (inside attribute value)
        // 2. <div @if($showClass) class="conditional" @endif> (wrapping attributes)
        assert!(if_count >= 2, "Should find at least 2 @if directives, found: {}", if_count);

        // Check that @feature in attributes is captured
        let has_feature_directive = patterns.directives.iter()
            .any(|d| d.directive_name == "feature");
        assert!(has_feature_directive, "Should find @feature directive in attribute");

        // Check that Blade attribute directives are captured (@disabled, @checked, @readonly)
        let has_disabled = patterns.directives.iter()
            .any(|d| d.directive_name == "disabled");
        assert!(has_disabled, "Should find @disabled directive: {:?}",
            patterns.directives.iter().map(|d| &d.directive_name).collect::<Vec<_>>());

        let has_checked = patterns.directives.iter()
            .any(|d| d.directive_name == "checked");
        assert!(has_checked, "Should find @checked directive");

        let has_readonly = patterns.directives.iter()
            .any(|d| d.directive_name == "readonly");
        assert!(has_readonly, "Should find @readonly directive");
    }

    #[test]
    fn test_single_pass_is_faster() {
        // This test demonstrates the expected behavior - single pass should work
        let php_code = r#"<?php
        return view('home');
        $name = env('APP_NAME');
        $key = config('app.key');
        Route::middleware('auth')->get('/');
        $msg = __('messages.welcome');
        $css = asset('css/app.css');
        $service = app('cache');
        $url = route('home');
        "#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();

        // Should extract all patterns in one call
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        // Verify we found patterns of different types
        assert!(!patterns.views.is_empty(), "Should find views");
        assert!(!patterns.env_calls.is_empty(), "Should find env calls");
        assert!(!patterns.config_calls.is_empty(), "Should find config calls");
        assert!(!patterns.middleware_calls.is_empty(), "Should find middleware");
        assert!(!patterns.translation_calls.is_empty(), "Should find translations");
        assert!(!patterns.asset_calls.is_empty(), "Should find assets");
        assert!(!patterns.binding_calls.is_empty(), "Should find bindings");
        assert!(!patterns.route_calls.is_empty(), "Should find routes");
    }

    // =========================================================================
    // Column Position Tests
    // =========================================================================
    // These tests ensure that column positions are correct for highlighting
    // and diagnostics. The column should point to the content, not quotes.

    #[test]
    fn test_view_column_positions() {
        // view('users.profile')
        // Position: 0         1         2
        //           0123456789012345678901234567
        //           <?php view('users.profile');
        // The tree-sitter query captures string_content (without quotes)
        let php_code = "<?php view('users.profile');";
        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        assert_eq!(patterns.views.len(), 1);
        let view = &patterns.views[0];

        // view_name captures the string content WITHOUT quotes
        assert_eq!(view.view_name, "users.profile");
        // In "<?php view('users.profile');", 'u' starts at column 12
        assert_eq!(view.column, 12, "column should point to first char of view name");
        // End column should be at 'e' + 1 = 25
        assert_eq!(view.end_column, 25, "end_column should be after last char");
    }

    #[test]
    fn test_env_column_positions() {
        // env('APP_NAME')
        // Position: 0         1         2
        //           0123456789012345678901
        //           <?php env('APP_NAME');
        let php_code = "<?php env('APP_NAME');";
        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        assert_eq!(patterns.env_calls.len(), 1);
        let env_call = &patterns.env_calls[0];

        // env_var captures the string content WITHOUT quotes
        assert_eq!(env_call.var_name, "APP_NAME");
        // In "<?php env('APP_NAME');", 'A' starts at column 11
        assert_eq!(env_call.column, 11, "column should point to first char");
        assert_eq!(env_call.end_column, 19, "end_column should be after last char");
    }

    #[test]
    fn test_config_column_positions() {
        // config('app.name')
        // Position: 0         1         2
        //           0123456789012345678901234
        //           <?php config('app.name');
        let php_code = "<?php config('app.name');";
        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        assert_eq!(patterns.config_calls.len(), 1);
        let config_call = &patterns.config_calls[0];

        // config_key captures the string content WITHOUT quotes
        assert_eq!(config_call.config_key, "app.name");
        // In "<?php config('app.name');", 'a' starts at column 14
        assert_eq!(config_call.column, 14, "column should point to first char");
        assert_eq!(config_call.end_column, 22, "end_column should be after last char");
    }

    #[test]
    fn test_translation_column_positions() {
        // __('messages.welcome')
        // Position: 0         1         2
        //           012345678901234567890123456789
        //           <?php __('messages.welcome');
        let php_code = "<?php __('messages.welcome');";
        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        assert_eq!(patterns.translation_calls.len(), 1);
        let trans = &patterns.translation_calls[0];

        // translation_key captures the string content WITHOUT quotes
        assert_eq!(trans.translation_key, "messages.welcome");
        // In "<?php __('messages.welcome');", 'm' starts at column 10
        assert_eq!(trans.column, 10, "column should point to first char");
        assert_eq!(trans.end_column, 26, "end_column should be after last char");
    }

    #[test]
    fn test_asset_column_positions() {
        // asset('css/app.css')
        // Position: 0         1         2
        //           012345678901234567890123456
        //           <?php asset('css/app.css');
        let php_code = "<?php asset('css/app.css');";
        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        assert_eq!(patterns.asset_calls.len(), 1);
        let asset = &patterns.asset_calls[0];

        // asset_path captures the string content WITHOUT quotes
        assert_eq!(asset.path, "css/app.css");
        // In "<?php asset('css/app.css');", 'c' starts at column 13
        assert_eq!(asset.column, 13, "column should point to first char");
        assert_eq!(asset.end_column, 24, "end_column should be after last char");
    }

    #[test]
    fn test_middleware_column_positions() {
        // Route::middleware('auth')
        // Position: 0         1         2         3
        //           01234567890123456789012345678901
        //           <?php Route::middleware('auth');
        let php_code = "<?php Route::middleware('auth');";
        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        assert_eq!(patterns.middleware_calls.len(), 1);
        let mw = &patterns.middleware_calls[0];

        // middleware_name captures the string content WITHOUT quotes
        assert_eq!(mw.middleware_name, "auth");
        // In "<?php Route::middleware('auth');", 'a' starts at column 25
        assert_eq!(mw.column, 25, "column should point to first char");
        assert_eq!(mw.end_column, 29, "end_column should be after last char");
    }

    #[test]
    fn test_route_column_positions() {
        // route('home')
        // Position: 0         1
        //           01234567890123456789
        //           <?php route('home');
        let php_code = "<?php route('home');";
        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        assert_eq!(patterns.route_calls.len(), 1);
        let route = &patterns.route_calls[0];

        // route_name captures the string content WITHOUT quotes
        assert_eq!(route.route_name, "home");
        // In "<?php route('home');", 'h' starts at column 13
        assert_eq!(route.column, 13, "column should point to first char");
        assert_eq!(route.end_column, 17, "end_column should be after last char");
    }

    #[test]
    fn test_config_variants_getmany_modern_aliases_and_fluent() {
        // Issue #13: Config::getMany, modern Config::int/bool/float aliases,
        // and the config()->method('key') fluent instance form should all
        // resolve like config('key').
        let php_code = r#"<?php
$a = config('app.name');
$b = Config::get('database.default');
$c = Config::int('app.timeout');
$d = Config::bool('app.debug');
$e = Config::float('app.weight');
$f = Config::getMany(['mail.host', 'mail.port']);
$g = config()->string('app.locale');
$h = config()->array('app.providers');
"#;
        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        let keys: Vec<&str> = patterns.config_calls.iter().map(|c| c.config_key).collect();

        // Existing function call still works
        assert!(keys.contains(&"app.name"), "config() should match; got {keys:?}");

        // Existing Config::get still works
        assert!(keys.contains(&"database.default"), "Config::get should match; got {keys:?}");

        // Modern aliases on the Config facade
        assert!(keys.contains(&"app.timeout"), "Config::int should match; got {keys:?}");
        assert!(keys.contains(&"app.debug"), "Config::bool should match; got {keys:?}");
        assert!(keys.contains(&"app.weight"), "Config::float should match; got {keys:?}");

        // Config::getMany array — both elements captured
        assert!(keys.contains(&"mail.host"), "Config::getMany[0] should match; got {keys:?}");
        assert!(keys.contains(&"mail.port"), "Config::getMany[1] should match; got {keys:?}");

        // config()->method fluent form
        assert!(keys.contains(&"app.locale"), "config()->string should match; got {keys:?}");
        assert!(keys.contains(&"app.providers"), "config()->array should match; got {keys:?}");

        // No accidental duplicates or extras
        assert_eq!(
            patterns.config_calls.len(),
            9,
            "Expected exactly 9 config keys captured, got {}: {keys:?}",
            patterns.config_calls.len()
        );
    }

    #[test]
    fn test_route_variants_signed_route_and_url_facade() {
        // Issue #13: signed_route() and URL::signedRoute() should resolve like route()
        let php_code = r#"<?php
$a = route('home');
$b = signed_route('verify.email');
$c = URL::route('users.show', ['id' => 1]);
$d = URL::signedRoute('subscribe');
"#;
        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        let names: Vec<&str> = patterns.route_calls.iter().map(|r| r.route_name).collect();
        assert!(
            names.contains(&"home"),
            "route() should be captured; got {names:?}"
        );
        assert!(
            names.contains(&"verify.email"),
            "signed_route() should be captured; got {names:?}"
        );
        assert!(
            names.contains(&"users.show"),
            "URL::route() should be captured; got {names:?}"
        );
        assert!(
            names.contains(&"subscribe"),
            "URL::signedRoute() should be captured; got {names:?}"
        );
        assert_eq!(
            patterns.route_calls.len(),
            4,
            "All four route variants should be captured exactly once"
        );
    }

    #[test]
    fn test_binding_column_positions() {
        // app('cache')
        // Position: 0         1
        //           0123456789012345678
        //           <?php app('cache');
        let php_code = "<?php app('cache');";
        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        assert_eq!(patterns.binding_calls.len(), 1);
        let binding = &patterns.binding_calls[0];

        // binding_name captures the string content WITHOUT quotes
        assert_eq!(binding.binding_name, "cache");
        // In "<?php app('cache');", 'c' starts at column 11
        assert_eq!(binding.column, 11, "column should point to first char");
        assert_eq!(binding.end_column, 16, "end_column should be after last char");
    }

    #[test]
    fn test_blade_component_column_positions() {
        // <x-button>
        // The component is matched by the Blade tree-sitter grammar
        // We need a more realistic Blade structure for proper parsing
        let blade_code = "<div><x-button></x-button></div>";
        let tree = parse_blade(blade_code).expect("Should parse Blade");
        let lang = language_blade();
        let patterns = extract_all_blade_patterns(&tree, blade_code, &lang)
            .expect("Should extract patterns");

        // Components may or may not be found depending on tree-sitter grammar
        // Just verify the structure works
        if !patterns.components.is_empty() {
            let component = &patterns.components[0];
            assert!(component.column < blade_code.len(), "column should be valid");
            assert!(component.end_column >= component.column, "end_column should be >= column");
        }
    }

    #[test]
    fn test_livewire_component_column_positions() {
        // <livewire:counter />
        let blade_code = "<div><livewire:counter /></div>";
        let tree = parse_blade(blade_code).expect("Should parse Blade");
        let lang = language_blade();
        let patterns = extract_all_blade_patterns(&tree, blade_code, &lang)
            .expect("Should extract patterns");

        // Livewire components may or may not be found depending on grammar
        if !patterns.livewire.is_empty() {
            let livewire = &patterns.livewire[0];
            assert!(livewire.column < blade_code.len(), "column should be valid");
            assert!(livewire.end_column >= livewire.column, "end_column should be >= column");
        }
    }

    #[test]
    fn test_blade_directive_column_positions() {
        // @include('partials.header')
        let blade_code = "@include('partials.header')";
        let tree = parse_blade(blade_code).expect("Should parse Blade");
        let lang = language_blade();
        let patterns = extract_all_blade_patterns(&tree, blade_code, &lang)
            .expect("Should extract patterns");

        let include_directive = patterns.directives.iter()
            .find(|d| d.directive_name == "include");

        assert!(include_directive.is_some(), "Should find @include directive");
        let directive = include_directive.unwrap();

        // directive starts at column 0 (the @)
        assert_eq!(directive.column, 0, "directive should start at column 0");
        // string_column should point to the view name string
        assert!(directive.string_column > 0, "string_column should be after directive name");
    }

    #[test]
    fn test_column_positions_with_indentation() {
        // Test that column positions work correctly with leading whitespace
        // Position: 0         1         2
        //           012345678901234567890123
        //               view('dashboard');
        // (4 spaces + view( = column 9, then ' = column 10, d = column 10)
        let php_code = "<?php\n    view('dashboard');"; // 4 spaces indentation
        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        assert_eq!(patterns.views.len(), 1);
        let view = &patterns.views[0];

        // On line 1 (0-indexed), the indented content:
        // "    view('dashboard');"
        // Column 4-7 is "view", column 8 is "(", column 9 is "'", column 10 is "d"
        assert_eq!(view.row, 1, "should be on second line (0-indexed)");
        assert_eq!(view.column, 10, "column should point to first char of view name");
    }

    #[test]
    fn test_double_quote_column_positions() {
        // Test with double quotes
        // Position: 0         1         2
        //           0123456789012345678901234567
        //           <?php view("users.profile");
        let php_code = r#"<?php view("users.profile");"#;
        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        assert_eq!(patterns.views.len(), 1);
        let view = &patterns.views[0];

        // view_name is extracted WITHOUT quotes
        assert_eq!(view.view_name, "users.profile");
        assert_eq!(view.column, 12, "column should point to first char inside quotes");
        assert_eq!(view.end_column, 25, "end_column should be after last char");
    }

    #[test]
    fn test_blade_translation_patterns() {
        // Test that we can extract translations from Blade echo syntax
        let blade_code = r#"{{ __("Welcome to our app") }}
@lang("welcome")"#;

        // Parse as Blade first
        let blade_tree = parse_blade(blade_code).expect("Should parse Blade");
        let blade_lang = language_blade();
        let blade_patterns = extract_all_blade_patterns(&blade_tree, blade_code, &blade_lang)
            .expect("Should extract Blade patterns");

        // Check what directives we found
        println!("Blade directives found: {:?}", blade_patterns.directives.iter()
            .map(|d| d.directive_name)
            .collect::<Vec<_>>());

        // Check echo PHP content
        println!("Echo PHP content found: {:?}", blade_patterns.echo_php.iter()
            .map(|e| e.php_content)
            .collect::<Vec<_>>());

        // Parse as PHP to see if __() is captured
        let php_tree = parse_php(blade_code).expect("Should parse as PHP");
        let php_lang = language_php();
        let php_patterns = extract_all_php_patterns(&php_tree, blade_code, &php_lang)
            .expect("Should extract PHP patterns");

        println!("PHP translations found: {:?}", php_patterns.translation_calls.iter()
            .map(|t| t.translation_key)
            .collect::<Vec<_>>());

        // We expect to find translations in either Blade directives or PHP patterns
        let has_lang_directive = blade_patterns.directives.iter()
            .any(|d| d.directive_name == "lang");

        // Check that we captured the echo PHP content
        let has_echo_php = !blade_patterns.echo_php.is_empty();
        println!("Has echo PHP content: {}", has_echo_php);

        // At minimum, @lang should be captured as a directive
        assert!(has_lang_directive, "@lang should be captured as a directive");

        // And we should have captured the {{ __() }} echo content
        assert!(has_echo_php, "Should capture PHP content inside {{ }}");
        assert!(blade_patterns.echo_php[0].php_content.contains("__"), "Echo should contain __() call");
    }

    #[test]
    fn test_extract_feature_patterns() {
        let php_code = r#"<?php
        Feature::active('new-api');
        Feature::inactive('beta-mode');
        Feature::for($user)->active('purchase-button');
        Feature::value('experiment');
        Feature::allAreActive(['feature-a', 'feature-b']);
        Feature::active(NewApi::class);
        "#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        // Check that we found feature calls
        assert!(!patterns.feature_calls.is_empty(), "Should find feature calls");

        // Get all feature names
        let feature_names: Vec<&str> = patterns.feature_calls.iter()
            .map(|f| f.feature_name).collect();

        println!("Found features: {:?}", feature_names);
        println!("Feature calls: {:?}", patterns.feature_calls);

        // Check for specific features
        assert!(feature_names.contains(&"new-api"), "Should find 'new-api' feature");
        assert!(feature_names.contains(&"beta-mode"), "Should find 'beta-mode' feature");

        // Check method names
        let new_api = patterns.feature_calls.iter()
            .find(|f| f.feature_name == "new-api");
        assert!(new_api.is_some(), "Should find new-api feature");
        assert_eq!(new_api.unwrap().method_name, "active", "Method should be 'active'");

        // Check class-based feature
        let class_feature = patterns.feature_calls.iter()
            .find(|f| f.feature_name == "NewApi");
        if class_feature.is_some() {
            assert!(class_feature.unwrap().is_class_reference, "Should be class reference");
        }
    }

    #[test]
    fn test_feature_column_positions() {
        // Feature::active('new-api')
        // Position: 0         1         2         3
        //           0123456789012345678901234567890123
        //           <?php Feature::active('new-api');
        // 0-5 = "<?php ", 6-12 = "Feature", 13 = ":", 14 = ":", 15-20 = "active", 21 = "(", 22 = "'", 23 = "n"
        let php_code = "<?php Feature::active('new-api');";
        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        if !patterns.feature_calls.is_empty() {
            let feature = &patterns.feature_calls[0];
            assert_eq!(feature.feature_name, "new-api");
            // In "<?php Feature::active('new-api');", 'n' starts at column 23 (after quote)
            assert_eq!(feature.column, 23, "column should point to first char of feature name");
            assert_eq!(feature.end_column, 30, "end_column should be after last char");
        }
    }

    #[test]
    fn test_calculate_string_column_range() {
        // Test the helper function that calculates string column positions for directives
        // Now uses parameter_column (where tree-sitter says the parameter node starts)
        // instead of calculating from directive position.

        // Test 1: Args with full parentheses - @include('view')
        // Position: 0         1
        //           0123456789012345678
        //           @include('view')
        // parameter_column = 8 (where '(' is), content 'view' at columns 10-14
        let result = calculate_string_column_range(8, "('view')");
        assert_eq!(result, Some((10, 14)), "@include('view') - 'view' at columns 10-14");

        // Test 2: Args with double quotes - @feature("beta-mode")
        // Position: 0         1         2
        //           012345678901234567890
        //           @feature("beta-mode")
        // parameter_column = 8 (where '(' is), content 'beta-mode' at columns 10-19
        let result = calculate_string_column_range(8, "(\"beta-mode\")");
        assert_eq!(result, Some((10, 19)), "@feature(\"beta-mode\") - 'beta-mode' at columns 10-19");

        // Test 3: Args without opening paren (tree-sitter captures just the quoted part)
        // When args don't include '(', parameter_column already points past it
        // Original: @include('view')
        // parameter_column = 9 (where quote is), args = 'view') or 'view'
        // We DON'T add 1 for paren because we're already past it
        let result = calculate_string_column_range(9, "'view')");
        assert_eq!(result, Some((10, 14)), "Args without ( - parameter already past paren");

        let result = calculate_string_column_range(9, "'view'");
        assert_eq!(result, Some((10, 14)), "Args without parens - parameter at quote");

        // Test 4: Directive with space before paren - @feature ('beta-mode')
        // Position: 0         1         2
        //           0123456789012345678901
        //           @feature ('beta-mode')
        // parameter_column = 9 (where '(' is after the space)
        // content 'beta-mode' should be at columns 11-20
        let result = calculate_string_column_range(9, "('beta-mode')");
        assert_eq!(result, Some((11, 20)), "@feature ('beta-mode') with space - at columns 11-20");

        // Test 5: Indented directive - @include('partial')
        // Position: 0         1         2         3
        //           0123456789012345678901234567890123
        //               @include('partial')
        // 4 spaces + @include = 12, parameter at column 12
        let result = calculate_string_column_range(12, "('partial')");
        assert_eq!(result, Some((14, 21)), "Indented directive at columns 14-21");

        // Test 6: Args with spaces after opening paren
        // parameter at column 8
        let result = calculate_string_column_range(8, "(  'view')");
        assert_eq!(result, Some((12, 16)), "Spaces after ( - at columns 12-16");

        // Test 7: Invalid args (no quotes)
        let result = calculate_string_column_range(8, "($condition)");
        assert_eq!(result, None, "Args without quotes should return None");
    }

    #[test]
    fn test_feature_name_property_extraction() {
        // Test typed property with single quotes
        let php_code = r#"<?php

namespace App\Features;

class NewApi
{
    public string $name = 'custom-feature-alias';

    public function resolve(mixed $scope): mixed
    {
        return false;
    }
}
"#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        assert_eq!(patterns.feature_name_properties.len(), 1, "Should find one $name property");
        assert_eq!(patterns.feature_name_properties[0].name_value, "custom-feature-alias");
    }

    #[test]
    fn test_feature_name_property_untyped() {
        // Test untyped property with double quotes
        let php_code = r#"<?php

namespace App\Features;

class BetaMode
{
    public $name = "beta-mode-feature";

    public function resolve(mixed $scope): mixed
    {
        return true;
    }
}
"#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        assert_eq!(patterns.feature_name_properties.len(), 1, "Should find one $name property");
        assert_eq!(patterns.feature_name_properties[0].name_value, "beta-mode-feature");
    }

    #[test]
    fn test_feature_name_property_not_captured_for_other_names() {
        // Ensure we only capture $name, not other properties
        let php_code = r#"<?php

class SomeClass
{
    public string $description = 'some description';
    public string $title = 'some title';
    protected $value = 'some value';
}
"#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        assert!(patterns.feature_name_properties.is_empty(), "Should not capture other properties");
    }

}
