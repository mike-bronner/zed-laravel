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

use crate::salsa_impl::AccessForm;

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
    PHP_QUERY_CACHE
        .as_ref()
        .ok_or_else(|| anyhow!("Failed to compile PHP query"))
}

/// Get the cached Blade query, or compile it if needed
fn get_blade_query(_language: &Language) -> Result<&'static Query> {
    BLADE_QUERY_CACHE
        .as_ref()
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

/// Represents a member access in PHP code — property-form (`$user->email`,
/// `$this->profile`, `$user?->name`) or call-form (`$user->active()`,
/// `User::whereEmail()`), distinguished by [`MemberAccessMatch::form`].
///
/// This is the raw capture only: resolving the receiver to a declaring class
/// and classifying the member (scope / accessor / relationship / column /
/// dynamic finder) happens later (M3 of the semantic-index plan; call-form
/// added in #77).
#[derive(Debug, Clone, PartialEq)]
pub struct MemberAccessMatch<'a> {
    /// The accessed member name (e.g. `email`, `posts`, `profile`).
    pub member: &'a str,
    /// Raw source text of the receiver expression (e.g. `$user`, `$this`).
    pub receiver: &'a str,
    /// Byte range of the receiver expression — lets the M3 resolver locate the
    /// receiver node in the live tree to run `var_type::resolve`.
    pub receiver_byte_start: usize,
    pub receiver_byte_end: usize,
    /// Whether the access used the nullsafe operator (`?->`).
    pub is_nullsafe: bool,
    /// Property read vs instance/static call (`$user->email` vs
    /// `$user->active()` / `User::whereEmail()`).
    pub form: AccessForm,
    /// Byte range of the member name node.
    pub byte_start: usize,
    pub byte_end: usize,
    /// Position of the member name (0-based — repo convention).
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
    /// Property-form member accesses (`$user->email`, `$this->profile`).
    /// Raw capture for the semantic-index magic-member work (M2).
    pub member_accesses: Vec<MemberAccessMatch<'a>>,
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
                let view = query_match
                    .captures
                    .iter()
                    .find(|c| query.capture_names()[c.index as usize] == "blade_alias_view")
                    .and_then(|c| c.node.utf8_text(source_bytes).ok());

                if let Some(view) = view {
                    result
                        .blade_component_aliases
                        .push(BladeComponentAliasMatch {
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
                let class_name = query_match
                    .captures
                    .iter()
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
                let method_name =
                    get_feature_method_name(query_match, query, source_bytes).unwrap_or("active");
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
                let method_name =
                    get_feature_method_name(query_match, query, source_bytes).unwrap_or("active");
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
                result
                    .feature_name_properties
                    .push(FeatureNamePropertyMatch {
                        name_value: text,
                        byte_start: node.start_byte(),
                        byte_end: node.end_byte(),
                        row: start_pos.row,
                        column: start_pos.column,
                        end_column: end_pos.column,
                    });
            }

            // Property-form member access ($user->email, $this->profile).
            // `node` is the member NAME; its parent is the
            // (nullsafe_)member_access_expression carrying the receiver.
            "member_access_name" => {
                let Some(parent) = node.parent() else {
                    continue;
                };
                let Some(object) = parent.child_by_field_name("object") else {
                    continue;
                };
                let Ok(receiver) = object.utf8_text(source_bytes) else {
                    continue;
                };
                result.member_accesses.push(MemberAccessMatch {
                    member: text,
                    receiver,
                    receiver_byte_start: object.start_byte(),
                    receiver_byte_end: object.end_byte(),
                    is_nullsafe: parent.kind() == "nullsafe_member_access_expression",
                    form: AccessForm::Property,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }

            // Call-form member access, instance ($user->active(), $user->posts()).
            // Same shape as the property arm; the receiver is the call's
            // `object` field. (#77)
            "member_call_name" => {
                let Some(parent) = node.parent() else {
                    continue;
                };
                let Some(object) = parent.child_by_field_name("object") else {
                    continue;
                };
                let Ok(receiver) = object.utf8_text(source_bytes) else {
                    continue;
                };
                result.member_accesses.push(MemberAccessMatch {
                    member: text,
                    receiver,
                    receiver_byte_start: object.start_byte(),
                    receiver_byte_end: object.end_byte(),
                    is_nullsafe: parent.kind() == "nullsafe_member_call_expression",
                    form: AccessForm::InstanceCall,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }

            // Call-form member access, static (User::active(), User::whereEmail()).
            // The receiver is the call's `scope` field — a class name /
            // qualified name (or `relative_scope` for self::/static::, which
            // later fails receiver resolution and drops). (#77)
            "scoped_call_name" => {
                let Some(parent) = node.parent() else {
                    continue;
                };
                let Some(scope) = parent.child_by_field_name("scope") else {
                    continue;
                };
                let Ok(receiver) = scope.utf8_text(source_bytes) else {
                    continue;
                };
                result.member_accesses.push(MemberAccessMatch {
                    member: text,
                    receiver,
                    receiver_byte_start: scope.start_byte(),
                    receiver_byte_end: scope.end_byte(),
                    is_nullsafe: false,
                    form: AccessForm::StaticCall,
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
    let pattern_count = result.views.len()
        + result.env_calls.len()
        + result.config_calls.len()
        + result.middleware_calls.len()
        + result.translation_calls.len()
        + result.asset_calls.len()
        + result.binding_calls.len()
        + result.route_calls.len()
        + result.url_calls.len()
        + result.action_calls.len()
        + result.feature_calls.len()
        + result.feature_name_properties.len();
    // Per-file extraction stats are spammy at scale — a 40k-file project
    // generates >1M log lines, which dominates warming time. Demote to debug.
    tracing::debug!(
        "📊 PHP extraction: {:?} total (query fetch: {:?}), {} patterns found",
        total_time,
        query_fetch_time,
        pattern_count
    );

    Ok(result)
}

/// Whether an extracted component / Livewire name is constructed at runtime
/// rather than being a static literal.
///
/// Component, Livewire, and view names are kebab/dotted/colon-cased — they
/// never legitimately contain `{` or `$`. So either marker means the name
/// carries an interpolated part (`<x-alert-{{ $type }}>`,
/// `@livewire("edit-{$type}-flow")`, `${x}`) and the real target isn't known
/// until runtime. Such references can't be resolved or validated statically, so
/// callers skip them instead of emitting a phantom "not found" diagnostic.
fn name_is_runtime_constructed(name: &str) -> bool {
    name.contains('{') || name.contains('$')
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
                    // Blade component. A runtime-interpolated tag name
                    // (`<x-alert-{{ $type }}>`) names no single component, so
                    // skip it rather than emit a phantom "not found".
                    if !name_is_runtime_constructed(component_name) {
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
                    }
                } else if let Some(component_name) = text.strip_prefix("livewire:") {
                    // Livewire component tag syntax (prefix stripped). Same
                    // dynamic-name guard as Blade components above.
                    if !name_is_runtime_constructed(component_name) {
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
                    (
                        "extends" | "include" | "slot" | "component" | "lang" | "feature"
                        | "livewire",
                        Some(info),
                    ) => calculate_string_column_range(info.column, info.text)
                        .unwrap_or((directive_column, directive_end_column)),
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

                // `@livewire('counter')` also feeds the Livewire bucket
                // so the symbol index treats directive-form references
                // the same as `<livewire:counter>` tag references —
                // find-references / rename / hover all see both.
                if directive_name == "livewire" {
                    if let Some(info) = &param_info {
                        let trimmed = info
                            .text
                            .trim()
                            .trim_start_matches('(')
                            .trim_end_matches(')')
                            .trim();
                        if let Some(first_arg) = trimmed.split(',').next() {
                            let first_arg = first_arg.trim();
                            // Three shapes appear in real Laravel code:
                            //   - `@livewire('name')` — string name (the
                            //     common case)
                            //   - `@livewire(Foo::class)` — class-reference
                            //     form (less common; the runtime derives
                            //     the kebab name from the class basename)
                            //   - `@livewire($var)` — dynamic; can't be
                            //     resolved statically, skip entirely
                            let component_name: Option<&str> = if first_arg.starts_with('\'')
                                || first_arg.starts_with('"')
                            {
                                // String literal — strip the quotes.
                                let unquoted = first_arg
                                    .trim_start_matches(['\'', '"'])
                                    .trim_end_matches(['\'', '"']);
                                // A double-quoted name with interpolation
                                // (`@livewire("edit-{$type}-flow")`) is built
                                // at runtime — not a static reference, skip.
                                if unquoted.is_empty() || name_is_runtime_constructed(unquoted) {
                                    None
                                } else {
                                    Some(unquoted)
                                }
                            } else if let Some(class_fqn) = first_arg.strip_suffix("::class") {
                                // Class reference — extract the basename
                                // (slice into source, no allocation).
                                // e.g. `App\Livewire\NestedComponentA::class`
                                // → `NestedComponentA`. The basename
                                // stays in PascalCase here; if cross-
                                // form linkage to `<livewire:kebab-case>`
                                // tags is needed later, the salsa layer
                                // can normalize when it copies the name
                                // into an owned String.
                                let trimmed_fqn = class_fqn.trim();
                                let basename =
                                    trimmed_fqn.rsplit('\\').next().unwrap_or(trimmed_fqn);
                                if basename.is_empty() {
                                    None
                                } else {
                                    Some(basename)
                                }
                            } else {
                                // Dynamic (`$var`) or anything else
                                // we can't resolve at parse time.
                                None
                            };

                            if let Some(name) = component_name {
                                result.livewire.push(LivewireMatch {
                                    component_name: name,
                                    byte_start: node.start_byte(),
                                    byte_end: node.end_byte(),
                                    row: start_pos.row,
                                    column: string_column,
                                    end_column: string_end_column,
                                });
                            }
                        }
                    }
                }
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
                let (string_column, string_end_column) =
                    match (directive_name, &arguments, paren_pos) {
                        (
                            "extends" | "include" | "slot" | "component" | "lang" | "feature"
                            | "livewire",
                            Some(args),
                            Some(pos),
                        ) => {
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
    let pattern_count = result.components.len()
        + result.livewire.len()
        + result.directives.len()
        + result.echo_php.len()
        + result.slots.len();
    tracing::debug!(
        "📊 Blade extraction: {:?} total (query fetch: {:?}), {} patterns found",
        total_time,
        query_fetch_time,
        pattern_count
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
    let (paren_offset, content) = if let Some(rest) = trimmed.strip_prefix('(') {
        // Args include '(' - need to skip past it
        (1, rest)
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

#[cfg(test)]
mod tests;
