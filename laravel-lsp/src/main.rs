// The LSP server holds a number of deeply-nested Arc/RwLock state fields
// (caches, route indexes, vendor maps). Extracting type aliases for each
// would be more noise than signal; allow the complex types crate-wide to
// match the same opt-out already in place on the library at lib.rs:10.
#![allow(clippy::type_complexity)]

use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio::time::sleep;
use tower_lsp::jsonrpc;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};
use tracing::{debug, info, warn};
use walkdir::WalkDir;

// Use the library crate for all modules
use laravel_lsp::cache_manager::{
    BindingEntry, CacheManager, CachedEnvVars, CachedLaravelConfig, MiddlewareEntry, RescanType,
    ScanResult,
};
use laravel_lsp::config::find_project_root;
use laravel_lsp::middleware_parser::{middleware_base_alias, resolve_class_to_file};
use laravel_lsp::route_discovery::{build_route_index, discover_route_files, RouteIndex};

// Salsa 0.25 database - integrated via actor pattern for async compatibility
use laravel_lsp::salsa_impl::{
    ActionReferenceData, AssetReferenceData, BindingReferenceData, ComponentReferenceData,
    ConfigReferenceData, DirectiveReferenceData, EnvReferenceData, FeatureReferenceData,
    LaravelConfigData, LivewireReferenceData, MiddlewareReferenceData, PatternAtPosition,
    ReferenceLocationData, RouteReferenceData, SalsaActor, SalsaHandle, TranslationReferenceData,
    UrlReferenceData, ViewReferenceData,
};

// ============================================================================
// PART 1: Core Language Server Implementation
// ============================================================================

/// Extract middleware configuration class imports from PHP content
///
/// Parses `use` statements to find imported middleware classes (like
/// `Illuminate\Foundation\Configuration\Middleware`) and resolves them
/// to file paths for scanning.
fn extract_middleware_imports(content: &str, root: &Path) -> Vec<PathBuf> {
    use lazy_static::lazy_static;
    use regex::Regex;

    lazy_static! {
        // Match: use Illuminate\...\Middleware;
        // or: use Some\Namespace\Configuration\Middleware;
        static ref USE_RE: Regex = Regex::new(
            r#"use\s+((?:[A-Za-z0-9_\\]+\\)?(?:Configuration\\)?Middleware)\s*;"#
        ).unwrap();
    }

    let mut files = Vec::new();

    for cap in USE_RE.captures_iter(content) {
        if let Some(class_match) = cap.get(1) {
            let class_name = class_match.as_str();

            // Resolve the class to a file path using PSR-4 conventions
            if let Some(file_path) = resolve_class_to_vendor_file(class_name, root) {
                if file_path.exists() {
                    files.push(file_path);
                }
            }
        }
    }

    files
}

/// Scan Blade source for `@props([..., 'var_name', ...])` and return the
/// 0-based line where the declaration sits. Recognises both single- and
/// double-quoted keys, both with and without `=> default` defaults.
///
/// Used by the hover handler as a fallback source location when a Blade
/// variable's type can't be resolved to a class — at least point the user at
/// `@props` so they know where the variable was declared in this template.
fn find_props_declaration_line(content: &str, var_name: &str) -> Option<u32> {
    use lazy_static::lazy_static;
    use regex::Regex;
    lazy_static! {
        // `@props(` followed by anything up to `)` — captured greedily but
        // bounded by `)` so we don't span past the directive.
        static ref PROPS_RE: Regex = Regex::new(r"@props\s*\(([^)]*)\)").unwrap();
    }
    for cap in PROPS_RE.captures_iter(content) {
        let body = cap.get(1)?.as_str();
        // Check for `'var_name'` or `"var_name"` as either an array value
        // (no `=>` after) or an array key (`=> default`).
        let single = format!("'{}'", var_name);
        let double = format!("\"{}\"", var_name);
        if body.contains(&single) || body.contains(&double) {
            // Compute the 0-based line of the match start.
            let match_start = cap.get(0)?.start();
            let mut line = 0u32;
            for (i, ch) in content.char_indices() {
                if i >= match_start {
                    break;
                }
                if ch == '\n' {
                    line += 1;
                }
            }
            return Some(line);
        }
    }
    None
}

/// Resolve a class name to a vendor file path using PSR-4 conventions
fn resolve_class_to_vendor_file(class_name: &str, root: &Path) -> Option<PathBuf> {
    // Common namespace to directory mappings
    let mappings = [
        ("Illuminate\\", "vendor/laravel/framework/src/Illuminate/"),
        ("Laravel\\", "vendor/laravel/"),
        ("App\\", "app/"),
    ];

    for (namespace, dir) in &mappings {
        if class_name.starts_with(namespace) {
            let relative = class_name.strip_prefix(namespace)?;
            let file_path = root
                .join(dir)
                .join(relative.replace('\\', "/"))
                .with_extension("php");
            return Some(file_path);
        }
    }

    None
}

// Removed: Old cache structures (FileReferences, ParsedMatches, ReferenceCache)
// These have been replaced by the high-performance PerformanceCache system

/// Result of checking if a translation exists
struct TranslationCheck {
    /// Whether the translation key exists
    exists: bool,
    /// Whether this is a dotted key (validation.required) vs text key ("Welcome")
    is_dotted_key: bool,
    /// The expected file path for this translation
    expected_path: Option<PathBuf>,
    /// Whether the translation file exists (separate from whether the key exists)
    file_exists: bool,
    /// The nested key within the file (for dotted keys like "validation.required" → "required")
    nested_key: Option<String>,
}

/// Result of checking if a config key exists
struct ConfigCheck {
    /// Whether the config key exists
    exists: bool,
    /// The expected file path for this config (e.g., config/app.php)
    expected_path: Option<PathBuf>,
    /// Whether the config file exists (separate from whether the key exists)
    file_exists: bool,
    /// The nested key within the file (e.g., "app.name" → "name")
    nested_key: Option<String>,
}

/// A config key for autocomplete
struct ConfigKeyCompletion {
    /// The full dot-notation key (e.g., "app.name")
    key: String,
    /// The value (truncated for display)
    value: String,
    /// Source file (e.g., "config/app.php")
    source: String,
}

/// A route name for autocomplete
struct RouteNameCompletion {
    /// The route name (e.g., "users.index")
    name: String,
    /// Source file (e.g., "routes/web.php")
    source: String,
}

/// A view name for autocomplete
struct ViewNameCompletion {
    /// The view name in dot notation (e.g., "users.profile")
    name: String,
    /// Relative path to the view file (e.g., "resources/views/users/profile.blade.php")
    path: String,
}

/// A Blade component for autocomplete
struct BladeComponentCompletion {
    /// The component name (e.g., "button", "forms.input")
    name: String,
    /// Relative path to the component file
    path: String,
}

/// A Livewire component for autocomplete
struct LivewireComponentCompletion {
    /// The component name in kebab-case (e.g., "user-profile", "admin.dashboard")
    name: String,
    /// Relative path to the component PHP file
    path: String,
}

/// A file path for autocomplete (used by asset(), @vite(), path helpers)
struct FilePathCompletion {
    /// The relative path from the base directory (e.g., "css/app.css", "js/app.js")
    path: String,
}

/// A model property for autocomplete (used by $model->)
struct ModelPropertyCompletion {
    /// The property name (e.g., "id", "email", "first_name")
    name: String,
    /// The PHP type (e.g., "int", "string", "Carbon", "Collection<Post>")
    php_type: String,
    /// Source of the property (database, cast, accessor, relationship)
    source: String,
}

/// A translation key for autocomplete
struct TranslationKeyCompletion {
    /// The full dot-notation key (e.g., "messages.welcome")
    key: String,
    /// The translated value (for display)
    value: String,
    /// Source file (e.g., "lang/en/messages.php")
    source: String,
}

/// A validation rule for autocomplete
struct ValidationRuleInfo {
    /// The rule name (e.g., "required", "email", "max")
    name: String,
    /// Brief description
    description: String,
    /// Whether this rule accepts parameters (e.g., "max:255")
    has_params: bool,
    /// Source: "laravel" for built-in, or file path for custom
    source: String,
}

/// Context for validation rule parameter completion (e.g., "exists:█" or "after:█")
struct ValidationParamContext {
    /// The rule name (e.g., "exists", "after", "dimensions")
    rule_name: String,
    /// Text typed after colon (or after last comma for multi-param rules)
    current_param: String,
    /// Full text after the colon (for rules that need to reference previous params)
    full_params: String,
    /// Parameter index (0 = first param, 1 = second, etc.)
    param_index: usize,
}

/// Represents the type of PHP array context we're inside
/// Used to distinguish validation arrays from model property arrays
#[derive(Debug, Clone, PartialEq)]
enum ArrayContext {
    /// Validation rules: $rules, rules() method, validate(), Validator::make()
    Validation,
    /// Model casts: $casts property or casts() method
    Casts,
    /// Model fillable/guarded: $fillable, $guarded
    MassAssignment,
    /// Model visibility: $hidden, $visible, $appends
    Visibility,
    /// Model relationships: $with, $withCount
    Relationships,
    /// Unknown or generic array context
    Unknown,
}

/// An Eloquent cast type for autocomplete
struct CastTypeInfo {
    /// The cast type name (e.g., "string", "datetime", "array")
    name: String,
    /// Brief description
    description: String,
    /// Whether this cast type has parameters (e.g., "decimal:2")
    has_params: bool,
    /// Source: "laravel" for built-in, or "app/Casts" for custom
    source: String,
}

/// Laravel's built-in Eloquent cast types
/// Reference: https://laravel.com/docs/12.x/eloquent-mutators#attribute-casting
fn get_laravel_cast_types() -> Vec<CastTypeInfo> {
    vec![
        // Primitive types
        CastTypeInfo {
            name: "array".into(),
            description: "JSON to PHP array".into(),
            has_params: false,
            source: "laravel".into(),
        },
        CastTypeInfo {
            name: "boolean".into(),
            description: "Cast to boolean".into(),
            has_params: false,
            source: "laravel".into(),
        },
        CastTypeInfo {
            name: "collection".into(),
            description: "JSON to Laravel Collection".into(),
            has_params: false,
            source: "laravel".into(),
        },
        CastTypeInfo {
            name: "date".into(),
            description: "Cast to Carbon date (without time)".into(),
            has_params: false,
            source: "laravel".into(),
        },
        CastTypeInfo {
            name: "datetime".into(),
            description: "Cast to Carbon datetime".into(),
            has_params: false,
            source: "laravel".into(),
        },
        CastTypeInfo {
            name: "immutable_date".into(),
            description: "Cast to CarbonImmutable date".into(),
            has_params: false,
            source: "laravel".into(),
        },
        CastTypeInfo {
            name: "immutable_datetime".into(),
            description: "Cast to CarbonImmutable datetime".into(),
            has_params: false,
            source: "laravel".into(),
        },
        CastTypeInfo {
            name: "decimal".into(),
            description: "Cast to decimal with precision (e.g., decimal:2)".into(),
            has_params: true,
            source: "laravel".into(),
        },
        CastTypeInfo {
            name: "double".into(),
            description: "Cast to double/float".into(),
            has_params: false,
            source: "laravel".into(),
        },
        CastTypeInfo {
            name: "float".into(),
            description: "Cast to float".into(),
            has_params: false,
            source: "laravel".into(),
        },
        CastTypeInfo {
            name: "hashed".into(),
            description: "Hash value when setting (Laravel 10+)".into(),
            has_params: false,
            source: "laravel".into(),
        },
        CastTypeInfo {
            name: "integer".into(),
            description: "Cast to integer".into(),
            has_params: false,
            source: "laravel".into(),
        },
        CastTypeInfo {
            name: "object".into(),
            description: "JSON to PHP stdClass object".into(),
            has_params: false,
            source: "laravel".into(),
        },
        CastTypeInfo {
            name: "real".into(),
            description: "Cast to real/float".into(),
            has_params: false,
            source: "laravel".into(),
        },
        CastTypeInfo {
            name: "string".into(),
            description: "Cast to string".into(),
            has_params: false,
            source: "laravel".into(),
        },
        CastTypeInfo {
            name: "timestamp".into(),
            description: "Cast to Unix timestamp".into(),
            has_params: false,
            source: "laravel".into(),
        },
        // Encrypted types
        CastTypeInfo {
            name: "encrypted".into(),
            description: "Encrypt/decrypt value".into(),
            has_params: false,
            source: "laravel".into(),
        },
        CastTypeInfo {
            name: "encrypted:array".into(),
            description: "Encrypt/decrypt as array".into(),
            has_params: false,
            source: "laravel".into(),
        },
        CastTypeInfo {
            name: "encrypted:collection".into(),
            description: "Encrypt/decrypt as Collection".into(),
            has_params: false,
            source: "laravel".into(),
        },
        CastTypeInfo {
            name: "encrypted:object".into(),
            description: "Encrypt/decrypt as object".into(),
            has_params: false,
            source: "laravel".into(),
        },
        // Castable classes (common ones)
        CastTypeInfo {
            name: "AsStringable::class".into(),
            description: "Cast to Stringable instance".into(),
            has_params: false,
            source: "laravel".into(),
        },
        CastTypeInfo {
            name: "AsArrayObject::class".into(),
            description: "Cast to ArrayObject instance".into(),
            has_params: false,
            source: "laravel".into(),
        },
        CastTypeInfo {
            name: "AsCollection::class".into(),
            description: "Cast to Collection instance".into(),
            has_params: false,
            source: "laravel".into(),
        },
        CastTypeInfo {
            name: "AsEncryptedArrayObject::class".into(),
            description: "Encrypted ArrayObject".into(),
            has_params: false,
            source: "laravel".into(),
        },
        CastTypeInfo {
            name: "AsEncryptedCollection::class".into(),
            description: "Encrypted Collection".into(),
            has_params: false,
            source: "laravel".into(),
        },
        CastTypeInfo {
            name: "AsEnumArrayObject::class".into(),
            description: "Cast to EnumArrayObject".into(),
            has_params: false,
            source: "laravel".into(),
        },
        CastTypeInfo {
            name: "AsEnumCollection::class".into(),
            description: "Cast to EnumCollection".into(),
            has_params: false,
            source: "laravel".into(),
        },
    ]
}

/// Scan for cast classes from Laravel framework, packages, and app/Casts
fn scan_all_casts(project_root: &Path) -> Vec<CastTypeInfo> {
    let mut casts = Vec::new();

    // 1. Scan Laravel framework's built-in casts
    let laravel_casts_path =
        project_root.join("vendor/laravel/framework/src/Illuminate/Database/Eloquent/Casts");
    if laravel_casts_path.exists() {
        casts.extend(scan_cast_directory(
            &laravel_casts_path,
            "Illuminate\\Database\\Eloquent\\Casts",
            "laravel",
        ));
    }

    // 2. Scan common package locations for casts
    let package_paths = [
        // Spatie packages
        (
            "vendor/spatie/laravel-data/src/Casts",
            "Spatie\\LaravelData\\Casts",
        ),
        (
            "vendor/spatie/laravel-enum/src/Casts",
            "Spatie\\Enum\\Laravel\\Casts",
        ),
        // Add more common packages as needed
    ];

    for (rel_path, namespace) in package_paths {
        let package_path = project_root.join(rel_path);
        if package_path.exists() {
            casts.extend(scan_cast_directory(&package_path, namespace, "package"));
        }
    }

    // 3. Scan app/Casts for custom casts
    let app_casts_path = project_root.join("app/Casts");
    if app_casts_path.exists() {
        casts.extend(scan_cast_directory(
            &app_casts_path,
            "App\\Casts",
            "app/Casts",
        ));
    }

    casts
}

/// Scan a directory for cast classes
fn scan_cast_directory(dir_path: &Path, namespace: &str, source: &str) -> Vec<CastTypeInfo> {
    let mut casts = Vec::new();

    for entry in WalkDir::new(dir_path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "php"))
    {
        let path = entry.path();
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            // Skip interfaces, traits, and abstract classes by naming convention
            if stem.ends_with("Interface")
                || stem.ends_with("Trait")
                || stem.starts_with("Abstract")
            {
                continue;
            }

            // Build the relative path for nested directories
            let relative = path.strip_prefix(dir_path).unwrap_or(path);
            let class_path = relative
                .with_extension("")
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "\\");

            let full_class = format!("{}\\{}", namespace, class_path);
            let cast_name = format!("{}::class", class_path);
            let description = full_class.to_string();

            casts.push(CastTypeInfo {
                name: cast_name,
                description,
                has_params: true, // Casts might accept parameters
                source: source.into(),
            });
        }
    }

    casts
}

/// Convert a feature key to a class name
/// Examples:
///   "new-api" -> "NewApi"
///   "purchase_button" -> "PurchaseButton"
///   "myFeature" -> "MyFeature"
fn feature_key_to_class_name(key: &str) -> String {
    key.split(['-', '_'])
        .map(|s| {
            let mut chars = s.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().chain(chars).collect(),
            }
        })
        .collect()
}

/// Convert a class name to a feature key
/// Examples:
///   "NewApi" -> "new-api"
///   "PurchaseButton" -> "purchase-button"
fn class_name_to_feature_key(class_name: &str) -> String {
    let mut result = String::new();
    for (i, c) in class_name.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                result.push('-');
            }
            result.push(c.to_lowercase().next().unwrap_or(c));
        } else {
            result.push(c);
        }
    }
    result
}

/// Information about a Laravel Pennant feature class
#[derive(Debug, Clone)]
struct FeatureInfo {
    /// The string key used in Feature::active('key')
    pub feature_key: String,
    /// The class name (e.g., "NewApi")
    pub class_name: String,
    /// The full PHP namespace (e.g., "App\\Features\\NewApi")
    pub full_class: String,
}

/// Scan for feature classes in app/Features/
fn scan_feature_classes(project_root: &Path) -> Vec<FeatureInfo> {
    let mut features = Vec::new();

    let features_dir = project_root.join("app/Features");
    if !features_dir.exists() {
        return features;
    }

    for entry in WalkDir::new(&features_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "php"))
    {
        let path = entry.path();
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            // Skip interfaces, traits, and abstract classes by naming convention
            if stem.ends_with("Interface")
                || stem.ends_with("Trait")
                || stem.starts_with("Abstract")
            {
                continue;
            }

            // Build the relative path for nested directories
            let relative = path.strip_prefix(&features_dir).unwrap_or(path);
            let class_path = relative
                .with_extension("")
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "\\");

            let full_class = format!("App\\Features\\{}", class_path);
            let class_name = stem.to_string();

            // Try to extract the $name property from the feature class
            let feature_key = extract_feature_name_property(path)
                .unwrap_or_else(|| class_name_to_feature_key(stem));

            features.push(FeatureInfo {
                feature_key,
                class_name,
                full_class,
            });
        }
    }

    features
}

/// Extract the $name property value from a feature class file using tree-sitter
fn extract_feature_name_property(path: &Path) -> Option<String> {
    use laravel_lsp::parser::{language_php, parse_php};
    use laravel_lsp::queries::extract_all_php_patterns;

    // Read the file content
    let content = std::fs::read_to_string(path).ok()?;

    // Parse with tree-sitter
    let tree = parse_php(&content).ok()?;
    let language = language_php();

    // Extract patterns
    let patterns = extract_all_php_patterns(&tree, &content, &language).ok()?;

    // Return the first $name property value if found
    patterns
        .feature_name_properties
        .first()
        .map(|p| p.name_value.to_string())
}

/// Information about a discovered Blade directive
#[derive(Debug, Clone)]
struct BladeDirectiveInfo {
    /// The directive name (e.g., "if", "foreach", "feature")
    pub name: String,
    /// Description of the directive
    pub description: String,
    /// Whether the directive accepts parameters
    pub has_params: bool,
    /// The closing directive name if this is a block directive (e.g., "endif" for "if")
    pub closing: Option<String>,
    /// Source of the directive: "laravel", "app", or package name
    pub source: String,
}

/// Scan Laravel's BladeCompiler traits for built-in directives
fn scan_laravel_blade_directives(project_root: &Path) -> Vec<BladeDirectiveInfo> {
    use regex::Regex;

    let mut directives = Vec::new();

    // Path to Laravel's Blade compiler concerns (traits)
    let concerns_dir =
        project_root.join("vendor/laravel/framework/src/Illuminate/View/Compilers/Concerns");

    if !concerns_dir.exists() {
        // Fallback: return minimal core directives
        return get_fallback_blade_directives();
    }

    // Regex to match compile* methods: protected function compileIf($expression)
    let compile_method_re =
        Regex::new(r"protected\s+function\s+compile([A-Z][a-zA-Z]*)\s*\(").unwrap();

    // Scan all PHP files in the Concerns directory
    for entry in WalkDir::new(&concerns_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "php"))
    {
        if let Ok(content) = std::fs::read_to_string(entry.path()) {
            for cap in compile_method_re.captures_iter(&content) {
                if let Some(name_match) = cap.get(1) {
                    let method_name = name_match.as_str();
                    // Convert CamelCase to lowercase directive name
                    let directive_name = camel_to_directive(method_name);

                    // Skip "end" prefixed directives - they're closings, not standalone
                    if directive_name.starts_with("end") {
                        continue;
                    }
                    // Skip "else" variants - they're part of other directives
                    if directive_name.starts_with("else") && directive_name != "else" {
                        continue;
                    }

                    // Determine if it has a closing directive
                    let closing = find_closing_directive(&directive_name, &content);

                    // Check if it has parameters (look for $expression in method signature)
                    let has_params = content.contains(&format!("compile{}($", method_name))
                        || content.contains(&format!("compile{}( $", method_name));

                    // Generate description from trait file name
                    let source_file = entry
                        .path()
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("unknown");
                    let description = generate_directive_description(&directive_name, source_file);

                    directives.push(BladeDirectiveInfo {
                        name: directive_name,
                        description,
                        has_params,
                        closing,
                        source: "laravel".into(),
                    });
                }
            }
        }
    }

    // Remove duplicates (some directives might appear multiple times)
    directives.sort_by(|a, b| a.name.cmp(&b.name));
    directives.dedup_by(|a, b| a.name == b.name);

    // If we found nothing, return fallback
    if directives.is_empty() {
        return get_fallback_blade_directives();
    }

    directives
}

/// Convert CamelCase method name to lowercase directive name
fn camel_to_directive(name: &str) -> String {
    let mut result = String::new();
    for (i, c) in name.chars().enumerate() {
        if i > 0 && c.is_uppercase() {
            // Don't add underscore, just lowercase
        }
        result.push(c.to_ascii_lowercase());
    }
    result
}

/// Find if a directive has a closing directive
fn find_closing_directive(directive: &str, content: &str) -> Option<String> {
    // Look for compileEnd{Directive} method
    let pascal = directive
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default()
        + &directive[1..];

    let end_method = format!("compileEnd{}", pascal);
    if content.contains(&end_method) {
        return Some(format!("end{}", directive));
    }

    // Special cases
    match directive {
        "if" => Some("endif".into()),
        "unless" => Some("endunless".into()),
        "foreach" => Some("endforeach".into()),
        "forelse" => Some("endforelse".into()),
        "for" => Some("endfor".into()),
        "while" => Some("endwhile".into()),
        "switch" => Some("endswitch".into()),
        "section" => Some("endsection".into()),
        "push" => Some("endpush".into()),
        "prepend" => Some("endprepend".into()),
        "php" => Some("endphp".into()),
        "verbatim" => Some("endverbatim".into()),
        "component" => Some("endcomponent".into()),
        "slot" => Some("endslot".into()),
        "once" => Some("endonce".into()),
        "auth" => Some("endauth".into()),
        "guest" => Some("endguest".into()),
        "can" => Some("endcan".into()),
        "cannot" => Some("endcannot".into()),
        "canany" => Some("endcanany".into()),
        "env" => Some("endenv".into()),
        "production" => Some("endproduction".into()),
        "error" => Some("enderror".into()),
        "isset" => Some("endisset".into()),
        "empty" => Some("endempty".into()),
        "fragment" => Some("endfragment".into()),
        "session" => Some("endsession".into()),
        "persist" => Some("endpersist".into()),
        "teleport" => Some("endteleport".into()),
        _ => None,
    }
}

/// Generate a description for a directive based on its name and source file
fn generate_directive_description(_directive: &str, source_file: &str) -> String {
    // Map source files to categories
    let category = match source_file {
        "CompilesConditionals" => "Conditional",
        "CompilesLoops" => "Loop",
        "CompilesIncludes" => "Include",
        "CompilesComponents" => "Component",
        "CompilesLayouts" => "Layout",
        "CompilesStacks" => "Stack",
        "CompilesAuthorizations" => "Authorization",
        "CompilesErrors" => "Validation",
        "CompilesFragments" => "Fragment",
        "CompilesTranslations" => "Translation",
        "CompilesSessions" => "Session",
        "CompilesClasses" => "CSS Class",
        "CompilesStyles" => "Style",
        _ => "Blade",
    };

    format!("{} directive", category)
}

/// Scan for custom Blade directives registered via Blade::directive()
fn scan_custom_blade_directives(project_root: &Path) -> Vec<BladeDirectiveInfo> {
    use regex::Regex;

    let mut directives = Vec::new();

    // Regex to match Blade::directive('name', ...) or $blade->directive('name', ...)
    let directive_re =
        Regex::new(r#"(?:Blade::|->)directive\s*\(\s*['"]([a-zA-Z_][a-zA-Z0-9_]*)['"]"#).unwrap();

    // Scan app service providers
    let app_providers = project_root.join("app/Providers");
    if app_providers.exists() {
        scan_directory_for_directives(&app_providers, &directive_re, "app", &mut directives);
    }

    // Scan vendor packages for service providers
    let vendor_dir = project_root.join("vendor");
    if vendor_dir.exists() {
        // Look for common packages that register directives
        let packages_to_scan = [
            ("laravel/framework", "laravel"),
            ("livewire/livewire", "livewire"),
            ("laravel/pennant", "pennant"),
            ("spatie/laravel-permission", "spatie/permission"),
            ("spatie/laravel-html", "spatie/html"),
        ];

        for (package, source) in packages_to_scan {
            let package_path = vendor_dir.join(package);
            if package_path.exists() {
                // Look for service providers
                for entry in WalkDir::new(&package_path)
                    .into_iter()
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        e.path().extension().is_some_and(|ext| ext == "php")
                            && e.path().to_string_lossy().contains("ServiceProvider")
                    })
                {
                    if let Ok(content) = std::fs::read_to_string(entry.path()) {
                        for cap in directive_re.captures_iter(&content) {
                            if let Some(name) = cap.get(1) {
                                directives.push(BladeDirectiveInfo {
                                    name: name.as_str().to_string(),
                                    description: format!("Custom directive from {}", source),
                                    has_params: true, // Assume custom directives have params
                                    closing: None,    // Custom directives are typically inline
                                    source: source.to_string(),
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    directives
}

/// Helper to scan a directory for Blade::directive() calls
fn scan_directory_for_directives(
    dir: &Path,
    re: &regex::Regex,
    source: &str,
    directives: &mut Vec<BladeDirectiveInfo>,
) {
    for entry in WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "php"))
    {
        if let Ok(content) = std::fs::read_to_string(entry.path()) {
            for cap in re.captures_iter(&content) {
                if let Some(name) = cap.get(1) {
                    directives.push(BladeDirectiveInfo {
                        name: name.as_str().to_string(),
                        description: format!("Custom directive from {}", source),
                        has_params: true,
                        closing: None,
                        source: source.to_string(),
                    });
                }
            }
        }
    }
}

/// Get all Blade directives (Laravel built-in + custom)
fn get_all_blade_directives(project_root: &Path) -> Vec<BladeDirectiveInfo> {
    let mut all_directives = scan_laravel_blade_directives(project_root);
    let custom_directives = scan_custom_blade_directives(project_root);

    // Add custom directives, avoiding duplicates
    for custom in custom_directives {
        if !all_directives.iter().any(|d| d.name == custom.name) {
            all_directives.push(custom);
        }
    }

    // Sort by name for consistent ordering
    all_directives.sort_by(|a, b| a.name.cmp(&b.name));

    all_directives
}

/// Fallback list of core Blade directives when Laravel framework isn't found
fn get_fallback_blade_directives() -> Vec<BladeDirectiveInfo> {
    vec![
        BladeDirectiveInfo {
            name: "if".into(),
            description: "Conditional statement".into(),
            has_params: true,
            closing: Some("endif".into()),
            source: "laravel".into(),
        },
        BladeDirectiveInfo {
            name: "elseif".into(),
            description: "Else-if branch".into(),
            has_params: true,
            closing: None,
            source: "laravel".into(),
        },
        BladeDirectiveInfo {
            name: "else".into(),
            description: "Else branch".into(),
            has_params: false,
            closing: None,
            source: "laravel".into(),
        },
        BladeDirectiveInfo {
            name: "unless".into(),
            description: "Negative conditional".into(),
            has_params: true,
            closing: Some("endunless".into()),
            source: "laravel".into(),
        },
        BladeDirectiveInfo {
            name: "foreach".into(),
            description: "Loop through collection".into(),
            has_params: true,
            closing: Some("endforeach".into()),
            source: "laravel".into(),
        },
        BladeDirectiveInfo {
            name: "forelse".into(),
            description: "Loop with empty fallback".into(),
            has_params: true,
            closing: Some("endforelse".into()),
            source: "laravel".into(),
        },
        BladeDirectiveInfo {
            name: "for".into(),
            description: "For loop".into(),
            has_params: true,
            closing: Some("endfor".into()),
            source: "laravel".into(),
        },
        BladeDirectiveInfo {
            name: "while".into(),
            description: "While loop".into(),
            has_params: true,
            closing: Some("endwhile".into()),
            source: "laravel".into(),
        },
        BladeDirectiveInfo {
            name: "include".into(),
            description: "Include a view".into(),
            has_params: true,
            closing: None,
            source: "laravel".into(),
        },
        BladeDirectiveInfo {
            name: "extends".into(),
            description: "Extend a layout".into(),
            has_params: true,
            closing: None,
            source: "laravel".into(),
        },
        BladeDirectiveInfo {
            name: "section".into(),
            description: "Define section content".into(),
            has_params: true,
            closing: Some("endsection".into()),
            source: "laravel".into(),
        },
        BladeDirectiveInfo {
            name: "yield".into(),
            description: "Yield section content".into(),
            has_params: true,
            closing: None,
            source: "laravel".into(),
        },
        BladeDirectiveInfo {
            name: "csrf".into(),
            description: "CSRF token field".into(),
            has_params: false,
            closing: None,
            source: "laravel".into(),
        },
        BladeDirectiveInfo {
            name: "method".into(),
            description: "HTTP method field".into(),
            has_params: true,
            closing: None,
            source: "laravel".into(),
        },
        BladeDirectiveInfo {
            name: "auth".into(),
            description: "Authenticated user block".into(),
            has_params: true,
            closing: Some("endauth".into()),
            source: "laravel".into(),
        },
        BladeDirectiveInfo {
            name: "guest".into(),
            description: "Guest user block".into(),
            has_params: true,
            closing: Some("endguest".into()),
            source: "laravel".into(),
        },
        BladeDirectiveInfo {
            name: "php".into(),
            description: "Raw PHP block".into(),
            has_params: false,
            closing: Some("endphp".into()),
            source: "laravel".into(),
        },
        BladeDirectiveInfo {
            name: "vite".into(),
            description: "Vite asset".into(),
            has_params: true,
            closing: None,
            source: "laravel".into(),
        },
        BladeDirectiveInfo {
            name: "props".into(),
            description: "Component props".into(),
            has_params: true,
            closing: None,
            source: "laravel".into(),
        },
        BladeDirectiveInfo {
            name: "slot".into(),
            description: "Component slot".into(),
            has_params: true,
            closing: Some("endslot".into()),
            source: "laravel".into(),
        },
        BladeDirectiveInfo {
            name: "component".into(),
            description: "Render component".into(),
            has_params: true,
            closing: Some("endcomponent".into()),
            source: "laravel".into(),
        },
        BladeDirectiveInfo {
            name: "livewire".into(),
            description: "Livewire component".into(),
            has_params: true,
            closing: None,
            source: "livewire".into(),
        },
        BladeDirectiveInfo {
            name: "feature".into(),
            description: "Laravel Pennant feature flag".into(),
            has_params: true,
            closing: Some("endfeature".into()),
            source: "pennant".into(),
        },
    ]
}

/// Laravel's built-in validation rules
/// Reference: https://laravel.com/docs/12.x/validation#available-validation-rules
fn get_laravel_validation_rules() -> Vec<ValidationRuleInfo> {
    vec![
        // Boolean/Acceptance Rules
        ValidationRuleInfo {
            name: "accepted".into(),
            description: "Must be yes, on, 1, or true".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "accepted_if".into(),
            description: "Accepted if another field equals value".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "boolean".into(),
            description: "Must be boolean (true, false, 1, 0)".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "declined".into(),
            description: "Must be no, off, 0, or false".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "declined_if".into(),
            description: "Declined if another field equals value".into(),
            has_params: true,
            source: "laravel".into(),
        },
        // String Rules
        ValidationRuleInfo {
            name: "active_url".into(),
            description: "Valid URL with DNS record".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "alpha".into(),
            description: "Only alphabetic characters".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "alpha_dash".into(),
            description: "Alphanumeric, dashes, underscores".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "alpha_num".into(),
            description: "Only alphanumeric characters".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "ascii".into(),
            description: "Only 7-bit ASCII characters".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "confirmed".into(),
            description: "Must have matching _confirmation field".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "current_password".into(),
            description: "Must match user's password".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "different".into(),
            description: "Must differ from another field".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "doesnt_start_with".into(),
            description: "Must not start with given values".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "doesnt_end_with".into(),
            description: "Must not end with given values".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "email".into(),
            description: "Valid email address".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "ends_with".into(),
            description: "Must end with given values".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "enum".into(),
            description: "Valid backed enum value".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "hex_color".into(),
            description: "Valid hexadecimal color".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "in".into(),
            description: "Must be in given list".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "ip".into(),
            description: "Valid IP address".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "ipv4".into(),
            description: "Valid IPv4 address".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "ipv6".into(),
            description: "Valid IPv6 address".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "json".into(),
            description: "Valid JSON string".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "lowercase".into(),
            description: "Must be lowercase".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "mac_address".into(),
            description: "Valid MAC address".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "not_in".into(),
            description: "Must not be in given list".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "not_regex".into(),
            description: "Must not match regex".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "regex".into(),
            description: "Must match regex pattern".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "same".into(),
            description: "Must match another field".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "starts_with".into(),
            description: "Must start with given values".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "string".into(),
            description: "Must be a string".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "uppercase".into(),
            description: "Must be uppercase".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "url".into(),
            description: "Valid URL".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "ulid".into(),
            description: "Valid ULID".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "uuid".into(),
            description: "Valid UUID".into(),
            has_params: false,
            source: "laravel".into(),
        },
        // Numeric Rules
        ValidationRuleInfo {
            name: "between".into(),
            description: "Value between min and max".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "decimal".into(),
            description: "Numeric with decimal places".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "digits".into(),
            description: "Exact number of digits".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "digits_between".into(),
            description: "Digits between min and max".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "gt".into(),
            description: "Greater than field/value".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "gte".into(),
            description: "Greater than or equal".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "integer".into(),
            description: "Must be an integer".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "lt".into(),
            description: "Less than field/value".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "lte".into(),
            description: "Less than or equal".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "max".into(),
            description: "Maximum value/length/size".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "max_digits".into(),
            description: "Maximum number of digits".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "min".into(),
            description: "Minimum value/length/size".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "min_digits".into(),
            description: "Minimum number of digits".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "multiple_of".into(),
            description: "Must be multiple of value".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "numeric".into(),
            description: "Must be numeric".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "size".into(),
            description: "Exact size/length".into(),
            has_params: true,
            source: "laravel".into(),
        },
        // Array Rules
        ValidationRuleInfo {
            name: "array".into(),
            description: "Must be an array".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "contains".into(),
            description: "Array must contain values".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "doesnt_contain".into(),
            description: "Array must not contain values".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "distinct".into(),
            description: "Array values must be unique".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "in_array".into(),
            description: "Must exist in another array field".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "list".into(),
            description: "Array with consecutive keys".into(),
            has_params: false,
            source: "laravel".into(),
        },
        // Date Rules
        ValidationRuleInfo {
            name: "after".into(),
            description: "Date after given date".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "after_or_equal".into(),
            description: "Date after or equal".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "before".into(),
            description: "Date before given date".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "before_or_equal".into(),
            description: "Date before or equal".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "date".into(),
            description: "Valid date".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "date_equals".into(),
            description: "Date equals given date".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "date_format".into(),
            description: "Matches date format".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "timezone".into(),
            description: "Valid timezone".into(),
            has_params: false,
            source: "laravel".into(),
        },
        // File Rules
        ValidationRuleInfo {
            name: "dimensions".into(),
            description: "Image dimension constraints".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "extensions".into(),
            description: "File extension".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "file".into(),
            description: "Successfully uploaded file".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "image".into(),
            description: "Image file".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "mimetypes".into(),
            description: "MIME type".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "mimes".into(),
            description: "File extension/MIME".into(),
            has_params: true,
            source: "laravel".into(),
        },
        // Database Rules
        ValidationRuleInfo {
            name: "exists".into(),
            description: "Record exists in database".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "unique".into(),
            description: "Unique in database table".into(),
            has_params: true,
            source: "laravel".into(),
        },
        // Presence/Required Rules
        ValidationRuleInfo {
            name: "bail".into(),
            description: "Stop on first failure".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "exclude".into(),
            description: "Exclude from validated data".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "exclude_if".into(),
            description: "Exclude if field equals value".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "exclude_unless".into(),
            description: "Exclude unless field equals".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "exclude_with".into(),
            description: "Exclude if field present".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "exclude_without".into(),
            description: "Exclude if field absent".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "filled".into(),
            description: "Not empty when present".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "missing".into(),
            description: "Must not be present".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "missing_if".into(),
            description: "Missing if field equals".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "missing_unless".into(),
            description: "Missing unless field equals".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "missing_with".into(),
            description: "Missing if field present".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "missing_with_all".into(),
            description: "Missing if all fields present".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "nullable".into(),
            description: "May be null".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "present".into(),
            description: "Must be present".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "present_if".into(),
            description: "Present if field equals".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "present_unless".into(),
            description: "Present unless field equals".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "present_with".into(),
            description: "Present if field present".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "present_with_all".into(),
            description: "Present if all fields present".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "prohibited".into(),
            description: "Must not be present".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "prohibited_if".into(),
            description: "Prohibited if field equals".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "prohibited_unless".into(),
            description: "Prohibited unless field equals".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "prohibits".into(),
            description: "Prohibits other fields".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "required".into(),
            description: "Field is required".into(),
            has_params: false,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "required_if".into(),
            description: "Required if field equals".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "required_if_accepted".into(),
            description: "Required if field accepted".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "required_if_declined".into(),
            description: "Required if field declined".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "required_unless".into(),
            description: "Required unless field equals".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "required_with".into(),
            description: "Required if field present".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "required_with_all".into(),
            description: "Required if all fields present".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "required_without".into(),
            description: "Required if field absent".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "required_without_all".into(),
            description: "Required if all fields absent".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "required_array_keys".into(),
            description: "Array must have keys".into(),
            has_params: true,
            source: "laravel".into(),
        },
        ValidationRuleInfo {
            name: "sometimes".into(),
            description: "Validate only when present".into(),
            has_params: false,
            source: "laravel".into(),
        },
    ]
}

/// The main Laravel Language Server struct
/// This holds all the state for our LSP
#[derive(Clone)]
struct LaravelLanguageServer {
    /// LSP client for sending messages to the editor
    client: Client,
    /// Store document contents and versions for analysis (content, version)
    documents: Arc<RwLock<HashMap<Url, (String, i32)>>>,
    /// The root path of the Laravel project
    root_path: Arc<RwLock<Option<PathBuf>>>,
    /// Store diagnostics per file (for hover filtering)
    diagnostics: Arc<RwLock<HashMap<Url, Vec<Diagnostic>>>>,
    /// Pending debounced diagnostic tasks (uri -> task handle)
    pending_diagnostics: Arc<RwLock<HashMap<Url, tokio::task::JoinHandle<()>>>>,
    /// Debounce delay for diagnostics in milliseconds (default: 200ms)
    debounce_delay_ms: u64,
    /// Salsa 0.25 database handle (runs on dedicated thread via actor pattern)
    salsa: SalsaHandle,
    /// Smart cache manager for middleware/bindings (mtime-based invalidation)
    cache: Arc<RwLock<Option<CacheManager>>>,
    /// Pending background rescans (debounced)
    pending_rescans: Arc<RwLock<HashSet<RescanType>>>,
    /// Handle for the rescan debounce timer
    rescan_debounce_handle: Arc<RwLock<Option<tokio::task::JoinHandle<()>>>>,
    /// File existence cache with TTL (path -> (exists, cached_at))
    /// This avoids blocking I/O in async context for file_exists checks
    file_exists_cache: Arc<RwLock<HashMap<PathBuf, (bool, Instant)>>>,
    /// Cached Laravel config to avoid repeated Salsa lookups
    cached_config: Arc<RwLock<Option<LaravelConfigData>>>,
    /// Cached Livewire config + version, keyed by the root path they were
    /// loaded for. Parsing `config/livewire.php` and scanning
    /// `composer.lock` on every diagnostic / hover / goto would be wasteful;
    /// this caches the parsed result across calls. Invalidated by
    /// `invalidate_config_cache` along with the other config state.
    cached_livewire: Arc<
        RwLock<
            Option<(
                PathBuf,
                laravel_lsp::livewire_config::LivewireConfig,
                laravel_lsp::livewire_version::LivewireVersion,
            )>,
        >,
    >,
    /// Track last goto_definition request per file for coalescing rapid requests
    /// Maps URI to (position, timestamp) - skip duplicate requests within coalesce window
    last_goto_request: Arc<RwLock<HashMap<Url, (Position, Instant)>>>,
    /// Track which root we've fully initialized for (to avoid re-initialization on file open)
    initialized_root: Arc<RwLock<Option<PathBuf>>>,
    /// Pending debounced Salsa updates per file (uri -> task handle)
    /// Used to debounce did_change events before updating Salsa
    pending_salsa_updates: Arc<RwLock<HashMap<Url, tokio::task::JoinHandle<()>>>>,
    /// Configurable debounce delay for autocomplete updates in milliseconds (default: 200ms)
    /// Can be configured via LSP settings: { "autoCompleteDebounce": 200 }
    auto_complete_debounce_ms: Arc<RwLock<u64>>,
    /// Add space between directive name and parentheses in completions
    /// false: @if($condition)  |  true: @if ($condition)
    directive_spacing: Arc<RwLock<bool>>,
    /// Whether we've shown the vendor missing diagnostic this session
    vendor_diagnostic_shown: Arc<RwLock<bool>>,
    /// Cached validation rule names (parsed from Laravel framework at startup)
    cached_validation_rule_names: Arc<RwLock<Vec<String>>>,
    /// Database schema provider for exists:/unique: validation rules
    database_schema: Arc<RwLock<Option<laravel_lsp::database::DatabaseSchemaProvider>>>,
    /// Whether we've shown the database connection error diagnostic this session
    database_diagnostic_shown: Arc<RwLock<bool>>,
    /// Cached index of named routes discovered across project / packages / framework.
    /// Populated at init by walking routes/, vendor/*/routes/, and content-matched
    /// vendor PHP files. Replaces the legacy hard-coded route-file scan.
    route_index: Arc<RwLock<Option<RouteIndex>>>,

    /// Cached map of translation `namespace → absolute lang directory` for
    /// unpublished vendor packages. Lazily populated on the first translation
    /// hover that needs it; subsequent hovers reuse the cached map. See
    /// [`laravel_lsp::vendor_translations`] for the scan implementation.
    /// `None` means "not yet scanned"; `Some(map)` means "scanned, here's
    /// what we found" (the map can be empty if no packages register).
    /// Wrapped in `Arc` so clones share memory across hover calls.
    vendor_translation_namespaces: Arc<RwLock<Option<Arc<HashMap<String, PathBuf>>>>>,

    /// Per-route-file cache of [`route_name_locator`] output, keyed by
    /// mtime. Find-references on a route runs route_name_locator on every
    /// file in `routes/` — without this cache, every invocation re-parses
    /// the routes/ directory from scratch. The cache turns subsequent
    /// invocations into a stat + HashMap lookup.
    route_decl_cache: Arc<
        RwLock<
            HashMap<
                PathBuf,
                (
                    std::time::SystemTime,
                    Arc<Vec<laravel_lsp::route_name_locator::RouteNameDeclaration>>,
                ),
            >,
        >,
    >,
}

/// Default Salsa debounce delay in milliseconds
const DEFAULT_SALSA_DEBOUNCE_MS: u64 = 200;

// NOTE: Blade directives are now dynamically discovered via get_all_blade_directives()
// which scans the Laravel framework, app service providers, and packages.

/// Blade-specific settings
/// Configured via: { "lsp": { "laravel-lsp": { "settings": { "blade": { ... } } } } }
#[derive(Debug, Clone, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct BladeSettings {
    /// Add space between directive name and parentheses (default: false)
    /// false: @if($condition)
    /// true:  @if ($condition)
    #[serde(default)]
    directive_spacing: bool,
}

fn default_auto_complete_debounce() -> u64 {
    DEFAULT_SALSA_DEBOUNCE_MS
}

/// LSP settings object from Zed
/// Configured via: { "lsp": { "laravel-lsp": { "settings": { ... } } } }
#[derive(Debug, Clone, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LspSettings {
    /// Debounce delay for autocomplete updates in milliseconds (default: 200)
    /// Lower values = faster updates but more CPU usage during typing
    /// Higher values = less CPU but slower feedback
    #[serde(default = "default_auto_complete_debounce")]
    auto_complete_debounce: u64,
    #[serde(default)]
    blade: BladeSettings,
}

// ============================================================================
// Code Action types
// ============================================================================

/// Types of files that can be created via code actions
#[derive(Debug, Clone)]
enum FileActionType {
    View,
    /// Anonymous Blade component (view only)
    BladeComponent,
    /// Blade component with both view and PHP class
    BladeComponentWithClass,
    Livewire,
    Middleware,
    /// PHP translation file (e.g., lang/en/messages.php)
    TranslationPhp,
    /// JSON translation file (e.g., lang/en.json)
    TranslationJson,
    /// PHP config file (e.g., config/app.php)
    ConfigPhp,
    /// Environment variable in .env file
    EnvVar,
    /// Laravel Pennant feature class
    Feature,
}

/// Represents a file creation action parsed from a diagnostic
#[derive(Debug, Clone)]
struct FileAction {
    action_type: FileActionType,
    name: String,
    target_path: PathBuf,
    /// Whether the target file already exists (relevant for translations/config/env)
    file_exists: bool,
    /// Path to copy from (for .env.example → .env)
    copy_from: Option<PathBuf>,
}

impl FileAction {
    /// Parse a diagnostic message into FileAction(s)
    /// Returns a Vec because some diagnostics (like "Blade component not found")
    /// can offer multiple actions (create view only OR create view with class)
    fn from_diagnostic(message: &str) -> Vec<Self> {
        let target_path = match LaravelLanguageServer::extract_expected_path(message) {
            Some(path) => path,
            None => return Vec::new(),
        };

        if message.starts_with("View file not found") {
            vec![FileAction {
                action_type: FileActionType::View,
                name: LaravelLanguageServer::extract_name_from_diagnostic(
                    message,
                    "View file not found: '",
                    "'",
                )
                .unwrap_or("view")
                .to_string(),
                target_path: PathBuf::from(target_path),
                file_exists: false,
                copy_from: None,
            }]
        } else if message.starts_with("Blade component not found") {
            // Offer two options: create view only OR create view with class
            let name = LaravelLanguageServer::extract_name_from_diagnostic(
                message,
                "Blade component not found: '",
                "'",
            )
            .unwrap_or("component")
            .to_string();
            let path = PathBuf::from(target_path);

            vec![
                // Option 1: Create anonymous component (view only)
                FileAction {
                    action_type: FileActionType::BladeComponent,
                    name: name.clone(),
                    target_path: path.clone(),
                    file_exists: false,
                    copy_from: None,
                },
                // Option 2: Create component with PHP class
                FileAction {
                    action_type: FileActionType::BladeComponentWithClass,
                    name,
                    target_path: path,
                    file_exists: false,
                    copy_from: None,
                },
            ]
        } else if message.starts_with("Livewire component not found") {
            vec![FileAction {
                action_type: FileActionType::Livewire,
                name: LaravelLanguageServer::extract_name_from_diagnostic(
                    message,
                    "Livewire component not found: '",
                    "'",
                )
                .unwrap_or("component")
                .to_string(),
                target_path: PathBuf::from(target_path),
                file_exists: false,
                copy_from: None,
            }]
        } else if message.starts_with("Middleware") && message.contains("not found") {
            vec![FileAction {
                action_type: FileActionType::Middleware,
                name: LaravelLanguageServer::extract_name_from_diagnostic(
                    message,
                    "Middleware '",
                    "'",
                )
                .unwrap_or("middleware")
                .to_string(),
                target_path: PathBuf::from(target_path),
                file_exists: false,
                copy_from: None,
            }]
        } else if message.starts_with("Translation not found") {
            // Extract the translation key from the message
            let key = LaravelLanguageServer::extract_name_from_diagnostic(
                message,
                "Translation not found: '",
                "'",
            )
            .unwrap_or("key")
            .to_string();

            // Determine if it's PHP or JSON based on the file extension
            let path = PathBuf::from(target_path);
            let is_php = path.extension().map(|e| e == "php").unwrap_or(false);

            // Check if the file exists from the message
            let file_exists = message.contains("not found in file");

            vec![FileAction {
                action_type: if is_php {
                    FileActionType::TranslationPhp
                } else {
                    FileActionType::TranslationJson
                },
                name: key,
                target_path: path,
                file_exists,
                copy_from: None,
            }]
        } else if message.starts_with("Config not found") {
            // Extract the config key from the message
            let key = LaravelLanguageServer::extract_name_from_diagnostic(
                message,
                "Config not found: '",
                "'",
            )
            .unwrap_or("key")
            .to_string();

            let path = PathBuf::from(target_path);

            // Check if the file exists from the message
            let file_exists = message.contains("not found in file");

            vec![FileAction {
                action_type: FileActionType::ConfigPhp,
                name: key,
                target_path: path,
                file_exists,
                copy_from: None,
            }]
        } else if message.starts_with("Environment variable") {
            // Extract the env var name from the message
            let name = LaravelLanguageServer::extract_name_from_diagnostic(
                message,
                "Environment variable '",
                "'",
            )
            .unwrap_or("VAR")
            .to_string();

            let path = PathBuf::from(target_path);

            // Check if .env exists (from message "not found in file" vs "file not found")
            // "not found in file" → file exists, var is missing
            // "file not found" → file doesn't exist
            let file_exists = message.contains("not found in file");

            // Check if there's a "Copy from:" line for .env.example
            let copy_from = LaravelLanguageServer::extract_copy_from_path(message);

            vec![FileAction {
                action_type: FileActionType::EnvVar,
                name,
                target_path: path,
                file_exists,
                copy_from,
            }]
        } else if message.starts_with("Feature not found")
            || message.starts_with("Feature class not found")
        {
            // Extract the feature name from the message
            let name = LaravelLanguageServer::extract_name_from_diagnostic(
                message,
                "Feature not found: '",
                "'",
            )
            .or_else(|| {
                LaravelLanguageServer::extract_name_from_diagnostic(
                    message,
                    "Feature class not found: '",
                    "'",
                )
            })
            .unwrap_or("feature")
            .to_string();

            vec![FileAction {
                action_type: FileActionType::Feature,
                name,
                target_path: PathBuf::from(target_path),
                file_exists: false,
                copy_from: None,
            }]
        } else {
            Vec::new()
        }
    }

    /// Get the title for the code action
    fn title(&self) -> String {
        match self.action_type {
            FileActionType::View => format!("Create view: {}", self.name),
            FileActionType::BladeComponent => format!("Create component: {}", self.name),
            FileActionType::BladeComponentWithClass => {
                format!("Create component with class: {}", self.name)
            }
            FileActionType::Livewire => format!("Create Livewire: {}", self.name),
            FileActionType::Middleware => format!("Create middleware: {}", self.name),
            FileActionType::TranslationPhp | FileActionType::TranslationJson => {
                if self.file_exists {
                    format!("Add translation: {}", self.name)
                } else {
                    format!("Create translation: {}", self.name)
                }
            }
            FileActionType::ConfigPhp => {
                if self.file_exists {
                    format!("Add config: {}", self.name)
                } else {
                    format!("Create config: {}", self.name)
                }
            }
            FileActionType::EnvVar => {
                if self.copy_from.is_some() {
                    "Copy .env.example to .env".to_string()
                } else if self.file_exists {
                    format!("Add env var: {}", self.name)
                } else {
                    format!("Create .env with {}", self.name)
                }
            }
            FileActionType::Feature => {
                // Convert the feature key to PascalCase for the class name
                let class_name = feature_key_to_class_name(&self.name);
                format!("Create feature class: {}", class_name)
            }
        }
    }

    /// Get the Blade component PHP class path
    /// e.g., "button" -> "app/View/Components/Button.php"
    /// e.g., "forms.input" -> "app/View/Components/Forms/Input.php"
    fn get_component_class_path(&self, root: &Path) -> PathBuf {
        let parts: Vec<&str> = self.name.split('.').collect();
        let mut path = root.join("app/View/Components");

        for (i, part) in parts.iter().enumerate() {
            let pascal = Self::kebab_to_pascal_case_static(part);
            if i == parts.len() - 1 {
                path.push(format!("{}.php", pascal));
            } else {
                path.push(pascal);
            }
        }
        path
    }

    /// Convert kebab-case to PascalCase (static version for use in FileAction)
    fn kebab_to_pascal_case_static(s: &str) -> String {
        s.split('-')
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    None => String::new(),
                    Some(first) => first.to_uppercase().chain(chars).collect(),
                }
            })
            .collect()
    }

    /// Get the Blade component PHP class template
    fn get_component_class_template(&self) -> String {
        // For nested components like "forms.input":
        // - Class name: last segment in PascalCase ("Input")
        // - Namespace: App\View\Components + intermediate segments ("App\View\Components\Forms")
        let parts: Vec<&str> = self.name.split('.').collect();
        let class_name =
            Self::kebab_to_pascal_case_static(parts.last().unwrap_or(&self.name.as_str()));

        let namespace = if parts.len() > 1 {
            let namespace_parts: Vec<String> = parts[..parts.len() - 1]
                .iter()
                .map(|p| Self::kebab_to_pascal_case_static(p))
                .collect();
            format!("App\\View\\Components\\{}", namespace_parts.join("\\"))
        } else {
            "App\\View\\Components".to_string()
        };

        // View name for the render method (keeps original format with dots)
        let view_name = &self.name;

        format!(
            r#"<?php

namespace {};

use Closure;
use Illuminate\Contracts\View\View;
use Illuminate\View\Component;

class {} extends Component
{{
    /**
     * Create a new component instance.
     */
    public function __construct()
    {{
        //
    }}

    /**
     * Get the view / contents that represent the component.
     */
    public function render(): View|Closure|string
    {{
        return view('components.{}');
    }}
}}
"#,
            namespace, class_name, view_name
        )
    }

    /// Get the Livewire Blade view path for a component
    /// e.g., "counter" -> "resources/views/livewire/counter.blade.php"
    /// e.g., "admin.dashboard" -> "resources/views/livewire/admin/dashboard.blade.php"
    fn get_livewire_view_path(&self, root: &Path) -> PathBuf {
        // Convert dots to path separators, keep kebab-case
        let view_path = self.name.replace('.', "/");
        root.join("resources/views/livewire")
            .join(format!("{}.blade.php", view_path))
    }

    /// Get the Livewire Blade view template content
    fn get_livewire_view_template() -> String {
        "<div>\n    {{-- Component content --}}\n</div>\n".to_string()
    }

    /// Build a CodeAction that creates a file with the given content
    fn build_code_action(
        &self,
        template: String,
        diagnostic: &Diagnostic,
        root: Option<&Path>,
    ) -> Option<CodeActionOrCommand> {
        let file_uri = Url::from_file_path(&self.target_path).ok()?;

        // Handle different action types
        let workspace_edit = if let FileActionType::Livewire = self.action_type {
            // Livewire creates TWO files: PHP class and Blade view
            let root = root?;
            let view_path = self.get_livewire_view_path(root);
            let view_uri = Url::from_file_path(&view_path).ok()?;
            let view_template = Self::get_livewire_view_template();

            WorkspaceEdit {
                changes: None,
                document_changes: Some(DocumentChanges::Operations(vec![
                    // Create PHP class file
                    DocumentChangeOperation::Op(ResourceOp::Create(CreateFile {
                        uri: file_uri.clone(),
                        options: Some(CreateFileOptions {
                            overwrite: Some(false),
                            ignore_if_exists: Some(true),
                        }),
                        annotation_id: None,
                    })),
                    DocumentChangeOperation::Edit(TextDocumentEdit {
                        text_document: OptionalVersionedTextDocumentIdentifier {
                            uri: file_uri,
                            version: None,
                        },
                        edits: vec![OneOf::Left(TextEdit {
                            range: Range {
                                start: Position {
                                    line: 0,
                                    character: 0,
                                },
                                end: Position {
                                    line: 0,
                                    character: 0,
                                },
                            },
                            new_text: template,
                        })],
                    }),
                    // Create Blade view file
                    DocumentChangeOperation::Op(ResourceOp::Create(CreateFile {
                        uri: view_uri.clone(),
                        options: Some(CreateFileOptions {
                            overwrite: Some(false),
                            ignore_if_exists: Some(true),
                        }),
                        annotation_id: None,
                    })),
                    DocumentChangeOperation::Edit(TextDocumentEdit {
                        text_document: OptionalVersionedTextDocumentIdentifier {
                            uri: view_uri,
                            version: None,
                        },
                        edits: vec![OneOf::Left(TextEdit {
                            range: Range {
                                start: Position {
                                    line: 0,
                                    character: 0,
                                },
                                end: Position {
                                    line: 0,
                                    character: 0,
                                },
                            },
                            new_text: view_template,
                        })],
                    }),
                ])),
                change_annotations: None,
            }
        } else if let FileActionType::BladeComponentWithClass = self.action_type {
            // Create both the Blade view and the PHP class
            let root = root?;
            let class_path = self.get_component_class_path(root);
            let class_uri = Url::from_file_path(&class_path).ok()?;
            let class_template = self.get_component_class_template();
            let view_template = "@props([])\n\n<div>\n    {{ $slot }}\n</div>\n".to_string();

            WorkspaceEdit {
                changes: None,
                document_changes: Some(DocumentChanges::Operations(vec![
                    // Create Blade view file (target_path is the view)
                    DocumentChangeOperation::Op(ResourceOp::Create(CreateFile {
                        uri: file_uri.clone(),
                        options: Some(CreateFileOptions {
                            overwrite: Some(false),
                            ignore_if_exists: Some(true),
                        }),
                        annotation_id: None,
                    })),
                    DocumentChangeOperation::Edit(TextDocumentEdit {
                        text_document: OptionalVersionedTextDocumentIdentifier {
                            uri: file_uri,
                            version: None,
                        },
                        edits: vec![OneOf::Left(TextEdit {
                            range: Range {
                                start: Position {
                                    line: 0,
                                    character: 0,
                                },
                                end: Position {
                                    line: 0,
                                    character: 0,
                                },
                            },
                            new_text: view_template,
                        })],
                    }),
                    // Create PHP class file
                    DocumentChangeOperation::Op(ResourceOp::Create(CreateFile {
                        uri: class_uri.clone(),
                        options: Some(CreateFileOptions {
                            overwrite: Some(false),
                            ignore_if_exists: Some(true),
                        }),
                        annotation_id: None,
                    })),
                    DocumentChangeOperation::Edit(TextDocumentEdit {
                        text_document: OptionalVersionedTextDocumentIdentifier {
                            uri: class_uri,
                            version: None,
                        },
                        edits: vec![OneOf::Left(TextEdit {
                            range: Range {
                                start: Position {
                                    line: 0,
                                    character: 0,
                                },
                                end: Position {
                                    line: 0,
                                    character: 0,
                                },
                            },
                            new_text: class_template,
                        })],
                    }),
                ])),
                change_annotations: None,
            }
        } else if let FileActionType::EnvVar = self.action_type {
            // EnvVar has special handling
            if let Some(copy_from) = &self.copy_from {
                // Copy .env.example to .env
                self.build_copy_file_edit(copy_from, &file_uri)?
            } else if self.file_exists {
                // Append env var to existing .env
                self.build_key_insert_edit(&file_uri)?
            } else {
                // Create new .env with just this var
                WorkspaceEdit {
                    changes: None,
                    document_changes: Some(DocumentChanges::Operations(vec![
                        DocumentChangeOperation::Op(ResourceOp::Create(CreateFile {
                            uri: file_uri.clone(),
                            options: Some(CreateFileOptions {
                                overwrite: Some(false),
                                ignore_if_exists: Some(true),
                            }),
                            annotation_id: None,
                        })),
                        DocumentChangeOperation::Edit(TextDocumentEdit {
                            text_document: OptionalVersionedTextDocumentIdentifier {
                                uri: file_uri,
                                version: None,
                            },
                            edits: vec![OneOf::Left(TextEdit {
                                range: Range {
                                    start: Position {
                                        line: 0,
                                        character: 0,
                                    },
                                    end: Position {
                                        line: 0,
                                        character: 0,
                                    },
                                },
                                new_text: template,
                            })],
                        }),
                    ])),
                    change_annotations: None,
                }
            }
        } else if self.file_exists
            && matches!(
                self.action_type,
                FileActionType::TranslationPhp
                    | FileActionType::TranslationJson
                    | FileActionType::ConfigPhp
            )
        {
            // For translations/config with existing files, we insert rather than create
            self.build_key_insert_edit(&file_uri)?
        } else {
            // Standard file creation
            WorkspaceEdit {
                changes: None,
                document_changes: Some(DocumentChanges::Operations(vec![
                    // Step 1: Create the file
                    DocumentChangeOperation::Op(ResourceOp::Create(CreateFile {
                        uri: file_uri.clone(),
                        options: Some(CreateFileOptions {
                            overwrite: Some(false),
                            ignore_if_exists: Some(true),
                        }),
                        annotation_id: None,
                    })),
                    // Step 2: Insert content into the new file
                    DocumentChangeOperation::Edit(TextDocumentEdit {
                        text_document: OptionalVersionedTextDocumentIdentifier {
                            uri: file_uri,
                            version: None,
                        },
                        edits: vec![OneOf::Left(TextEdit {
                            range: Range {
                                start: Position {
                                    line: 0,
                                    character: 0,
                                },
                                end: Position {
                                    line: 0,
                                    character: 0,
                                },
                            },
                            new_text: template,
                        })],
                    }),
                ])),
                change_annotations: None,
            }
        };

        let code_action = CodeAction {
            title: self.title(),
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: Some(vec![diagnostic.clone()]),
            edit: Some(workspace_edit),
            command: None,
            is_preferred: Some(true),
            disabled: None,
            data: None,
        };

        Some(CodeActionOrCommand::CodeAction(code_action))
    }

    /// Build a WorkspaceEdit to insert a key into an existing file (translations or config)
    fn build_key_insert_edit(&self, file_uri: &Url) -> Option<WorkspaceEdit> {
        // Read the existing file content
        let content = std::fs::read_to_string(&self.target_path).ok()?;
        let lines: Vec<&str> = content.lines().collect();

        // For the key, we need to extract just the last part for dotted keys
        // e.g., "messages.welcome" → "welcome" for PHP, but full key for JSON
        let key_for_insert = match self.action_type {
            FileActionType::TranslationPhp | FileActionType::ConfigPhp => {
                // For PHP files, use the nested key (last part after the dot)
                self.name.split('.').next_back().unwrap_or(&self.name)
            }
            _ => &self.name,
        };

        // Find insertion point and create the edit
        let (insert_line, insert_char, new_text) = match self.action_type {
            FileActionType::TranslationJson => {
                // Find the last line with content before the closing }
                // Insert: "key": "key",
                let mut insert_line = 0;
                let mut found_closing = false;

                for (i, line) in lines.iter().enumerate().rev() {
                    let trimmed = line.trim();
                    if trimmed == "}" {
                        found_closing = true;
                        insert_line = i;
                    } else if found_closing && !trimmed.is_empty() {
                        // Found a line with content before the closing brace
                        // We need to add a comma to this line if it doesn't have one
                        break;
                    }
                }

                if !found_closing {
                    return None;
                }

                // Insert before the closing brace with proper indentation
                let indent = "    ";
                let escaped_key = key_for_insert.replace('\\', "\\\\").replace('"', "\\\"");
                (
                    insert_line as u32,
                    0,
                    format!("{}\"{}\": \"{}\",\n", indent, escaped_key, escaped_key),
                )
            }
            FileActionType::TranslationPhp => {
                // Find the last line with ]; and insert before it
                // Insert: 'key' => 'key',
                let mut insert_line = 0;

                for (i, line) in lines.iter().enumerate().rev() {
                    if line.trim().starts_with("];") || line.trim() == "];" {
                        insert_line = i;
                        break;
                    }
                }

                // Insert before the closing bracket with proper indentation
                let indent = "    ";
                let escaped_key = key_for_insert.replace('\\', "\\\\").replace('\'', "\\'");
                (
                    insert_line as u32,
                    0,
                    format!("{}'{}' => '{}',\n", indent, escaped_key, escaped_key),
                )
            }
            FileActionType::ConfigPhp => {
                // Find the last line with ]; and insert before it
                // Insert: 'key' => '', (empty string value for config)
                let mut insert_line = 0;

                for (i, line) in lines.iter().enumerate().rev() {
                    if line.trim().starts_with("];") || line.trim() == "];" {
                        insert_line = i;
                        break;
                    }
                }

                // Insert before the closing bracket with proper indentation
                let indent = "    ";
                let escaped_key = key_for_insert.replace('\\', "\\\\").replace('\'', "\\'");
                (
                    insert_line as u32,
                    0,
                    format!("{}'{}' => '',\n", indent, escaped_key),
                )
            }
            FileActionType::EnvVar => {
                // Append: KEY=\n at end of file (with newline before if file doesn't end with one)
                let line_count = lines.len();
                let needs_newline = !content.ends_with('\n') && !content.is_empty();

                // Insert at end of file
                let insert_line = line_count;
                let prefix = if needs_newline { "\n" } else { "" };
                (insert_line as u32, 0, format!("{}{}=\n", prefix, self.name))
            }
            _ => return None,
        };

        Some(WorkspaceEdit {
            changes: None,
            document_changes: Some(DocumentChanges::Operations(vec![
                DocumentChangeOperation::Edit(TextDocumentEdit {
                    text_document: OptionalVersionedTextDocumentIdentifier {
                        uri: file_uri.clone(),
                        version: None,
                    },
                    edits: vec![OneOf::Left(TextEdit {
                        range: Range {
                            start: Position {
                                line: insert_line,
                                character: insert_char,
                            },
                            end: Position {
                                line: insert_line,
                                character: insert_char,
                            },
                        },
                        new_text,
                    })],
                }),
            ])),
            change_annotations: None,
        })
    }

    /// Build a WorkspaceEdit that copies a source file to the target (for .env.example → .env)
    fn build_copy_file_edit(&self, source: &Path, target_uri: &Url) -> Option<WorkspaceEdit> {
        // Read the source file content
        let content = std::fs::read_to_string(source).ok()?;

        Some(WorkspaceEdit {
            changes: None,
            document_changes: Some(DocumentChanges::Operations(vec![
                // Create the target file
                DocumentChangeOperation::Op(ResourceOp::Create(CreateFile {
                    uri: target_uri.clone(),
                    options: Some(CreateFileOptions {
                        overwrite: Some(false),
                        ignore_if_exists: Some(true),
                    }),
                    annotation_id: None,
                })),
                // Insert the copied content
                DocumentChangeOperation::Edit(TextDocumentEdit {
                    text_document: OptionalVersionedTextDocumentIdentifier {
                        uri: target_uri.clone(),
                        version: None,
                    },
                    edits: vec![OneOf::Left(TextEdit {
                        range: Range {
                            start: Position {
                                line: 0,
                                character: 0,
                            },
                            end: Position {
                                line: 0,
                                character: 0,
                            },
                        },
                        new_text: content,
                    })],
                }),
            ])),
            change_annotations: None,
        })
    }
}

/// Represents a variable property access in Blade content
/// e.g., $user->name, $post->title
struct VariableAccess {
    variable_name: String,
    line: u32,
    column: u32,
    end_column: u32,
}

/// Represents an available variable in a Blade file
/// Used for $variable name autocompletion
struct BladeVariableInfo {
    name: String,
    php_type: String,
    source: String, // "props", "controller", "component", "livewire", "framework", "loop"
}

// Blade loop-block types live in the library crate so they can be returned
// from Salsa tracked queries (see `laravel_lsp::blade_loops`).
use laravel_lsp::blade_loops::BladeLoopType;

/// Context information for string-based completions (config, view, route, etc.)
/// Contains position info needed to create proper text_edit ranges
#[derive(Debug, Clone)]
struct StringContext {
    /// The text typed so far inside the string (for filtering completions)
    prefix: String,
    /// Column where string content starts (right after opening quote), 0-based
    start_col: u32,
    /// Column where string content ends (right before closing quote, or at cursor if incomplete), 0-based
    end_col: u32,
    /// The quote character used (' or ")
    #[allow(dead_code)]
    quote_char: char,
}

use laravel_lsp::livewire_resolver::{
    blade_contains_inline_class, extract_blade_variable_at_cursor, mfc_sibling,
};

impl LaravelLanguageServer {
    /// Eloquent / DB query builder chain completion entry point.
    ///
    /// Resolves the cursor to a `ChainContext`, dispatches on
    /// `(BuilderMode, ArgKind)` to the appropriate completion-items helper,
    /// and returns the items (or `None` if no chain covers the cursor / the
    /// chain receiver is unresolved at this phase).
    ///
    /// Phase 3 implements `(BaseBuilder, Column)` only — Eloquent receivers
    /// short-circuit in [`laravel_lsp::query_chain::detect_chain_context_at`]
    /// because model resolution is async I/O and lands in later phases.
    async fn try_query_chain_completion(
        &self,
        content: &str,
        position: tower_lsp::lsp_types::Position,
        uri: &tower_lsp::lsp_types::Url,
    ) -> Option<Vec<CompletionItem>> {
        use laravel_lsp::query_chain::{
            detect_chain_context_at, eloquent_completion, position_to_byte_offset, ArgKind,
            BuilderMode,
        };

        let file_path = uri.to_file_path().ok()?;
        let patterns = self.salsa.get_patterns(file_path).await.ok().flatten()?;
        if patterns.chains.is_empty() {
            debug!("🔗 chain completion: no chains cached for this file");
            return None;
        }
        debug!("🔗 chain completion: {} chains in file", patterns.chains.len());

        let byte_offset = position_to_byte_offset(content, position.line, position.character)?;
        let ctx = match detect_chain_context_at(&patterns.chains, byte_offset) {
            Some(c) => c,
            None => {
                debug!(
                    "🔗 chain completion: cursor at byte {} matched no chain link arg",
                    byte_offset
                );
                return None;
            }
        };
        info!(
            "🔗 chain completion: cursor in chain — mode={:?} expecting={:?} table={:?} model={:?}",
            ctx.mode, ctx.expecting, ctx.effective_table, ctx.effective_model
        );

        let db_guard = self.database_schema.read().await;
        let db = match db_guard.as_ref() {
            Some(db) => db,
            None => {
                info!("🔗 chain completion: database_schema not initialised yet");
                return None;
            }
        };

        let items = match (ctx.mode, ctx.expecting) {
            (BuilderMode::BaseBuilder, ArgKind::Column) => {
                eloquent_completion::columns_raw(&ctx, db).await
            }
            // `DB::table('|')` — table-name completion. Mode is always
            // BaseBuilder by the time the receiver is recognised, but we
            // don't gate on it: even if a future receiver shape produced
            // Table args in another mode, tables are tables.
            (_, ArgKind::Table) => eloquent_completion::tables(db).await,
            // Other (mode, expecting) combinations land in Phases 4-6.
            _ => Vec::new(),
        };

        if items.is_empty() {
            info!(
                "🔗 chain completion: matched {:?} but produced 0 items — \
                 likely DB unreachable or table/columns not in introspected schema. \
                 Check the database WARN above; .env DB_HOST may not resolve \
                 outside of Docker/Sail.",
                ctx.expecting
            );
        } else {
            info!("🔗 chain completion: returning {} items", items.len());
        }

        Some(items)
    }

    fn new(client: Client) -> Self {
        Self {
            client,
            documents: Arc::new(RwLock::new(HashMap::new())),
            root_path: Arc::new(RwLock::new(None)),
            diagnostics: Arc::new(RwLock::new(HashMap::new())),
            pending_diagnostics: Arc::new(RwLock::new(HashMap::new())),
            debounce_delay_ms: 200, // 200ms for diagnostics
            salsa: SalsaActor::spawn(),
            cache: Arc::new(RwLock::new(None)),
            pending_rescans: Arc::new(RwLock::new(HashSet::new())),
            rescan_debounce_handle: Arc::new(RwLock::new(None)),
            file_exists_cache: Arc::new(RwLock::new(HashMap::new())),
            cached_config: Arc::new(RwLock::new(None)),
            cached_livewire: Arc::new(RwLock::new(None)),
            last_goto_request: Arc::new(RwLock::new(HashMap::new())),
            initialized_root: Arc::new(RwLock::new(None)),
            pending_salsa_updates: Arc::new(RwLock::new(HashMap::new())),
            auto_complete_debounce_ms: Arc::new(RwLock::new(DEFAULT_SALSA_DEBOUNCE_MS)),
            directive_spacing: Arc::new(RwLock::new(false)),
            vendor_diagnostic_shown: Arc::new(RwLock::new(false)),
            cached_validation_rule_names: Arc::new(RwLock::new(Vec::new())),
            database_schema: Arc::new(RwLock::new(None)),
            database_diagnostic_shown: Arc::new(RwLock::new(false)),
            route_index: Arc::new(RwLock::new(None)),
            vendor_translation_namespaces: Arc::new(RwLock::new(None)),
            route_decl_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Update settings from LSP configuration
    async fn update_settings(&self, settings: &LspSettings) {
        // Autocomplete debounce setting
        let new_debounce = settings.auto_complete_debounce;
        let old_debounce = *self.auto_complete_debounce_ms.read().await;

        if new_debounce != old_debounce {
            info!(
                "⚙️  Updating autocomplete debounce: {}ms → {}ms",
                old_debounce, new_debounce
            );
            *self.auto_complete_debounce_ms.write().await = new_debounce;
        }

        // Blade settings
        let new_spacing = settings.blade.directive_spacing;
        let old_spacing = *self.directive_spacing.read().await;

        if new_spacing != old_spacing {
            info!(
                "⚙️  Updating directive spacing: {} → {}",
                old_spacing, new_spacing
            );
            *self.directive_spacing.write().await = new_spacing;
        }
    }

    /// Extract Blade directive tokens for semantic highlighting
    ///
    /// Finds all `@directive` patterns in the content and converts them to
    /// LSP semantic tokens with delta encoding. Each token is marked as FUNCTION
    /// type (index 0 in our legend).
    ///
    /// The delta encoding format is:
    /// - delta_line: lines since previous token
    /// - delta_start: characters since previous token (or start of line if new line)
    /// - length: token length in characters
    /// - token_type: 0 for FUNCTION
    /// - token_modifiers: 0 (no modifiers)
    fn extract_blade_directive_tokens(&self, content: &str) -> Vec<SemanticToken> {
        use lazy_static::lazy_static;
        use regex::Regex;

        lazy_static! {
            // Match Blade directives: @if, @foreach, @feature, @customDirective, etc.
            // Also matches @end... directives
            static ref DIRECTIVE_RE: Regex = Regex::new(r"@[a-zA-Z]+").unwrap();
        }

        // First pass: collect all directive positions with line/column
        let mut directives: Vec<(u32, u32, u32)> = Vec::new(); // (line, col, length)

        // Build a line/column map by scanning through the content
        let bytes = content.as_bytes();
        let mut line_starts: Vec<usize> = vec![0]; // byte offset of each line start

        for (i, &b) in bytes.iter().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }

        // Find all directive matches
        for mat in DIRECTIVE_RE.find_iter(content) {
            let start_byte = mat.start();
            let length = mat.len() as u32;

            // Find which line this byte offset is on
            let line = line_starts
                .iter()
                .position(|&start| start > start_byte)
                .map(|i| i - 1)
                .unwrap_or(line_starts.len() - 1) as u32;

            // Calculate column (character offset from line start)
            let line_start_byte = line_starts[line as usize];
            let col = (start_byte - line_start_byte) as u32;

            directives.push((line, col, length));
        }

        // Second pass: convert to delta-encoded semantic tokens
        let mut tokens: Vec<SemanticToken> = Vec::new();
        let mut prev_line: u32 = 0;
        let mut prev_col: u32 = 0;

        for (line, col, length) in directives {
            let delta_line = line - prev_line;
            let delta_start = if delta_line == 0 {
                col - prev_col
            } else {
                col // Reset to absolute column on new line
            };

            tokens.push(SemanticToken {
                delta_line,
                delta_start,
                length,
                token_type: 0, // FUNCTION (index 0 in our legend)
                token_modifiers_bitset: 0,
            });

            prev_line = line;
            prev_col = col;
        }

        tokens
    }

    /// Register config files with Salsa for incremental computation
    ///
    /// This reads the config file contents from disk and registers them
    /// with the Salsa actor. The Salsa system will then use these files
    /// for incremental config parsing and caching.
    async fn register_config_with_salsa(&self, root_path: &Path) {
        use std::fs;

        // Read composer.json
        let composer_json = fs::read_to_string(root_path.join("composer.json")).ok();

        // Read config/view.php
        let view_config = fs::read_to_string(root_path.join("config/view.php")).ok();

        // Read config/livewire.php
        let livewire_config = fs::read_to_string(root_path.join("config/livewire.php")).ok();

        // Register with Salsa
        if let Err(e) = self
            .salsa
            .register_config_files(
                root_path.to_path_buf(),
                composer_json,
                view_config,
                livewire_config,
            )
            .await
        {
            debug!("Failed to register config files with Salsa: {}", e);
        } else {
            info!("Laravel LSP: Config files registered with Salsa for incremental caching");
        }
    }

    /// Register project files with Salsa for reference finding
    ///
    /// This scans key directories (controllers, views, Livewire, routes) and
    /// registers all PHP/Blade files with Salsa. The Salsa system will then
    /// cache parsed patterns for efficient reference lookups.
    /// Register the project's PHP/Blade files with Salsa and kick off the
    /// pattern-cache warming task. If `progress` is provided it'll drive the
    /// "Discovering files" → "Indexing X of N" → "Indexed" status-bar updates;
    /// pass `None` from any code path that just needs the registration done
    /// without UI (e.g. re-registration after a config change).
    async fn register_project_files_with_salsa(
        &self,
        root_path: &Path,
        progress: Option<laravel_lsp::indexing_progress::IndexingProgress>,
    ) {
        let config = match self.get_cached_config().await {
            Some(c) => c,
            None => {
                debug!("Cannot register project files - no config available");
                if let Some(p) = progress {
                    p.end("No project config — indexing skipped.").await;
                }
                return;
            }
        };

        // Get view paths from config
        let view_paths = config.view_paths.clone();

        // Get Livewire path from config
        let livewire_path = config.livewire_path.clone();

        // Phase 1: file discovery. The Salsa actor's
        // `handle_register_project_files` walks the project tree and creates
        // a Salsa input per file. This is the synchronous-inside-the-actor
        // step that takes ~3s on a 40k-file project (40k file reads).
        let mut progress = progress;
        if let Some(p) = progress.as_mut() {
            // Force this report through the throttle: the very first
            // user-visible message MUST land, and there's nothing else
            // competing for the throttle window yet.
            // Percentage is sent on the wire (driving the fill bar in
            // clients that render one); the message text never includes
            // a `(X%)` suffix because Zed's status bar is narrow and the
            // numerical progress already lives in the descriptive
            // "Indexing N of M files" text below. Discovery is the
            // pre-parse phase — leave it at 0 so the bar starts empty.
            p.report("Discovering project files…", Some(0), true).await;
        }

        // Register with Salsa
        if let Err(e) = self
            .salsa
            .register_project_files(
                root_path.to_path_buf(),
                vec![PathBuf::from("app/Http/Controllers")], // Default controller path
                view_paths,
                livewire_path,
                PathBuf::from("routes"),
            )
            .await
        {
            debug!("Failed to register project files with Salsa: {}", e);
            if let Some(p) = progress {
                p.end("Failed to register project files.").await;
            }
            return;
        }

        info!("Laravel LSP: Project files registered with Salsa for reference finding");

        // Disk-cache restore. Loads previously-parsed patterns into the
        // shared pattern_cache, dropping any entry whose on-disk mtime
        // doesn't match what was cached. Anything restored here gets
        // skipped by the warming pass below — that's the whole win for
        // cross-restart speed. First-ever launch on a project has no
        // cache file and load_into returns (0, 0).
        if let Some(p) = progress.as_mut() {
            p.report("Loading cached index…", Some(0), true).await;
        }
        let pattern_cache = self.salsa.pattern_cache();
        let root_for_load = root_path.to_path_buf();
        let cache_for_load = pattern_cache.clone();
        let (restored, dropped) = tokio::task::spawn_blocking(move || {
            laravel_lsp::pattern_disk_cache::load_into(&cache_for_load, &root_for_load)
        })
        .await
        .unwrap_or((0, 0));
        if restored + dropped > 0 {
            info!(
                "🗄️  Disk cache: restored {} fresh entries, dropped {} stale",
                restored, dropped
            );
        }

        // Pattern-cache warming via THROTTLED parallel parsing.
        //
        // History: a previous revision of this code spawned an unbounded
        // `tokio::task::spawn_blocking` per file. On real-world projects
        // (hundreds-to-thousands of PHP files) this saturated the blocking
        // thread pool, pinned every CPU core, and consumed enough RAM to
        // hang the machine. Don't do that again.
        //
        // Throttle invariants:
        //   * At most `MAX_CONCURRENT_PARSES` parses in flight at any time
        //     (semaphore-gated). Memory and CPU stay bounded regardless of
        //     project size.
        //   * Per-file size cap. This is NOT a defensive guard against
        //     50MB files — it's a hard requirement for warming to finish
        //     in reasonable time. Some auto-generated vendor PHP files
        //     (composer autoload maps, AWS SDK service definitions, Faker
        //     locale text dumps, IDE helper output) are 500KB–2MB+ of
        //     deeply-nested array literals. tree-sitter-php on those
        //     files takes 30+ seconds *each*, single-threaded. With our
        //     8-worker pool, ~8 such files in flight stalls every core
        //     and the warming task wall-clock balloons from "seconds"
        //     to "minutes" on a 40k-file project. Empirically: dropping
        //     the cap from 4MB → 256KB cut warming from 70s to <10s on
        //     a real Laravel project with full vendor + Flux icons +
        //     IDE helpers checked in.
        //
        //     Picking 256KB: Mike's biggest real-app PHP file in the
        //     test project is 55KB (a static data table). IDE helpers
        //     top out at 186KB. Vendor blobs that hit this cap are all
        //     auto-generated metadata that doesn't contain user-facing
        //     `route()`/`view()`/`config()` patterns we care about.
        //   * Bulk-import in chunks so the actor's pattern_cache.put loop
        //     never holds the actor for too long on a giant project.
        const MAX_CONCURRENT_PARSES: usize = 8;
        const MAX_FILE_SIZE_BYTES: u64 = 256 * 1024; // 256 KB

        let salsa = self.salsa.clone();
        let root_for_save = root_path.to_path_buf();
        // pattern_cache already cloned above for the load step; clone
        // again here so the warming task can (a) skip files already in
        // cache from the load and (b) hand the same map to save_from.
        let pattern_cache_for_warm = pattern_cache.clone();
        tokio::spawn(async move {
            // `progress` moves into this task — when warming finishes (or
            // hits an early return) we call `end` to clear the status bar.
            let mut progress = progress;
            let started_at = std::time::Instant::now();

            // Defensively prewarm the global query cache on this thread
            // before spawning parallel parses. The Salsa actor already
            // prewarms during init, but if our warming task races ahead
            // of that, one parse_owned call would pay the ~400ms one-time
            // compilation cost. Doing it here once is free if already
            // warm, prevents per-task surprises if not.
            tokio::task::spawn_blocking(laravel_lsp::queries::prewarm_query_cache)
                .await
                .ok();

            // Discovery is done — transition to "Indexing" phase. Forced
            // so the user sees the phase change even if the throttle
            // would otherwise drop it.
            if let Some(p) = progress.as_mut() {
                // Parse phase transition: still at 0% — the per-file
                // updates below increment from here to 100.
                p.report("Indexing project files…", Some(0), true).await;
            }

            let paths = match salsa.list_project_files().await {
                Ok(p) => p,
                Err(e) => {
                    debug!("list_project_files failed: {}", e);
                    if let Some(p) = progress {
                        p.end("Failed to list project files.").await;
                    }
                    return;
                }
            };
            // Filter out paths whose parsed patterns the disk cache
            // already restored — those files are unchanged since the
            // last save and don't need re-parsing. This is THE point
            // where cross-restart speed comes from: on a project with
            // 40k files and no edits, this drops `paths_to_parse` to
            // (close to) zero, and warming finishes in milliseconds.
            let total = paths.len();
            let paths_to_parse: Vec<_> = paths
                .into_iter()
                .filter(|p| !pattern_cache_for_warm.contains_key(p))
                .collect();
            let to_parse = paths_to_parse.len();
            let cached_hits = total - to_parse;
            if cached_hits > 0 {
                info!(
                    "🗄️  Warming will reuse {} cached files, parse {}",
                    cached_hits, to_parse
                );
            }

            let semaphore = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_PARSES));

            // Spawn one task per file we still need to parse. The
            // semaphore ensures we never have more than N parses running
            // (or holding parsed-tree memory) at once.
            let mut handles = Vec::with_capacity(to_parse);
            for path in paths_to_parse {
                let permit_owner = semaphore.clone();
                handles.push(tokio::spawn(async move {
                    let _permit = match permit_owner.acquire_owned().await {
                        Ok(p) => p,
                        Err(_) => return None,
                    };
                    // Skip `.json.php` files (Laravel/PHP convention for
                    // pre-baked JSON data wrapped as PHP). These are
                    // pure data — never user-facing Laravel patterns —
                    // and tree-sitter-php chokes on their deeply-nested
                    // array literals (0.4–2.2s *per file*). Mike's test
                    // project has 1,735 of these under aws-sdk-php.
                    //
                    // **We still insert an empty `ParsedPatternsData`**
                    // for these files so future cache lookups hit
                    // immediately instead of falling through to the
                    // Salsa parse path. Without this, `find-references`
                    // walks every project file and any path not in
                    // `pattern_cache` re-parses via Salsa — which has
                    // no size cap and chokes on these exact files.
                    // The empty patterns are ~30 bytes each in the
                    // serialized disk cache; negligible.
                    let path_str = path.to_string_lossy();
                    if path_str.ends_with(".json.php") {
                        return Some((
                            path,
                            Arc::new(laravel_lsp::salsa_impl::ParsedPatternsData::default()),
                        ));
                    }

                    // Same idea for the size cap: skip the parse, but
                    // insert empty patterns so the cache lookup
                    // succeeds and never falls through to Salsa.
                    let metadata = std::fs::metadata(&path).ok()?;
                    if metadata.len() > MAX_FILE_SIZE_BYTES {
                        info!(
                            "warming: skipping oversized file {} ({} bytes > {} cap)",
                            path.display(),
                            metadata.len(),
                            MAX_FILE_SIZE_BYTES
                        );
                        return Some((
                            path,
                            Arc::new(laravel_lsp::salsa_impl::ParsedPatternsData::default()),
                        ));
                    }
                    // Actual parse on the blocking pool — tree-sitter is
                    // CPU-bound and would block the async runtime if run
                    // directly in a tokio task.
                    let path_for_task = path.clone();
                    let parsed: Option<Arc<laravel_lsp::salsa_impl::ParsedPatternsData>> =
                        tokio::task::spawn_blocking(move || {
                            let text = std::fs::read_to_string(&path_for_task).ok()?;
                            Some(laravel_lsp::pattern_indexer::parse_owned(
                                &path_for_task,
                                &text,
                            ))
                        })
                        .await
                        .ok()
                        .flatten();
                    parsed.map(|data| (path, data))
                }));
            }

            // Collect all parse results into a single buffer, then bulk-
            // import directly into the shared DashMap-backed pattern cache
            // via SalsaHandle::bulk_import_patterns. That call bypasses
            // the actor's mpsc channel entirely — see the comment on
            // SalsaActor::pattern_cache and SalsaHandle::bulk_import_patterns
            // for the architectural why. With this path: warming on a
            // 40k-file project takes ~7s wall (~6.5s parse + ~10ms import).
            //
            // Progress reports are emitted as each handle completes. The
            // IndexingProgress helper throttles internally so we don't
            // need to be careful about emitting every iteration.
            let mut buffer: Vec<(PathBuf, Arc<laravel_lsp::salsa_impl::ParsedPatternsData>)> =
                Vec::with_capacity(to_parse);
            let mut completed = 0usize;
            for h in handles {
                if let Ok(Some(pair)) = h.await {
                    buffer.push(pair);
                }
                completed += 1;
                if let Some(p) = progress.as_mut() {
                    // Progress is over the files we're actually parsing
                    // this session, not the total project size. Showing
                    // "Indexing 12 of 12 files" after a warm restart
                    // gives accurate feedback for the work in progress;
                    // the user already saw the cached restore land in
                    // the "Loading cached index…" step before this.
                    let denom = to_parse.max(1);
                    let pct = ((completed.saturating_mul(100) / denom) as u32).min(100);
                    p.report(
                        format!("Indexing {} of {} files…", completed, to_parse),
                        Some(pct),
                        false,
                    )
                    .await;
                }
            }
            let imported = salsa.bulk_import_patterns(buffer).await.unwrap_or(0);

            // Persist the entire live pattern_cache (cached restores +
            // freshly parsed entries) so the next LSP startup can skip
            // most of the work. Runs on the blocking pool because it's
            // sync I/O — and we don't gate user-visible warming
            // completion on it; the save just runs and logs its outcome.
            let cache_for_save = pattern_cache_for_warm.clone();
            let root_for_save_inner = root_for_save.clone();
            let save_result = tokio::task::spawn_blocking(move || {
                laravel_lsp::pattern_disk_cache::save_from(&cache_for_save, &root_for_save_inner)
            })
            .await;
            match save_result {
                Ok(Ok(n)) => info!("🗄️  Disk cache: saved {} entries", n),
                Ok(Err(e)) => debug!("Disk cache save failed: {}", e),
                Err(e) => debug!("Disk cache save task panicked: {}", e),
            }

            // Build the inverted symbol index now that pattern_cache
            // is fully populated. Without this, the first
            // `find-references` query pays an O(N files) walk; with
            // it, queries are an O(1) hashmap lookup. Index build is
            // ~50ms on a 60k-file project — well within the warming
            // budget we already log.
            match salsa.build_symbol_index().await {
                Ok(count) => info!("🔍 Symbol index built: {} symbol entries", count),
                Err(e) => debug!("Symbol index build failed: {}", e),
            }

            let elapsed = started_at.elapsed();
            info!(
                "🔥 Laravel LSP: pattern cache warmed ({} newly parsed, {} from disk, total {}, in {:?})",
                imported, cached_hits, total, elapsed
            );

            if let Some(p) = progress {
                p.end(format!(
                    "Indexed {} files in {:.1}s.",
                    total,
                    elapsed.as_secs_f64()
                ))
                .await;
            }
        });
    }

    /// Return the cached `route_name_locator` output for `path`. Re-parses
    /// only when the file's mtime differs from the cached entry. Returns
    /// `None` if the file can't be stat'd or read.
    ///
    /// Without this cache, every find-references / rename / prepare_rename
    /// on a route triggers a fresh tree-sitter parse of every routes/*.php
    /// file. With it, the first call per mtime parses; subsequent calls
    /// are HashMap lookups.
    async fn cached_route_decls(
        &self,
        path: &Path,
    ) -> Option<Arc<Vec<laravel_lsp::route_name_locator::RouteNameDeclaration>>> {
        let disk_mtime = std::fs::metadata(path).ok()?.modified().ok()?;

        // Cache hit?
        {
            let cache = self.route_decl_cache.read().await;
            if let Some((cached_mtime, decls)) = cache.get(path) {
                if *cached_mtime == disk_mtime {
                    return Some(decls.clone());
                }
            }
        }

        // Cache miss / stale — read + parse + store.
        let content = tokio::fs::read_to_string(path).await.ok()?;
        let decls =
            Arc::new(laravel_lsp::route_name_locator::extract_route_name_declarations(&content));
        self.route_decl_cache
            .write()
            .await
            .insert(path.to_path_buf(), (disk_mtime, decls.clone()));
        Some(decls)
    }

    /// Register environment files directly with Salsa for parsing
    ///
    /// This registers raw .env file content with Salsa, which parses them
    /// using the tracked `parse_env_source` function. Salsa handles caching
    /// and incremental updates automatically.
    ///
    /// Priority: .env.example=0, .env.local=1, .env=2 (higher wins)
    async fn register_env_files_with_salsa(&self, root: &Path) {
        // Define env files with their priorities
        // Priority: 0=.env.example, 1=.env.local, 2=.env
        let env_files = [
            (root.join(".env.example"), 0u8),
            (root.join(".env.local"), 1u8),
            (root.join(".env"), 2u8),
        ];

        let documents = self.documents.read().await;
        let mut registered_count = 0;

        for (env_path, priority) in env_files {
            // Get content from editor buffer or disk
            let content = if let Ok(env_uri) = Url::from_file_path(&env_path) {
                if let Some((buffer_content, _version)) = documents.get(&env_uri) {
                    // Use editor buffer content (includes unsaved changes)
                    debug!("Laravel LSP: Registering .env from buffer: {:?}", env_path);
                    Some(buffer_content.clone())
                } else if env_path.exists() {
                    // Read from disk
                    debug!("Laravel LSP: Registering .env from disk: {:?}", env_path);
                    std::fs::read_to_string(&env_path).ok()
                } else {
                    None
                }
            } else if env_path.exists() {
                std::fs::read_to_string(&env_path).ok()
            } else {
                None
            };

            if let Some(text) = content {
                if let Err(e) = self
                    .salsa
                    .register_env_source(env_path.clone(), text, priority)
                    .await
                {
                    debug!(
                        "Failed to register env file {:?} with Salsa: {}",
                        env_path, e
                    );
                } else {
                    registered_count += 1;
                }
            }
        }

        if registered_count > 0 {
            info!(
                "Laravel LSP: {} env files registered with Salsa",
                registered_count
            );
        }
    }

    /// Register service provider files directly with Salsa for parsing
    ///
    /// This scans for service provider files and registers their raw content
    /// with Salsa, which parses them using the tracked `parse_service_provider_source`
    /// function. Salsa handles caching and incremental updates automatically.
    ///
    /// Priority: framework=0, packages=1, app=2 (higher wins)
    async fn register_service_provider_files_with_salsa(&self, root: &Path) {
        let documents = self.documents.read().await;
        let mut registered_count = 0;

        // Priority 0: Framework providers
        let framework_path = root.join("vendor/laravel/framework/src/Illuminate");
        if framework_path.exists() {
            for entry in WalkDir::new(&framework_path)
                .max_depth(10)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                let path = entry.path();
                if path.is_file()
                    && path.extension().is_some_and(|ext| ext == "php")
                    && path
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().ends_with("ServiceProvider.php"))
                {
                    if let Ok(content) = std::fs::read_to_string(path) {
                        if self
                            .salsa
                            .register_service_provider_source(
                                path.to_path_buf(),
                                content,
                                0, // framework priority
                                root.to_path_buf(),
                            )
                            .await
                            .is_ok()
                        {
                            registered_count += 1;
                        }
                    }
                }
            }
        }

        // Priority 1: Package providers and Kernel files (for middleware definitions)
        let vendor_path = root.join("vendor");
        debug!(
            "🔍 Scanning vendor path: {:?} (exists={})",
            vendor_path,
            vendor_path.exists()
        );
        if vendor_path.exists() {
            for entry in WalkDir::new(&vendor_path)
                .max_depth(6)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                let path = entry.path();
                // Skip framework (already done with priority 0)
                if path.starts_with(&framework_path) {
                    continue;
                }
                if path.is_file() && path.extension().is_some_and(|ext| ext == "php") {
                    let file_name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let path_str = path.to_string_lossy();

                    // Scan ServiceProvider files for middleware/binding registrations
                    // and Http/Kernel.php files for middleware alias/group definitions
                    let is_service_provider = file_name.ends_with("ServiceProvider.php");
                    let is_http_kernel = file_name == "Kernel.php"
                        && (path_str.contains("/Http/") || path_str.contains("\\Http\\"));

                    if is_service_provider || is_http_kernel {
                        debug!(
                            "📄 [init] Found vendor file: {} (SP={}, Kernel={})",
                            path_str, is_service_provider, is_http_kernel
                        );
                        if let Ok(content) = std::fs::read_to_string(path) {
                            if self
                                .salsa
                                .register_service_provider_source(
                                    path.to_path_buf(),
                                    content,
                                    1, // package priority
                                    root.to_path_buf(),
                                )
                                .await
                                .is_ok()
                            {
                                registered_count += 1;
                            }
                        }
                    }
                }
            }
        }

        // Priority 2: Application providers (app/Providers/)
        let app_providers_path = root.join("app/Providers");
        if app_providers_path.exists() {
            for entry in WalkDir::new(&app_providers_path)
                .max_depth(3)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                let path = entry.path();
                if path.is_file() && path.extension().is_some_and(|ext| ext == "php") {
                    // Check if file is open in editor
                    let content = if let Ok(uri) = Url::from_file_path(path) {
                        if let Some((buffer_content, _)) = documents.get(&uri) {
                            buffer_content.clone()
                        } else {
                            std::fs::read_to_string(path).unwrap_or_default()
                        }
                    } else {
                        std::fs::read_to_string(path).unwrap_or_default()
                    };

                    if !content.is_empty()
                        && self
                            .salsa
                            .register_service_provider_source(
                                path.to_path_buf(),
                                content,
                                2, // app priority
                                root.to_path_buf(),
                            )
                            .await
                            .is_ok()
                    {
                        registered_count += 1;
                    }
                }
            }
        }

        // Priority 0: Laravel framework's default middleware configuration (Laravel 11+)
        // This provides 'web', 'api', 'auth', 'guest', etc. from the framework
        let framework_middleware_config = root.join(
            "vendor/laravel/framework/src/Illuminate/Foundation/Configuration/Middleware.php",
        );
        if framework_middleware_config.exists() {
            if let Ok(content) = std::fs::read_to_string(&framework_middleware_config) {
                if self
                    .salsa
                    .register_service_provider_source(
                        framework_middleware_config,
                        content,
                        0, // framework priority (can be overridden by app)
                        root.to_path_buf(),
                    )
                    .await
                    .is_ok()
                {
                    registered_count += 1;
                }
            }
        }

        // Priority 2: bootstrap/app.php (Laravel 11+)
        let bootstrap_app = root.join("bootstrap/app.php");
        if bootstrap_app.exists() {
            let content = if let Ok(uri) = Url::from_file_path(&bootstrap_app) {
                if let Some((buffer_content, _)) = documents.get(&uri) {
                    buffer_content.clone()
                } else {
                    std::fs::read_to_string(&bootstrap_app).unwrap_or_default()
                }
            } else {
                std::fs::read_to_string(&bootstrap_app).unwrap_or_default()
            };

            if !content.is_empty() {
                // First, extract and scan imported middleware configuration classes
                // This handles Laravel's default middleware aliases (auth, guest, etc.)
                for imported_file in extract_middleware_imports(&content, root) {
                    if let Ok(imported_content) = std::fs::read_to_string(&imported_file) {
                        if self
                            .salsa
                            .register_service_provider_source(
                                imported_file,
                                imported_content,
                                0, // framework priority (can be overridden by app)
                                root.to_path_buf(),
                            )
                            .await
                            .is_ok()
                        {
                            registered_count += 1;
                        }
                    }
                }

                // Then scan bootstrap/app.php itself for user-defined middleware
                if self
                    .salsa
                    .register_service_provider_source(
                        bootstrap_app,
                        content,
                        2, // app priority
                        root.to_path_buf(),
                    )
                    .await
                    .is_ok()
                {
                    registered_count += 1;
                }
            }
        }

        // Priority 2: app/Http/Kernel.php (Laravel 10)
        let kernel_path = root.join("app/Http/Kernel.php");
        if kernel_path.exists() {
            let content = if let Ok(uri) = Url::from_file_path(&kernel_path) {
                if let Some((buffer_content, _)) = documents.get(&uri) {
                    buffer_content.clone()
                } else {
                    std::fs::read_to_string(&kernel_path).unwrap_or_default()
                }
            } else {
                std::fs::read_to_string(&kernel_path).unwrap_or_default()
            };

            if !content.is_empty()
                && self
                    .salsa
                    .register_service_provider_source(
                        kernel_path,
                        content,
                        2, // app priority
                        root.to_path_buf(),
                    )
                    .await
                    .is_ok()
            {
                registered_count += 1;
            }
        }

        if registered_count > 0 {
            info!(
                "Laravel LSP: {} service provider files registered with Salsa",
                registered_count
            );
        }
    }

    /// Load ALL cached data directly into memory (NO Salsa calls - instant)
    /// Returns the list of rescans needed for background processing
    async fn load_cache_data(&self, root: &Path) -> Vec<RescanType> {
        let start = std::time::Instant::now();

        // Load cache from disk
        let cache = CacheManager::load(root);

        if cache.has_cached_data() {
            // 1. Store cached Laravel config directly in memory (bypasses Salsa)
            if let Some(cached_config) = cache.get_laravel_config() {
                info!(
                    "📋 Loading cached Laravel config: {} view paths, root: {:?}",
                    cached_config.view_paths.len(),
                    cached_config.root
                );
                let config_data = LaravelConfigData {
                    root: cached_config.root.clone(),
                    view_paths: cached_config.view_paths.clone(),
                    component_paths: cached_config.component_paths.clone(),
                    livewire_path: cached_config.livewire_path.clone(),
                    has_livewire: cached_config.has_livewire,
                    view_namespaces: std::collections::HashMap::new(),
                    component_namespaces: std::collections::HashMap::new(),
                    component_aliases: laravel_lsp::config::load_component_aliases(
                        &cached_config.root,
                    ),
                    icon_aliases: laravel_lsp::config::scan_vendor_for_icon_sets(
                        &cached_config.root,
                    ),
                };
                // Store directly in memory - no Salsa channel call!
                *self.cached_config.write().await = Some(config_data);

                // Update root_path to the cached config's root (the actual Laravel project)
                // and mark it as initialized to prevent re-discovery on file open
                let actual_root = cached_config.root.clone();
                info!(
                    "📂 Setting actual Laravel root to {:?} from cache",
                    actual_root
                );
                *self.root_path.write().await = Some(actual_root.clone());
                *self.initialized_root.write().await = Some(actual_root);
            }

            // 2-4: Register middleware/bindings/env with Salsa in background
            // These are needed for goto but not for basic diagnostics
            let middleware_count = cache.get_all_middleware().len();
            let binding_count = cache.get_all_bindings().len();
            let env_count = cache.get_env_vars().map(|e| e.variables.len()).unwrap_or(0);
            info!(
                "📦 Queuing {} middleware, {} bindings, {} env vars for background registration",
                middleware_count, binding_count, env_count
            );

            // Spawn background registration (doesn't block initialize)
            let salsa = self.salsa.clone();
            let middleware_entries: Vec<_> = cache
                .get_all_middleware()
                .into_iter()
                .map(|(alias, entry)| {
                    (
                        alias,
                        entry.class,
                        entry.class_file,
                        entry.source_file,
                        entry.line,
                    )
                })
                .collect();
            let binding_entries: Vec<_> = cache
                .get_all_bindings()
                .into_iter()
                .map(|(name, entry)| {
                    (
                        name,
                        entry.class,
                        entry.binding_type,
                        entry.class_file,
                        entry.source_file,
                        entry.line,
                    )
                })
                .collect();
            let env_vars = cache.get_env_vars().map(|e| e.variables.clone());
            let cached_config_for_salsa = cache.get_laravel_config().map(|c| LaravelConfigData {
                root: c.root.clone(),
                view_paths: c.view_paths.clone(),
                component_paths: c.component_paths.clone(),
                livewire_path: c.livewire_path.clone(),
                has_livewire: c.has_livewire,
                view_namespaces: std::collections::HashMap::new(),
                component_namespaces: std::collections::HashMap::new(),
                component_aliases: laravel_lsp::config::load_component_aliases(&c.root),
                icon_aliases: laravel_lsp::config::scan_vendor_for_icon_sets(&c.root),
            });

            tokio::spawn(async move {
                // Register with Salsa in background for incremental updates
                if let Some(config) = cached_config_for_salsa {
                    let _ = salsa.register_cached_config(config).await;
                }
                if let Some(vars) = env_vars {
                    let _ = salsa.register_cached_env_vars(vars).await;
                }
                let _ = salsa
                    .register_cached_middleware_batch(middleware_entries)
                    .await;
                let _ = salsa.register_cached_binding_batch(binding_entries).await;
                info!("✅ Background Salsa registration complete");
            });

            // The route index is in-memory only — there's no disk cache for it
            // yet — so we must build it on every fast-path entry too. Without
            // this, projects with a hot cache skip the slow init entirely and
            // route goto-definition silently returns no results.
            let route_root = root.to_path_buf();
            let server = self.clone_for_spawn();
            tokio::spawn(async move {
                server.rebuild_route_index(&route_root).await;
            });
        }

        // Check what needs rescanning before storing cache
        let needs_rescans = cache.get_needed_rescans();

        // Store cache manager
        *self.cache.write().await = Some(cache);

        info!("⚡ Cache loaded in {:?}", start.elapsed());

        if needs_rescans.is_empty() {
            info!("✅ Cache is valid, no rescans needed");
        } else {
            info!("🔄 Will queue background rescans: {:?}", needs_rescans);
        }

        needs_rescans
    }

    /// Rescan vendor directory (framework + packages)
    async fn rescan_vendor_providers(&self, root: &Path) {
        info!("🔍 Rescanning vendor providers...");
        let start = std::time::Instant::now();

        let documents = self.documents.read().await;
        let mut registered_count = 0;
        let mut middleware_count = 0;
        let mut bindings_count = 0;

        // Priority 0: Framework providers
        let framework_path = root.join("vendor/laravel/framework/src/Illuminate");
        if framework_path.exists() {
            for entry in WalkDir::new(&framework_path)
                .max_depth(10)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                let path = entry.path();
                if path.is_file()
                    && path.extension().is_some_and(|ext| ext == "php")
                    && path
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().ends_with("ServiceProvider.php"))
                {
                    if let Ok(content) = std::fs::read_to_string(path) {
                        if self
                            .salsa
                            .register_service_provider_source(
                                path.to_path_buf(),
                                content,
                                0, // framework priority
                                root.to_path_buf(),
                            )
                            .await
                            .is_ok()
                        {
                            registered_count += 1;
                        }
                    }
                }
            }
        }

        // Priority 1: Package providers and Kernel files (for middleware definitions)
        let vendor_path = root.join("vendor");
        if vendor_path.exists() {
            for entry in WalkDir::new(&vendor_path)
                .max_depth(6)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                let path = entry.path();
                // Skip framework (already done with priority 0)
                if path.starts_with(&framework_path) {
                    continue;
                }
                if path.is_file() && path.extension().is_some_and(|ext| ext == "php") {
                    let file_name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let path_str = path.to_string_lossy();

                    // Scan ServiceProvider files for middleware/binding registrations
                    // and Http/Kernel.php files for middleware alias/group definitions
                    let is_service_provider = file_name.ends_with("ServiceProvider.php");
                    let is_http_kernel = file_name == "Kernel.php"
                        && (path_str.contains("/Http/") || path_str.contains("\\Http\\"));

                    if is_service_provider || is_http_kernel {
                        if let Ok(content) = std::fs::read_to_string(path) {
                            if self
                                .salsa
                                .register_service_provider_source(
                                    path.to_path_buf(),
                                    content,
                                    1, // package priority
                                    root.to_path_buf(),
                                )
                                .await
                                .is_ok()
                            {
                                registered_count += 1;
                            }
                        }
                    }
                }
            }
        }

        drop(documents);

        // Get counts for logging (cache population happens in execute_pending_rescans)
        if let Ok(all_mw) = self.salsa.get_all_parsed_middleware().await {
            middleware_count = all_mw.len();
        }
        if let Ok(all_bindings) = self.salsa.get_all_parsed_bindings().await {
            bindings_count = all_bindings.len();
        }

        // Update mtime (cache data population happens in populate_cache_from_salsa)
        let mut cache_guard = self.cache.write().await;
        if let Some(ref mut cache) = *cache_guard {
            cache.update_mtime("composer.lock");
        }

        let duration = start.elapsed();
        info!(
            "✅ Vendor rescan complete: {} providers, {} middleware, {} bindings in {:?}",
            registered_count, middleware_count, bindings_count, duration
        );
    }

    /// Rescan app providers (app/Providers + bootstrap/app.php)
    async fn rescan_app_providers(&self, root: &Path) {
        info!("🔍 Rescanning app providers...");
        let start = std::time::Instant::now();

        let documents = self.documents.read().await;
        let mut registered_count = 0;

        // Priority 2: Application providers (app/Providers/)
        let app_providers_path = root.join("app/Providers");
        if app_providers_path.exists() {
            for entry in WalkDir::new(&app_providers_path)
                .max_depth(3)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                let path = entry.path();
                if path.is_file() && path.extension().is_some_and(|ext| ext == "php") {
                    let content = if let Ok(uri) = Url::from_file_path(path) {
                        if let Some((buffer_content, _)) = documents.get(&uri) {
                            buffer_content.clone()
                        } else {
                            std::fs::read_to_string(path).unwrap_or_default()
                        }
                    } else {
                        std::fs::read_to_string(path).unwrap_or_default()
                    };

                    if !content.is_empty()
                        && self
                            .salsa
                            .register_service_provider_source(
                                path.to_path_buf(),
                                content,
                                2, // app priority
                                root.to_path_buf(),
                            )
                            .await
                            .is_ok()
                    {
                        registered_count += 1;
                    }
                }
            }
        }

        // Priority 0: Laravel framework's default middleware configuration (Laravel 11+)
        let framework_middleware_config = root.join(
            "vendor/laravel/framework/src/Illuminate/Foundation/Configuration/Middleware.php",
        );
        if framework_middleware_config.exists() {
            if let Ok(content) = std::fs::read_to_string(&framework_middleware_config) {
                if self
                    .salsa
                    .register_service_provider_source(
                        framework_middleware_config,
                        content,
                        0, // framework priority
                        root.to_path_buf(),
                    )
                    .await
                    .is_ok()
                {
                    registered_count += 1;
                }
            }
        }

        // Priority 2: bootstrap/app.php (Laravel 11+)
        let bootstrap_app = root.join("bootstrap/app.php");
        if bootstrap_app.exists() {
            let content = if let Ok(uri) = Url::from_file_path(&bootstrap_app) {
                if let Some((buffer_content, _)) = documents.get(&uri) {
                    buffer_content.clone()
                } else {
                    std::fs::read_to_string(&bootstrap_app).unwrap_or_default()
                }
            } else {
                std::fs::read_to_string(&bootstrap_app).unwrap_or_default()
            };

            if !content.is_empty() {
                // First, extract and scan imported middleware configuration classes
                for imported_file in extract_middleware_imports(&content, root) {
                    if let Ok(imported_content) = std::fs::read_to_string(&imported_file) {
                        if self
                            .salsa
                            .register_service_provider_source(
                                imported_file,
                                imported_content,
                                0, // framework priority
                                root.to_path_buf(),
                            )
                            .await
                            .is_ok()
                        {
                            registered_count += 1;
                        }
                    }
                }

                // Then scan bootstrap/app.php itself
                if self
                    .salsa
                    .register_service_provider_source(
                        bootstrap_app,
                        content,
                        2, // app priority
                        root.to_path_buf(),
                    )
                    .await
                    .is_ok()
                {
                    registered_count += 1;
                }
            }
        }

        // Priority 2: app/Http/Kernel.php (Laravel 10)
        let kernel_path = root.join("app/Http/Kernel.php");
        if kernel_path.exists() {
            let content = if let Ok(uri) = Url::from_file_path(&kernel_path) {
                if let Some((buffer_content, _)) = documents.get(&uri) {
                    buffer_content.clone()
                } else {
                    std::fs::read_to_string(&kernel_path).unwrap_or_default()
                }
            } else {
                std::fs::read_to_string(&kernel_path).unwrap_or_default()
            };

            if !content.is_empty()
                && self
                    .salsa
                    .register_service_provider_source(
                        kernel_path,
                        content,
                        2, // app priority
                        root.to_path_buf(),
                    )
                    .await
                    .is_ok()
            {
                registered_count += 1;
            }
        }

        drop(documents);

        // Update cache
        let mut cache_guard = self.cache.write().await;
        if let Some(ref mut cache) = *cache_guard {
            cache.update_mtime("bootstrap/app.php");
            cache.update_mtime_glob("app/Providers/*.php");
        }

        let duration = start.elapsed();
        info!(
            "✅ App rescan complete: {} providers in {:?}",
            registered_count, duration
        );
    }

    /// Rescan node_modules (for Flux, etc.)
    async fn rescan_node_modules(&self, _root: &Path) {
        info!("🔍 Rescanning node_modules...");
        let start = std::time::Instant::now();

        // TODO: Scan for Flux components in node_modules
        // For now, just update the mtime

        let mut cache_guard = self.cache.write().await;
        if let Some(ref mut cache) = *cache_guard {
            cache.update_mtime("package-lock.json");
            cache.update_mtime("yarn.lock");
            cache.update_mtime("pnpm-lock.yaml");
        }

        let duration = start.elapsed();
        info!("✅ Node modules rescan complete in {:?}", duration);
    }

    /// Queue a background rescan (debounced)
    async fn queue_background_rescan(&self, rescan_type: RescanType) {
        // Add to pending set
        self.pending_rescans.write().await.insert(rescan_type);

        // Cancel existing debounce timer
        if let Some(handle) = self.rescan_debounce_handle.write().await.take() {
            handle.abort();
        }

        // Start new debounce timer (500ms)
        let server = self.clone_for_spawn();
        let handle = tokio::spawn(async move {
            sleep(Duration::from_millis(500)).await;
            server.execute_pending_rescans().await;
        });

        *self.rescan_debounce_handle.write().await = Some(handle);
    }

    /// Execute all pending rescans
    async fn execute_pending_rescans(&self) {
        let pending: Vec<RescanType> = self.pending_rescans.write().await.drain().collect();

        if pending.is_empty() {
            return;
        }

        let root = self.root_path.read().await.clone();
        let Some(root) = root else {
            warn!("Cannot execute rescans: no root path");
            return;
        };

        info!("🔄 Executing pending rescans: {:?}", pending);

        for rescan_type in &pending {
            match rescan_type {
                RescanType::Vendor => self.rescan_vendor_providers(&root).await,
                RescanType::App => self.rescan_app_providers(&root).await,
                RescanType::NodeModules => self.rescan_node_modules(&root).await,
            }
        }

        // Rebuild the route name index so any route definition changes (added,
        // renamed, removed) are reflected on the next goto-definition request.
        self.rebuild_route_index(&root).await;

        // Populate cache with ALL parsed middleware/bindings AFTER all rescans complete
        // This ensures we capture middleware from both vendor and app sources
        self.populate_cache_from_salsa().await;

        // Save cache
        if let Some(ref cache) = *self.cache.read().await {
            if let Err(e) = cache.save() {
                warn!("Failed to save cache: {}", e);
            } else {
                info!("💾 Cache saved successfully");
            }
        }

        // Re-validate open documents
        self.revalidate_open_documents().await;
    }

    /// Populate cache with all data from Salsa (config, env, middleware, bindings)
    async fn populate_cache_from_salsa(&self) {
        let mut cache_guard = self.cache.write().await;
        let Some(ref mut cache) = *cache_guard else {
            return;
        };

        // 1. Cache Laravel config
        if let Ok(Some(config)) = self.salsa.get_laravel_config().await {
            let cached_config = CachedLaravelConfig {
                root: config.root.clone(),
                view_paths: config.view_paths.clone(),
                component_paths: config.component_paths.clone(),
                livewire_path: config.livewire_path.clone(),
                has_livewire: config.has_livewire,
            };
            info!(
                "📋 Caching Laravel config: {} view paths",
                config.view_paths.len()
            );
            cache.set_laravel_config(cached_config);
        }

        // 2. Cache env variables
        if let Ok(env_vars) = self.salsa.get_all_parsed_env_vars().await {
            let mut variables = std::collections::HashMap::new();
            for var in &env_vars {
                variables.insert(var.name.clone(), var.value.clone());
            }
            debug!("Caching {} env variables", variables.len());
            cache.set_env_vars(CachedEnvVars { variables });
        }

        // 3. Cache middleware
        if let Ok(all_mw) = self.salsa.get_all_parsed_middleware().await {
            let mut vendor_scan = ScanResult::default();
            for mw in &all_mw {
                vendor_scan.middleware.insert(
                    mw.alias.clone(),
                    MiddlewareEntry {
                        class: mw.class_name.clone(),
                        class_file: mw
                            .file_path
                            .as_ref()
                            .map(|p| p.to_string_lossy().into_owned()),
                        source_file: Some(mw.source_file.to_string_lossy().into_owned()),
                        line: mw.source_line,
                    },
                );
            }
            info!("📦 Caching {} middleware aliases", all_mw.len());
            cache.set_vendor_scan(vendor_scan);
        }

        // 4. Cache bindings
        if let Ok(all_bindings) = self.salsa.get_all_parsed_bindings().await {
            let mut app_scan = ScanResult::default();
            for binding in &all_bindings {
                app_scan.bindings.insert(
                    binding.abstract_name.clone(),
                    BindingEntry {
                        class: binding.concrete_class.clone(),
                        binding_type: format!("{:?}", binding.binding_type),
                        class_file: binding
                            .file_path
                            .as_ref()
                            .map(|p| p.to_string_lossy().into_owned()),
                        source_file: Some(binding.source_file.to_string_lossy().into_owned()),
                        line: binding.source_line,
                    },
                );
            }
            info!("📦 Caching {} bindings", all_bindings.len());
            cache.set_app_scan(app_scan);
        }
    }

    /// Re-validate all open documents after a rescan
    async fn revalidate_open_documents(&self) {
        let documents = self.documents.read().await;
        let uris: Vec<Url> = documents.keys().cloned().collect();
        drop(documents);

        for uri in uris {
            if let Some((content, _)) = self.documents.read().await.get(&uri).cloned() {
                self.validate_and_publish_diagnostics(&uri, &content).await;
            }
        }
    }

    /// Try to discover Laravel config from a file path
    ///
    /// This implements a hybrid discovery strategy:
    /// - Always tries to find Laravel root from the opened file
    /// - Updates config if discovered root is more specific or file is outside current root
    /// - This handles both nested Laravel projects and files outside initial workspace
    async fn try_discover_from_file(&self, file_path: &Path) {
        // Always try to find the Laravel project root from this file
        let Some(discovered_root) = find_project_root(file_path) else {
            debug!(
                "Could not find Laravel project root from file: {:?}",
                file_path
            );
            return;
        };

        // Check if we've already fully initialized for this root - if so, skip everything
        {
            let init_root = self.initialized_root.read().await;
            if let Some(ref init) = *init_root {
                if init == &discovered_root {
                    debug!(
                        "Already initialized for root {:?}, skipping",
                        discovered_root
                    );
                    return;
                }
            }
        }

        // Get current root to compare
        let current_root_guard = self.root_path.read().await;
        let current_root = current_root_guard.as_ref();

        // Decide if we should use the discovered root
        let should_update = match current_root {
            None => {
                // No current root, so always use discovered
                debug!(
                    "No current root, using discovered root: {:?}",
                    discovered_root
                );
                true
            }
            Some(current) => {
                // Check if file is outside current root
                let file_outside_root = !file_path.starts_with(current);

                // Check if discovered root is more specific (nested within current root)
                let more_specific =
                    discovered_root.starts_with(current) && discovered_root != *current;

                if file_outside_root {
                    info!(
                        "File {:?} is outside current root {:?}, switching to discovered root: {:?}",
                        file_path, current, discovered_root
                    );
                    true
                } else if more_specific {
                    info!(
                        "Discovered more specific Laravel root {:?} (current: {:?})",
                        discovered_root, current
                    );
                    true
                } else {
                    // File is within current root and discovered isn't more specific
                    debug!(
                        "Keeping current root {:?} for file {:?}",
                        current, file_path
                    );
                    false
                }
            }
        };

        drop(current_root_guard);

        if !should_update {
            return;
        }

        info!("Updating Laravel project root to: {:?}", discovered_root);

        // Store the new root path
        *self.root_path.write().await = Some(discovered_root.clone());

        // Register config files with Salsa for incremental computation
        self.register_config_with_salsa(&discovered_root).await;

        // Verify config is available (checks memory cache first)
        match self.get_cached_config().await {
            Some(config) => {
                info!(
                    "Laravel configuration available: {} view paths",
                    config.view_paths.len()
                );

                // Register project files with Salsa for reference finding.
                // No progress UI here — this path runs in response to a
                // mid-session config change, where flashing a status-bar
                // entry for a re-index would feel surprising. The
                // user-facing first-load progress is wired into
                // `initialized()` instead.
                self.register_project_files_with_salsa(&discovered_root, None)
                    .await;

                // Re-validate all open documents since config changed (view paths, component paths, etc.)
                info!("Laravel LSP: Re-validating all open documents due to config change");
                let documents = self.documents.read().await;
                for (doc_uri, (doc_text, _version)) in documents.iter() {
                    self.validate_and_publish_diagnostics(doc_uri, doc_text)
                        .await;
                }
            }
            None => {
                info!("Failed to get Laravel config");
            }
        }

        // Initialize service provider registry with Salsa
        info!("========================================");
        info!(
            "🛡️  Initializing service provider registry from root: {:?}",
            discovered_root
        );
        info!("========================================");
        self.register_service_provider_files_with_salsa(&discovered_root)
            .await;

        // Build route name index across project / packages / framework
        info!("========================================");
        info!("🛣️  Building route index from root: {:?}", discovered_root);
        info!("========================================");
        self.rebuild_route_index(&discovered_root).await;

        // Initialize environment variables with Salsa
        info!("========================================");
        info!("📁 Initializing env cache from root: {:?}", discovered_root);
        info!("========================================");
        self.register_env_files_with_salsa(&discovered_root).await;

        // Cache validation rule names from Laravel framework
        info!("========================================");
        info!(
            "📋 Caching validation rules from root: {:?}",
            discovered_root
        );
        info!("========================================");
        self.cache_validation_rule_names(&discovered_root).await;

        // Initialize database schema provider
        info!("🗄️  Initializing database schema provider");
        self.init_database_schema_provider(&discovered_root).await;

        // Mark this root as fully initialized
        *self.initialized_root.write().await = Some(discovered_root);
    }

    /// Cache validation rule names from Laravel framework for context detection
    async fn cache_validation_rule_names(&self, root: &Path) {
        use laravel_lsp::validation_rules::LaravelRulesParser;

        let parser = LaravelRulesParser::new(root.to_path_buf());
        let rules = parser.parse_validation_rules();

        // Extract rule names with colon suffix for context detection
        let rule_names: Vec<String> = rules
            .iter()
            .filter(|r| r.has_params)
            .map(|r| format!("{}:", r.name))
            .collect();

        info!(
            "   📋 Cached {} validation rule names for context detection",
            rule_names.len()
        );

        *self.cached_validation_rule_names.write().await = rule_names;
    }

    /// Validate validation rules in source code and return diagnostics
    ///
    /// Finds validation rule strings like `'email' => 'required|email|exists:users,email'`
    /// and validates:
    /// - Rule names exist (built-in Laravel rules or custom rules)
    /// - Database tables exist (for exists:/unique:)
    /// - Database columns exist (for exists:/unique:)
    /// - Required parameters are provided
    async fn validate_validation_rules(&self, source: &str) -> Vec<Diagnostic> {
        use regex::Regex;

        let mut diagnostics = Vec::new();

        // Get known validation rules
        let known_rules = self.get_all_validation_rules().await;
        let known_rule_names: std::collections::HashSet<String> =
            known_rules.iter().map(|r| r.name.to_lowercase()).collect();

        // Regex to find validation rule strings in arrays
        // Matches: 'field' => 'rule|rule:param' or "field" => "rule|rule:param"
        // Also matches array syntax in Form Requests, Validator::make, etc.
        let rule_string_regex =
            Regex::new(r#"['"]([a-zA-Z_][a-zA-Z0-9_.*]*)['"]\s*=>\s*['"]([^'"]+)['"]"#).unwrap();

        // Track line positions
        let lines: Vec<&str> = source.lines().collect();
        let mut line_starts: Vec<usize> = Vec::with_capacity(lines.len() + 1);
        let mut pos = 0;
        for line in &lines {
            line_starts.push(pos);
            pos += line.len() + 1; // +1 for newline
        }
        line_starts.push(pos);

        // Helper to convert byte offset to line/column
        let byte_to_position = |byte_offset: usize| -> (u32, u32) {
            for (line_num, &start) in line_starts.iter().enumerate() {
                if line_num + 1 < line_starts.len() && byte_offset < line_starts[line_num + 1] {
                    return (line_num as u32, (byte_offset - start) as u32);
                }
            }
            (0, 0)
        };

        // Find all validation rule strings
        for cap in rule_string_regex.captures_iter(source) {
            let _field_name = cap.get(1).map(|m| m.as_str()).unwrap_or("");
            let rules_string = cap.get(2).map(|m| m.as_str()).unwrap_or("");
            let rules_match = cap.get(2).unwrap();
            let rules_start = rules_match.start();

            // Skip if this doesn't look like validation rules (heuristic)
            // Must contain at least one known rule or a pipe
            let looks_like_rules = rules_string.contains('|')
                || rules_string.split('|').any(|r| {
                    let rule_name = r.split(':').next().unwrap_or("").to_lowercase();
                    known_rule_names.contains(&rule_name)
                });

            if !looks_like_rules {
                continue;
            }

            // Check for leading pipe
            if rules_string.starts_with('|') {
                let (line, col) = byte_to_position(rules_start);
                diagnostics.push(Diagnostic {
                    range: Range {
                        start: Position {
                            line,
                            character: col,
                        },
                        end: Position {
                            line,
                            character: col + 1,
                        },
                    },
                    severity: Some(DiagnosticSeverity::ERROR),
                    code: None,
                    source: Some("laravel".to_string()),
                    message: "Unexpected leading '|' in validation rules".to_string(),
                    related_information: None,
                    tags: None,
                    code_description: None,
                    data: None,
                });
            }

            // Check for trailing pipe
            if rules_string.ends_with('|') {
                let pipe_offset = rules_start + rules_string.len() - 1;
                let (line, col) = byte_to_position(pipe_offset);
                diagnostics.push(Diagnostic {
                    range: Range {
                        start: Position {
                            line,
                            character: col,
                        },
                        end: Position {
                            line,
                            character: col + 1,
                        },
                    },
                    severity: Some(DiagnosticSeverity::ERROR),
                    code: None,
                    source: Some("laravel".to_string()),
                    message: "Unexpected trailing '|' in validation rules".to_string(),
                    related_information: None,
                    tags: None,
                    code_description: None,
                    data: None,
                });
            }

            // Check for consecutive pipes (empty rule)
            if rules_string.contains("||") {
                let mut search_start = 0;
                while let Some(pos) = rules_string[search_start..].find("||") {
                    let pipe_offset = rules_start + search_start + pos;
                    let (line, col) = byte_to_position(pipe_offset);
                    diagnostics.push(Diagnostic {
                        range: Range {
                            start: Position {
                                line,
                                character: col,
                            },
                            end: Position {
                                line,
                                character: col + 2,
                            },
                        },
                        severity: Some(DiagnosticSeverity::ERROR),
                        code: None,
                        source: Some("laravel".to_string()),
                        message: "Empty validation rule between '||'".to_string(),
                        related_information: None,
                        tags: None,
                        code_description: None,
                        data: None,
                    });
                    search_start += pos + 2;
                }
            }

            // Parse individual rules (split by |)
            let mut current_offset = 0;
            for rule_part in rules_string.split('|') {
                let rule_offset = rules_start + current_offset; // rules_start already points to content
                current_offset += rule_part.len() + 1; // +1 for pipe

                if rule_part.is_empty() {
                    continue; // Already handled above with leading/trailing/consecutive checks
                }

                // Parse rule name and params
                let (rule_name, params) = if let Some(colon_pos) = rule_part.find(':') {
                    (&rule_part[..colon_pos], Some(&rule_part[colon_pos + 1..]))
                } else {
                    (rule_part, None)
                };

                let rule_name_lower = rule_name.to_lowercase();

                // Check if rule exists
                if !known_rule_names.contains(&rule_name_lower) {
                    let (line, col) = byte_to_position(rule_offset);
                    diagnostics.push(Diagnostic {
                        range: Range {
                            start: Position {
                                line,
                                character: col,
                            },
                            end: Position {
                                line,
                                character: col + rule_name.len() as u32,
                            },
                        },
                        severity: Some(DiagnosticSeverity::ERROR),
                        code: None,
                        source: Some("laravel".to_string()),
                        message: format!("Unknown validation rule: '{}'", rule_name),
                        related_information: None,
                        tags: None,
                        code_description: None,
                        data: None,
                    });
                    continue;
                }

                // Validate database rules (exists/unique)
                if rule_name_lower == "exists" || rule_name_lower == "unique" {
                    if let Some(params_str) = params {
                        // Parse table and column
                        let param_parts: Vec<&str> = params_str.split(',').collect();
                        let table_param = param_parts.first().map(|s| s.trim()).unwrap_or("");

                        if table_param.is_empty() {
                            let (line, col) = byte_to_position(rule_offset);
                            diagnostics.push(Diagnostic {
                                range: Range {
                                    start: Position {
                                        line,
                                        character: col,
                                    },
                                    end: Position {
                                        line,
                                        character: col + rule_part.len() as u32,
                                    },
                                },
                                severity: Some(DiagnosticSeverity::ERROR),
                                code: None,
                                source: Some("laravel".to_string()),
                                message: format!("Rule '{}' requires a table name", rule_name),
                                related_information: None,
                                tags: None,
                                code_description: None,
                                data: None,
                            });
                            continue;
                        }

                        // Extract actual table name (handle connection.table and Model class)
                        let table_name: String =
                            if table_param.contains('.') && !table_param.contains('\\') {
                                // connection.table syntax
                                table_param
                                    .split('.')
                                    .nth(1)
                                    .unwrap_or(table_param)
                                    .to_string()
                            } else if table_param.contains('\\') {
                                // Model class - infer table name
                                table_param
                                    .rsplit('\\')
                                    .next()
                                    .map(|class| format!("{}s", class.to_lowercase()))
                                    .unwrap_or_else(|| table_param.to_string())
                            } else {
                                table_param.to_string()
                            };

                        // Check if table exists in database
                        let schema_guard = self.database_schema.read().await;
                        if let Some(ref provider) = *schema_guard {
                            let tables = provider.get_tables().await;

                            // Only validate if we have database connection
                            if !tables.is_empty() {
                                let table_name_ref: &str = if table_param.contains('\\') {
                                    // For model class, use inferred name
                                    &table_name
                                } else if table_param.contains('.') && !table_param.contains('\\') {
                                    table_param.split('.').nth(1).unwrap_or(table_param)
                                } else {
                                    table_param
                                };

                                if !tables
                                    .iter()
                                    .any(|t| t.eq_ignore_ascii_case(table_name_ref))
                                {
                                    let colon_offset = rule_offset + rule_name.len();
                                    let table_offset = colon_offset + 1; // +1 for colon
                                    let (line, col) = byte_to_position(table_offset);

                                    diagnostics.push(Diagnostic {
                                        range: Range {
                                            start: Position {
                                                line,
                                                character: col,
                                            },
                                            end: Position {
                                                line,
                                                character: col + table_param.len() as u32,
                                            },
                                        },
                                        severity: Some(DiagnosticSeverity::ERROR),
                                        code: None,
                                        source: Some("laravel".to_string()),
                                        message: format!(
                                            "Table '{}' not found in database",
                                            table_name_ref
                                        ),
                                        related_information: None,
                                        tags: None,
                                        code_description: None,
                                        data: None,
                                    });
                                } else if param_parts.len() > 1 {
                                    // Check column exists
                                    let column_param =
                                        param_parts.get(1).map(|s| s.trim()).unwrap_or("");
                                    if !column_param.is_empty() {
                                        let columns = provider.get_columns(table_name_ref).await;

                                        if !columns.is_empty()
                                            && !columns
                                                .iter()
                                                .any(|c| c.eq_ignore_ascii_case(column_param))
                                        {
                                            let colon_offset = rule_offset + rule_name.len();
                                            let column_offset =
                                                colon_offset + 1 + table_param.len() + 1; // colon + table + comma
                                            let (line, col) = byte_to_position(column_offset);

                                            diagnostics.push(Diagnostic {
                                                range: Range {
                                                    start: Position {
                                                        line,
                                                        character: col,
                                                    },
                                                    end: Position {
                                                        line,
                                                        character: col + column_param.len() as u32,
                                                    },
                                                },
                                                severity: Some(DiagnosticSeverity::ERROR),
                                                code: None,
                                                source: Some("laravel".to_string()),
                                                message: format!(
                                                    "Column '{}' not found in table '{}'",
                                                    column_param, table_name_ref
                                                ),
                                                related_information: None,
                                                tags: None,
                                                code_description: None,
                                                data: None,
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    } else {
                        // No params for exists/unique
                        let (line, col) = byte_to_position(rule_offset);
                        diagnostics.push(Diagnostic {
                            range: Range {
                                start: Position {
                                    line,
                                    character: col,
                                },
                                end: Position {
                                    line,
                                    character: col + rule_name.len() as u32,
                                },
                            },
                            severity: Some(DiagnosticSeverity::ERROR),
                            code: None,
                            source: Some("laravel".to_string()),
                            message: format!(
                                "Rule '{}' requires a table name parameter",
                                rule_name
                            ),
                            related_information: None,
                            tags: None,
                            code_description: None,
                            data: None,
                        });
                    }
                }
            }
        }

        diagnostics
    }

    /// Initialize the database schema provider for exists:/unique: validation rules
    async fn init_database_schema_provider(&self, root: &Path) {
        use laravel_lsp::database::DatabaseSchemaProvider;

        let provider = DatabaseSchemaProvider::new(root.to_path_buf());

        // Log config status but always store provider
        // Errors will be handled when completions are requested
        if let Some(config) = provider.get_database_config().await {
            info!(
                "   🗄️  Database config found: {} @ {}:{}",
                config.driver, config.host, config.port
            );
        } else {
            debug!("warn:  Database config not found - will show diagnostic on first use");
        }

        // Always store provider - errors handled when completions requested
        *self.database_schema.write().await = Some(provider);
    }

    /// Check if a file exists with async I/O and TTL caching
    ///
    /// This method improves goto_definition performance by:
    /// 1. Checking editor buffers first (for unsaved files)
    /// 2. Using a TTL cache (5 seconds) to avoid repeated disk I/O
    /// 3. Using async file I/O (tokio::fs) to avoid blocking the runtime
    async fn file_exists_cached(&self, path: &PathBuf) -> bool {
        const CACHE_TTL: Duration = Duration::from_secs(5);

        // First check if file is open in editor (includes unsaved files)
        if let Ok(uri) = Url::from_file_path(path) {
            let documents = self.documents.read().await;
            if documents.contains_key(&uri) {
                return true;
            }
        }

        // Check TTL cache
        {
            let cache = self.file_exists_cache.read().await;
            if let Some((exists, cached_at)) = cache.get(path) {
                if cached_at.elapsed() < CACHE_TTL {
                    return *exists;
                }
            }
        }

        // Cache miss - check disk asynchronously
        let exists = tokio::fs::metadata(path).await.is_ok();

        // Update cache
        self.file_exists_cache
            .write()
            .await
            .insert(path.clone(), (exists, Instant::now()));

        exists
    }

    /// Get Laravel config with local caching
    ///
    /// This avoids repeated Salsa lookups on every goto_definition request.
    /// Cache is invalidated when config files change (in did_save).
    async fn get_cached_config(&self) -> Option<LaravelConfigData> {
        // Return cached config if available
        if let Some(config) = self.cached_config.read().await.clone() {
            return Some(config);
        }

        // Fetch from Salsa and cache
        match self.salsa.get_laravel_config().await {
            Ok(Some(config)) => {
                *self.cached_config.write().await = Some(config.clone());
                Some(config)
            }
            _ => None,
        }
    }

    /// For a single file-tree rename, classify the file as either a view
    /// or a Blade component and collect the text edits needed to rewrite
    /// every call site that referenced the old name with the new name.
    ///
    /// Returns `None` when the file pair doesn't match any handled kind
    /// (e.g., not a `.blade.php`, not under a configured `view_paths`)
    /// — the caller skips it without erroring. Returns `Some(vec)` even
    /// when the vec is empty (the symbol had no call sites) so the
    /// caller can distinguish "wasn't a known kind" from "known kind,
    /// nothing to do".
    async fn collect_will_rename_targets(
        &self,
        file_rename: &FileRename,
        config: &LaravelConfigData,
    ) -> Option<Vec<laravel_lsp::rename::EditTarget>> {
        let old_path = Url::parse(&file_rename.old_uri).ok()?.to_file_path().ok()?;
        let new_path = Url::parse(&file_rename.new_uri).ok()?.to_file_path().ok()?;
        debug!(
            "collect_will_rename_targets: {} → {}",
            old_path.display(),
            new_path.display()
        );

        // Try View first — `view_name_for_path` refuses
        // `components/`-rooted paths so component renames don't get
        // misclassified.
        if let Some(old_name) =
            laravel_lsp::view_declaration_locator::view_name_for_path(&old_path, config)
        {
            debug!("classified as VIEW: old name = '{}'", old_name);
            let Some(new_name) =
                laravel_lsp::view_declaration_locator::view_name_for_path(&new_path, config)
            else {
                debug!("warn:  new_path doesn't classify as a view — refusing");
                return None;
            };
            debug!("new view name = '{}'", new_name);
            if old_name == new_name {
                debug!("  old == new, no edits needed");
                return Some(Vec::new());
            }
            let edits = self
                .collect_call_site_edits_for_symbol(
                    laravel_lsp::salsa_impl::SymbolRefData::View(old_name),
                    new_name,
                )
                .await;
            debug!("produced {} text edits", edits.len());
            return Some(edits);
        }

        // Then try Blade component.
        if let Some(old_name) =
            laravel_lsp::component_declaration_locator::component_name_for_blade_path(
                &old_path, config,
            )
        {
            info!(
                "      ↪️  classified as COMPONENT: old name = '{}'",
                old_name
            );
            let Some(new_name) =
                laravel_lsp::component_declaration_locator::component_name_for_blade_path(
                    &new_path, config,
                )
            else {
                debug!("warn:  new_path doesn't classify as a component — refusing");
                return None;
            };
            debug!("new component name = '{}'", new_name);
            if old_name == new_name {
                debug!("  old == new, no edits needed");
                return Some(Vec::new());
            }
            // Component call sites carry the full `x-name` range and
            // need the `x-` prefix preserved on rewrite. Override the
            // text after collecting.
            let mut edits = self
                .collect_call_site_edits_for_symbol(
                    laravel_lsp::salsa_impl::SymbolRefData::Component(old_name),
                    new_name.clone(),
                )
                .await;
            let prefixed = format!("x-{}", new_name);
            for edit in &mut edits {
                edit.new_text = prefixed.clone();
            }
            info!(
                "      ✅ produced {} text edits (prefixed: '{}')",
                edits.len(),
                prefixed
            );
            return Some(edits);
        }

        debug!("  no classifier matched");
        None
    }

    /// Find every call site of `symbol` via Salsa and return one
    /// `EditTarget` per site, all rewriting to `new_text`. Used by both
    /// `rename` (with the user's typed new name) and `will_rename_files`
    /// (with a name derived from the renamed file path).
    async fn collect_call_site_edits_for_symbol(
        &self,
        symbol: laravel_lsp::salsa_impl::SymbolRefData,
        new_text: String,
    ) -> Vec<laravel_lsp::rename::EditTarget> {
        match self.salsa.find_references(symbol, true).await {
            Ok(refs) => refs
                .into_iter()
                .map(|r| laravel_lsp::rename::EditTarget {
                    file_path: r.file_path,
                    line: r.line,
                    start_column: r.column,
                    end_column: r.end_column,
                    new_text: new_text.clone(),
                })
                .collect(),
            Err(e) => {
                debug!("will_rename_files: find_references error {:?}", e);
                Vec::new()
            }
        }
    }

    /// Load `config/livewire.php` and parse it into a [`LivewireConfig`]
    /// against the given project root. Falls back to the v4 ship defaults
    /// when the file is missing or unreadable — Phase 3c never errors on
    /// the absence of a Livewire config, just degrades to defaults.
    async fn load_livewire_config(
        &self,
        root: &Path,
    ) -> laravel_lsp::livewire_config::LivewireConfig {
        let path = root.join("config/livewire.php");
        let source = tokio::fs::read_to_string(&path).await.unwrap_or_default();
        laravel_lsp::livewire_config::parse(&source, root)
    }

    /// Read `composer.lock` and return the detected Livewire major version.
    /// Returns `Unknown` when the lock is missing or doesn't carry a
    /// `livewire/livewire` entry — the resolver treats Unknown as
    /// "try v4 paths first, then v3" so this is a safe default.
    async fn detect_livewire_version(
        &self,
        root: &Path,
    ) -> laravel_lsp::livewire_version::LivewireVersion {
        let path = root.join("composer.lock");
        let json = tokio::fs::read_to_string(&path).await.unwrap_or_default();
        laravel_lsp::livewire_version::detect_from_composer_lock(&json)
    }

    /// Lazy + cached load of (LivewireConfig, LivewireVersion) for the
    /// current project root. Reused by every Livewire goto / hover /
    /// diagnostic — parsing `config/livewire.php` and scanning
    /// `composer.lock` on every call would be wasteful for what's
    /// effectively immutable per-project state.
    ///
    /// Returns `None` only when the project root hasn't been determined
    /// yet (first request before `initialize`).
    async fn get_cached_livewire(
        &self,
    ) -> Option<(
        laravel_lsp::livewire_config::LivewireConfig,
        laravel_lsp::livewire_version::LivewireVersion,
    )> {
        let root = self.root_path.read().await.clone()?;
        {
            let guard = self.cached_livewire.read().await;
            if let Some((cached_root, cfg, ver)) = guard.as_ref() {
                if cached_root == &root {
                    return Some((cfg.clone(), *ver));
                }
            }
        }
        let cfg = self.load_livewire_config(&root).await;
        let ver = self.detect_livewire_version(&root).await;
        *self.cached_livewire.write().await = Some((root, cfg.clone(), ver));
        Some((cfg, ver))
    }

    /// Resolve a Livewire-component name to its full
    /// [`livewire_resolver::LivewireComponent`] (kind + every file path
    /// that participates). The new-resolver entry point goto / hover /
    /// diagnostics route through to find v4 SFC and MFC components,
    /// not just the legacy `app/Livewire/{Pascal}.php` shape the older
    /// [`LaravelConfigData::resolve_livewire_path`] handled.
    ///
    /// Returns `None` when no on-disk file matches the name (the
    /// component genuinely doesn't exist) or when configs aren't loaded.
    async fn resolve_livewire_component(
        &self,
        name: &str,
    ) -> Option<laravel_lsp::livewire_resolver::LivewireComponent> {
        let (cfg, ver) = self.get_cached_livewire().await?;
        laravel_lsp::livewire_resolver::resolve_component(name, &cfg, ver)
    }

    /// Backwards-compatible single-path resolver for callers that just
    /// want "the file" backing a Livewire component (goto, hover). For
    /// V4 MFC this returns the directory; for everything else it returns
    /// the primary file (blade for SFC/Volt, class file for V3 Class).
    async fn resolve_livewire_primary_path(&self, name: &str) -> Option<PathBuf> {
        let component = self.resolve_livewire_component(name).await?;
        component.paths.into_iter().next()
    }

    /// Invalidate the local config cache
    /// Call this when config files change (composer.json, config/*.php)
    async fn invalidate_config_cache(&self) {
        *self.cached_config.write().await = None;
        // Livewire's cached config + version share the same invalidation
        // surface — they depend on `config/livewire.php` and
        // `composer.lock`, both of which change in tandem with the
        // general Laravel config layer.
        *self.cached_livewire.write().await = None;
    }

    /// Get middleware from cache first, then Salsa
    /// Returns (class_name, class_file, source_file, source_line)
    /// - class_file: for checking if the middleware class exists
    /// - source_file + source_line: for navigation to alias declaration
    ///
    /// Strips parameters from the middleware name before lookup — Laravel
    /// middleware can be invoked as `auth:sanctum` or `throttle:60,1`, where
    /// the part after `:` is passed as parameters to the middleware's
    /// `handle()` method, not part of the alias.
    async fn get_cached_middleware(
        &self,
        name: &str,
    ) -> Option<(String, Option<PathBuf>, Option<PathBuf>, Option<u32>)> {
        let base_alias = middleware_base_alias(name);

        // First check disk cache (instant)
        if let Some(ref cache) = *self.cache.read().await {
            let all_middleware = cache.get_all_middleware();
            if let Some(entry) = all_middleware.get(base_alias) {
                return Some((
                    entry.class.clone(),
                    entry.class_file.as_ref().map(PathBuf::from),
                    entry.source_file.as_ref().map(PathBuf::from),
                    Some(entry.line),
                ));
            }
        }

        // Fall back to Salsa (may not be ready yet)
        if let Ok(Some(mw_data)) = self
            .salsa
            .get_parsed_middleware(base_alias.to_string())
            .await
        {
            return Some((
                mw_data.class_name.clone(),
                mw_data.file_path.clone(),
                Some(mw_data.source_file.clone()),
                Some(mw_data.source_line),
            ));
        }

        None
    }

    /// Get binding from cache first, then Salsa
    /// Returns (class_name, class_file, source_file, source_line)
    /// - class_file: for checking if the concrete class exists
    /// - source_file + source_line: for navigation to binding declaration
    async fn get_cached_binding(
        &self,
        name: &str,
    ) -> Option<(String, Option<PathBuf>, Option<PathBuf>, Option<u32>)> {
        // First check disk cache (instant)
        if let Some(ref cache) = *self.cache.read().await {
            let all_bindings = cache.get_all_bindings();
            if let Some(entry) = all_bindings.get(name) {
                return Some((
                    entry.class.clone(),
                    entry.class_file.as_ref().map(PathBuf::from),
                    entry.source_file.as_ref().map(PathBuf::from),
                    Some(entry.line),
                ));
            }
        }

        // Fall back to Salsa (may not be ready yet)
        if let Ok(Some(binding_data)) = self.salsa.get_parsed_binding(name.to_string()).await {
            return Some((
                binding_data.concrete_class.clone(),
                binding_data.file_path.clone(),
                Some(binding_data.source_file.clone()),
                Some(binding_data.source_line),
            ));
        }

        None
    }

    // ========================================================================
    // Debounced Salsa Updates (Cache Invalidation Architecture)
    // ========================================================================

    /// Queue a debounced Salsa update for a file
    ///
    /// This is the core of the cache invalidation architecture:
    /// `did_change(file) → Debounce (configurable) → Update Salsa input → Queries recompute → UI updates`
    ///
    /// The debounce prevents excessive Salsa updates during rapid typing.
    /// After the debounce delay (default 250ms, configurable via settings),
    /// the file is updated in Salsa which triggers incremental recomputation
    /// of all affected queries.
    async fn queue_salsa_update(&self, uri: Url, content: String, version: i32) {
        let debounce_ms = *self.auto_complete_debounce_ms.read().await;
        let debounce_delay = Duration::from_millis(debounce_ms);

        // Cancel any existing pending Salsa update for this file
        if let Some(handle) = self.pending_salsa_updates.write().await.remove(&uri) {
            handle.abort();
        }

        // Clone values needed for the async task
        let uri_for_spawn = uri.clone();
        let server = self.clone_for_spawn();

        // Spawn a task that updates Salsa after debounce delay
        let handle = tokio::spawn(async move {
            // Wait for the debounce delay
            sleep(debounce_delay).await;

            debug!(
                "⏰ Salsa debounce expired for {} - updating Salsa",
                uri_for_spawn
            );

            // Execute the Salsa update based on file type
            server
                .execute_salsa_update(&uri_for_spawn, &content, version)
                .await;
        });

        // Store the task handle so we can cancel it if needed
        self.pending_salsa_updates.write().await.insert(uri, handle);
    }

    /// Execute a Salsa update based on file type
    ///
    /// Determines the file type and calls the appropriate Salsa update method:
    /// - SourceFile: PHP and Blade files (pattern extraction)
    /// - ConfigFile: config/*.php, composer.json (view paths, namespaces)
    /// - EnvFile: .env, .env.local, .env.example (environment variables)
    /// - ServiceProviderFile: bootstrap/app.php, Providers/*.php (middleware, bindings)
    async fn execute_salsa_update(&self, uri: &Url, content: &str, version: i32) {
        let path = match uri.to_file_path() {
            Ok(p) => p,
            Err(_) => return,
        };

        let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let path_str = path.to_string_lossy();

        // Get root path for service provider registration
        let root_path = self.root_path.read().await.clone();

        // Determine file type and update appropriate Salsa input
        if filename == "app.php" && path_str.contains("bootstrap") {
            // bootstrap/app.php - Service provider file (middleware aliases)
            if let Some(root) = root_path {
                debug!("📦 Updating Salsa: ServiceProviderFile (bootstrap/app.php)");
                if let Err(e) = self
                    .salsa
                    .register_service_provider_source(
                        path.clone(),
                        content.to_string(),
                        2, // priority: app = 2
                        root,
                    )
                    .await
                {
                    debug!("Failed to update service provider in Salsa: {}", e);
                }
            }
        } else if path_str.contains("app/Providers") && filename.ends_with(".php") {
            // App service provider - Service provider file
            if let Some(root) = root_path {
                debug!("📦 Updating Salsa: ServiceProviderFile ({})", filename);
                if let Err(e) = self
                    .salsa
                    .register_service_provider_source(
                        path.clone(),
                        content.to_string(),
                        2, // priority: app = 2
                        root,
                    )
                    .await
                {
                    debug!("Failed to update service provider in Salsa: {}", e);
                }
            }
        } else if filename.starts_with(".env") {
            // Env file (.env, .env.local, .env.example)
            let priority = match filename {
                ".env" => 2,
                ".env.local" => 1,
                _ => 0, // .env.example
            };
            debug!(
                "📦 Updating Salsa: EnvFile ({}, priority={})",
                filename, priority
            );
            if let Err(e) = self
                .salsa
                .register_env_source(path.clone(), content.to_string(), priority)
                .await
            {
                debug!("Failed to update env file in Salsa: {}", e);
            }
        } else if path_str.contains("/config/") && filename.ends_with(".php") {
            // Config file (config/*.php) - needs BOTH ConfigFile AND SourceFile treatment
            // ConfigFile: for config discovery (view paths, namespaces, etc.)
            // SourceFile: for pattern extraction (env() calls, etc.)
            debug!("📦 Updating Salsa: ConfigFile ({})", filename);
            if let Err(e) = self
                .salsa
                .update_config_file(path.clone(), content.to_string())
                .await
            {
                debug!("Failed to update config file in Salsa: {}", e);
            }
            // Also invalidate the cached config so next lookup refetches
            *self.cached_config.write().await = None;
            // Also update as SourceFile for pattern extraction (env() diagnostics)
            debug!(
                "📦 Updating Salsa: SourceFile ({}) for pattern extraction",
                filename
            );
            if let Err(e) = self
                .salsa
                .update_file(path.clone(), version, content.to_string())
                .await
            {
                debug!("Failed to update source file in Salsa: {}", e);
            }
        } else if filename == "composer.json" {
            // composer.json - Config file
            debug!("📦 Updating Salsa: ConfigFile (composer.json)");
            if let Err(e) = self
                .salsa
                .update_config_file(path.clone(), content.to_string())
                .await
            {
                debug!("Failed to update config file in Salsa: {}", e);
            }
            // Also invalidate the cached config so next lookup refetches
            *self.cached_config.write().await = None;
        } else if filename.ends_with(".php") || filename.ends_with(".blade.php") {
            // Source file (PHP or Blade) - pattern extraction
            debug!("📦 Updating Salsa: SourceFile ({})", filename);
            if let Err(e) = self
                .salsa
                .update_file(path.clone(), version, content.to_string())
                .await
            {
                debug!("Failed to update source file in Salsa: {}", e);
            }
        }

        // After Salsa update, re-run diagnostics for this file
        // This ensures diagnostics reflect the latest Salsa state
        self.validate_and_publish_diagnostics(uri, content).await;
    }

    // ========================================================================
    // Tree-sitter-based helper functions
    // ========================================================================

    /// Extract view name from directive arguments
    /// e.g., "('layouts.app')" → "layouts.app"
    fn extract_view_from_directive_args(args: &str) -> Option<String> {
        // Remove parentheses and quotes
        let trimmed = args.trim().trim_matches('(').trim_matches(')').trim();
        let unquoted = trimmed.trim_matches('\'').trim_matches('"');

        if !unquoted.is_empty() && !unquoted.contains(',') {
            Some(unquoted.to_string())
        } else {
            None
        }
    }

    /// Convert kebab-case to PascalCase
    /// e.g., "user-profile" → "UserProfile"
    fn kebab_to_pascal_case(s: &str) -> String {
        s.split('-')
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    None => String::new(),
                    Some(first) => first.to_uppercase().chain(chars).collect(),
                }
            })
            .collect()
    }

    // ========================================================================
    // Completion helpers
    // ========================================================================

    /// Find the end of a string starting at a given position
    /// Returns the column of the closing quote, or the end of line if not found
    fn find_string_end(line_text: &str, start_col: usize, quote_char: char) -> u32 {
        let after_start = &line_text[start_col..];
        if let Some(end_offset) = after_start.find(quote_char) {
            (start_col + end_offset) as u32
        } else {
            line_text.len() as u32
        }
    }

    /// Check if cursor is inside env('...') or env("...") in PHP/Blade
    /// Returns context with position info for text replacement
    ///
    /// Example: `env('APP_` with cursor at end returns Some(StringContext{prefix: "APP_", ...})
    fn get_env_call_context(line_text: &str, character: u32) -> Option<StringContext> {
        let cursor = character as usize;
        if cursor > line_text.len() {
            return None;
        }

        let before_cursor = &line_text[..cursor];

        // Look for env(' or env(" before cursor
        // Find the last occurrence in case there are multiple on the line
        let env_single = before_cursor.rfind("env('");
        let env_double = before_cursor.rfind("env(\"");

        let (start_pos, quote_char) = match (env_single, env_double) {
            (Some(s), Some(d)) => {
                if s > d {
                    (s + 5, '\'')
                } else {
                    (d + 5, '"')
                }
            }
            (Some(s), None) => (s + 5, '\''),
            (None, Some(d)) => (d + 5, '"'),
            (None, None) => return None,
        };

        // Check that there's no closing quote between start and cursor
        let after_env = &before_cursor[start_pos..];
        if after_env.contains(quote_char) {
            return None;
        }

        // Find where the string ends (closing quote or end of line)
        let end_col = Self::find_string_end(line_text, start_pos, quote_char);

        Some(StringContext {
            prefix: after_env.to_string(),
            start_col: start_pos as u32,
            end_col,
            quote_char,
        })
    }

    /// Check if cursor is inside ${...} in .env files
    /// Returns StringContext with position info for text_edit
    ///
    /// Example: `NEW_VAR=${APP` with cursor at end returns context with prefix="APP"
    fn get_env_interpolation_context(line_text: &str, character: u32) -> Option<StringContext> {
        let cursor = character as usize;
        if cursor > line_text.len() {
            return None;
        }

        let before_cursor = &line_text[..cursor];

        // Look for ${ before cursor (last occurrence)
        let start_pos = before_cursor.rfind("${")? + 2;

        // Check that there's no closing } between start and cursor
        let after_interpolation = &before_cursor[start_pos..];
        if after_interpolation.contains('}') {
            return None;
        }

        // Find the end of the interpolation (closing } or end of line)
        let end_col = if let Some(close_pos) = line_text[start_pos..].find('}') {
            (start_pos + close_pos) as u32
        } else {
            line_text.len() as u32
        };

        Some(StringContext {
            prefix: after_interpolation.to_string(),
            start_col: start_pos as u32,
            end_col,
            quote_char: '{', // Not really a quote, but indicates interpolation
        })
    }

    /// Check if cursor is inside <env name="..."> in PHPUnit XML files
    /// Returns StringContext with position info for text_edit
    ///
    /// Example: `<env name="APP_` with cursor at end returns context with prefix="APP_"
    fn get_phpunit_env_context(line_text: &str, character: u32) -> Option<StringContext> {
        let cursor = character as usize;
        if cursor > line_text.len() {
            return None;
        }

        let before_cursor = &line_text[..cursor];

        // Look for <env name=" before cursor (last occurrence)
        // Handle both <env name=" and <server name=" (PHPUnit uses both)
        let env_pattern = before_cursor.rfind("<env name=\"");
        let server_pattern = before_cursor.rfind("<server name=\"");

        let start_pos = match (env_pattern, server_pattern) {
            (Some(e), Some(s)) => {
                if e > s {
                    e + 11
                } else {
                    s + 14
                } // "<env name=\"" = 11, "<server name=\"" = 14
            }
            (Some(e), None) => e + 11,
            (None, Some(s)) => s + 14,
            (None, None) => return None,
        };

        // Check that there's no closing " between start and cursor
        let after_pattern = &before_cursor[start_pos..];
        if after_pattern.contains('"') {
            return None;
        }

        // Find the end of the attribute value (closing " or end of line)
        let end_col = Self::find_string_end(line_text, start_pos, '"');

        Some(StringContext {
            prefix: after_pattern.to_string(),
            start_col: start_pos as u32,
            end_col,
            quote_char: '"',
        })
    }

    /// Check if cursor is inside config('...') or Config::get('...') calls
    /// Returns the partial text typed so far (for filtering completions)
    ///
    /// Examples:
    /// - `config('app.` returns Some("app.")
    /// - `Config::get('db.` returns Some("db.")
    /// - `Config::string('app.` returns Some("app.")
    fn get_config_call_context(line_text: &str, character: u32) -> Option<StringContext> {
        let cursor = character as usize;
        if cursor > line_text.len() {
            return None;
        }

        let before_cursor = &line_text[..cursor];

        // Look for various config patterns before cursor
        // config(' or config("
        let config_single = before_cursor.rfind("config('");
        let config_double = before_cursor.rfind("config(\"");

        // Config::get(' or Config::get("
        let facade_get_single = before_cursor.rfind("Config::get('");
        let facade_get_double = before_cursor.rfind("Config::get(\"");

        // Config::string(', Config::integer(', Config::boolean(', Config::array('
        let facade_string_single = before_cursor.rfind("Config::string('");
        let facade_string_double = before_cursor.rfind("Config::string(\"");
        let facade_integer_single = before_cursor.rfind("Config::integer('");
        let facade_integer_double = before_cursor.rfind("Config::integer(\"");
        let facade_boolean_single = before_cursor.rfind("Config::boolean('");
        let facade_boolean_double = before_cursor.rfind("Config::boolean(\"");
        let facade_array_single = before_cursor.rfind("Config::array('");
        let facade_array_double = before_cursor.rfind("Config::array(\"");

        // Find the latest match and its corresponding quote character
        let matches: Vec<(usize, char, usize)> = vec![
            (config_single.unwrap_or(0), '\'', 8),          // config('
            (config_double.unwrap_or(0), '"', 8),           // config("
            (facade_get_single.unwrap_or(0), '\'', 13),     // Config::get('
            (facade_get_double.unwrap_or(0), '"', 13),      // Config::get("
            (facade_string_single.unwrap_or(0), '\'', 16),  // Config::string('
            (facade_string_double.unwrap_or(0), '"', 16),   // Config::string("
            (facade_integer_single.unwrap_or(0), '\'', 17), // Config::integer('
            (facade_integer_double.unwrap_or(0), '"', 17),  // Config::integer("
            (facade_boolean_single.unwrap_or(0), '\'', 17), // Config::boolean('
            (facade_boolean_double.unwrap_or(0), '"', 17),  // Config::boolean("
            (facade_array_single.unwrap_or(0), '\'', 15),   // Config::array('
            (facade_array_double.unwrap_or(0), '"', 15),    // Config::array("
        ];

        // Filter to only actual matches (position > 0 or the pattern was actually found at 0)
        let actual_matches: Vec<(usize, char, usize)> = matches
            .into_iter()
            .filter(|(pos, quote, len)| {
                if *pos == 0 {
                    // Check if pattern actually exists at position 0
                    let patterns = [
                        ("config('", '\'', 8),
                        ("config(\"", '"', 8),
                        ("Config::get('", '\'', 13),
                        ("Config::get(\"", '"', 13),
                        ("Config::string('", '\'', 16),
                        ("Config::string(\"", '"', 16),
                        ("Config::integer('", '\'', 17),
                        ("Config::integer(\"", '"', 17),
                        ("Config::boolean('", '\'', 17),
                        ("Config::boolean(\"", '"', 17),
                        ("Config::array('", '\'', 15),
                        ("Config::array(\"", '"', 15),
                    ];
                    patterns.iter().any(|(pat, q, l)| {
                        before_cursor.starts_with(pat) && *q == *quote && *l == *len
                    })
                } else {
                    true
                }
            })
            .collect();

        if actual_matches.is_empty() {
            return None;
        }

        // Find the latest match
        let (pos, quote_char, pattern_len) =
            actual_matches.into_iter().max_by_key(|(p, _, _)| *p)?;

        let start_pos = pos + pattern_len;

        // Check that there's no closing quote between start and cursor
        let after_pattern = &before_cursor[start_pos..];
        if after_pattern.contains(quote_char) {
            return None;
        }

        // Find where the string ends (closing quote or end of line)
        let end_col = Self::find_string_end(line_text, start_pos, quote_char);

        Some(StringContext {
            prefix: after_pattern.to_string(),
            start_col: start_pos as u32,
            end_col,
            quote_char,
        })
    }

    /// Check if cursor is inside route('...'), to_route('...'), or other route-related calls
    /// Returns the partial text typed so far (for filtering completions)
    ///
    /// Examples:
    /// - `route('users.` returns Some("users.")
    /// - `to_route('admin.` returns Some("admin.")
    /// - `redirect()->route('` returns Some("")
    fn get_route_call_context(line_text: &str, character: u32) -> Option<StringContext> {
        let cursor = character as usize;
        if cursor > line_text.len() {
            return None;
        }

        let before_cursor = &line_text[..cursor];

        // Look for various route patterns before cursor
        // Pattern: (pattern_string, quote_char, pattern_length)
        let patterns: Vec<(&str, char, usize)> = vec![
            // route(' or route("
            ("route('", '\'', 7),
            ("route(\"", '"', 7),
            // to_route(' or to_route("
            ("to_route('", '\'', 10),
            ("to_route(\"", '"', 10),
            // ->route(' (for redirect()->route)
            ("->route('", '\'', 9),
            ("->route(\"", '"', 9),
            // URL::route('
            ("URL::route('", '\'', 12),
            ("URL::route(\"", '"', 12),
            // Route::has('
            ("Route::has('", '\'', 12),
            ("Route::has(\"", '"', 12),
            // Route::is('
            ("Route::is('", '\'', 11),
            ("Route::is(\"", '"', 11),
            // Route::currentRouteNamed('
            ("Route::currentRouteNamed('", '\'', 26),
            ("Route::currentRouteNamed(\"", '"', 26),
            // ->routeIs('
            ("->routeIs('", '\'', 11),
            ("->routeIs(\"", '"', 11),
            // ->named('
            ("->named('", '\'', 9),
            ("->named(\"", '"', 9),
        ];

        // Find all matches and their positions
        let mut matches: Vec<(usize, char, usize)> = Vec::new();

        for (pattern, quote, len) in &patterns {
            if let Some(pos) = before_cursor.rfind(pattern) {
                matches.push((pos, *quote, *len));
            }
        }

        if matches.is_empty() {
            return None;
        }

        // Find the latest match (closest to cursor)
        let (pos, quote_char, pattern_len) = matches.into_iter().max_by_key(|(p, _, _)| *p)?;

        let start_pos = pos + pattern_len;

        // Check that there's no closing quote between start and cursor
        let after_pattern = &before_cursor[start_pos..];
        if after_pattern.contains(quote_char) {
            return None;
        }

        // Find where the string ends (closing quote or end of line)
        let end_col = Self::find_string_end(line_text, start_pos, quote_char);

        Some(StringContext {
            prefix: after_pattern.to_string(),
            start_col: start_pos as u32,
            end_col,
            quote_char,
        })
    }

    /// Check if cursor is inside ->middleware('...') or similar middleware calls
    /// Returns context with position info for text replacement
    ///
    /// Examples:
    /// - `->middleware('` returns Some(StringContext{prefix: "", ...})
    /// - `->middleware('auth` returns Some(StringContext{prefix: "auth", ...})
    /// - `->middleware(['auth', '` returns Some(StringContext{prefix: "", ...})
    ///
    /// The `previous_lines` parameter allows detecting multi-line middleware arrays:
    /// ```php
    /// 'middleware' => [
    ///     'api',
    ///     '  <-- cursor here, context found from previous lines
    /// ```
    fn get_middleware_call_context(
        line_text: &str,
        character: u32,
        previous_lines: Option<&[&str]>,
    ) -> Option<StringContext> {
        let cursor = character as usize;
        if cursor > line_text.len() {
            return None;
        }

        let before_cursor = &line_text[..cursor];

        // Look for various middleware patterns before cursor
        // Pattern: (pattern_string, quote_char, pattern_length)
        let patterns: Vec<(&str, char, usize)> = vec![
            // ->middleware('
            ("->middleware('", '\'', 14),
            ("->middleware(\"", '"', 14),
            // ->middleware([' (array syntax - first element)
            ("->middleware(['", '\'', 15),
            ("->middleware([\"", '"', 15),
            // 'middleware' => [' (array-key syntax in Route::group config)
            ("'middleware' => ['", '\'', 18),
            ("'middleware' => [\"", '"', 18),
            ("\"middleware\" => ['", '\'', 18),
            ("\"middleware\" => [\"", '"', 18),
            // ', ' inside middleware array (subsequent elements)
            ("', '", '\'', 4),
            ("\", \"", '"', 4),
            // ',' without space (also common)
            (",'", '\'', 2),
            (",\"", '"', 2),
            // Route::middleware('
            ("Route::middleware('", '\'', 19),
            ("Route::middleware(\"", '"', 19),
            // withMiddleware('
            ("withMiddleware('", '\'', 16),
            ("withMiddleware(\"", '"', 16),
            // ->withoutMiddleware('
            ("->withoutMiddleware('", '\'', 21),
            ("->withoutMiddleware(\"", '"', 21),
            // Standalone array element at start of line (for multi-line arrays)
            // Matches: '  (with leading whitespace) at line start
            ("'", '\'', 1),
            ("\"", '"', 1),
        ];

        // Find all matches and their positions
        let mut matches: Vec<(usize, char, usize)> = Vec::new();

        for (pattern, quote, len) in &patterns {
            if let Some(pos) = before_cursor.rfind(pattern) {
                // For single-quote patterns at position 0, only match if line starts with whitespace + quote
                if *len == 1 {
                    // Check if this is at line start (after optional whitespace)
                    let trimmed_start = line_text.len() - line_text.trim_start().len();
                    if pos == trimmed_start {
                        matches.push((pos, *quote, *len));
                    }
                } else {
                    matches.push((pos, *quote, *len));
                }
            }
        }

        if matches.is_empty() {
            return None;
        }

        // Find the latest match (closest to cursor)
        let (pos, quote_char, pattern_len) = matches.into_iter().max_by_key(|(p, _, _)| *p)?;

        let start_pos = pos + pattern_len;

        // Check that there's no closing quote between start and cursor
        let after_pattern = &before_cursor[start_pos..];
        if after_pattern.contains(quote_char) {
            return None;
        }

        // For array patterns like "', '" or standalone quotes, verify we're actually in a middleware context
        // by checking if ->middleware( or similar appears earlier in the line OR in previous lines
        if pattern_len <= 4 {
            let middleware_indicators = [
                "->middleware(",
                "->middleware([",
                "Route::middleware(",
                "Route::middleware([",
                "withMiddleware(",
                "withMiddleware([",
                "->withoutMiddleware(",
                "->withoutMiddleware([",
                "'middleware' => [",   // Array-key syntax in Route::group config
                "\"middleware\" => [", // Double-quote version
            ];

            // Check current line first
            let text_before_match = &before_cursor[..pos];
            let has_middleware_context = middleware_indicators
                .iter()
                .any(|ind| text_before_match.contains(ind));

            if !has_middleware_context {
                // Check previous lines (up to 20 lines back) for middleware array context
                // We need to find an opening pattern without a matching close
                if let Some(prev_lines) = previous_lines {
                    let mut found_in_previous = false;
                    let mut bracket_depth = 0i32;

                    // Scan backwards through previous lines
                    for prev_line in prev_lines.iter().rev().take(20) {
                        // Count brackets to track nesting
                        for ch in prev_line.chars() {
                            match ch {
                                '[' => bracket_depth += 1,
                                ']' => bracket_depth -= 1,
                                _ => {}
                            }
                        }

                        // Check if this line has a middleware indicator
                        if middleware_indicators
                            .iter()
                            .any(|ind| prev_line.contains(ind))
                        {
                            // Found middleware context, and we should still be inside it
                            // (bracket_depth > 0 means we haven't closed the array yet)
                            if bracket_depth > 0 {
                                found_in_previous = true;
                                break;
                            }
                        }
                    }

                    if !found_in_previous {
                        return None;
                    }
                } else {
                    return None;
                }
            }
        }

        // Find where the string ends (closing quote or end of line)
        let end_col = Self::find_string_end(line_text, start_pos, quote_char);

        Some(StringContext {
            prefix: after_pattern.to_string(),
            start_col: start_pos as u32,
            end_col,
            quote_char,
        })
    }

    /// Check if cursor is inside view('...'), View::make('...'), or similar view calls
    /// Returns context with position info for text replacement
    ///
    /// Examples:
    /// - `view('` returns Some(StringContext{prefix: "", ...})
    /// - `view('users.` returns Some(StringContext{prefix: "users.", ...})
    /// - `View::make('admin.` returns Some(StringContext{prefix: "admin.", ...})
    fn get_view_call_context(line_text: &str, character: u32) -> Option<StringContext> {
        let cursor = character as usize;
        if cursor > line_text.len() {
            return None;
        }

        let before_cursor = &line_text[..cursor];

        // Look for various view patterns before cursor
        // Pattern: (pattern_string, quote_char, pattern_length)
        let patterns: Vec<(&str, char, usize)> = vec![
            // view('
            ("view('", '\'', 6),
            ("view(\"", '"', 6),
            // View::make('
            ("View::make('", '\'', 12),
            ("View::make(\"", '"', 12),
            // Route::view(' - second argument is the view name
            // We need to be careful here - first arg is the URI
            // For now, let's match after the comma
            ("Route::view(", '\'', 12), // Will need special handling
            // @extends('
            ("@extends('", '\'', 10),
            ("@extends(\"", '"', 10),
            // @include('
            ("@include('", '\'', 10),
            ("@include(\"", '"', 10),
            // @includeIf('
            ("@includeIf('", '\'', 12),
            ("@includeIf(\"", '"', 12),
            // @includeWhen('
            ("@includeWhen('", '\'', 13),
            ("@includeWhen(\"", '"', 13),
            // @includeUnless('
            ("@includeUnless('", '\'', 16),
            ("@includeUnless(\"", '"', 16),
            // @includeFirst(['
            ("@includeFirst(['", '\'', 16),
            ("@includeFirst([\"", '"', 16),
            // @each('
            ("@each('", '\'', 7),
            ("@each(\"", '"', 7),
            // @component('
            ("@component('", '\'', 12),
            ("@component(\"", '"', 12),
        ];

        // Find all matches and their positions
        let mut matches: Vec<(usize, char, usize)> = Vec::new();

        for (pattern, quote, len) in &patterns {
            if let Some(pos) = before_cursor.rfind(pattern) {
                matches.push((pos, *quote, *len));
            }
        }

        // Special handling for Route::view - need to find the view argument (after first comma)
        if let Some(route_view_pos) = before_cursor.rfind("Route::view(") {
            let after_route_view = &before_cursor[route_view_pos + 12..];
            // Find the first comma (after the URI argument)
            if let Some(comma_pos) = after_route_view.find(',') {
                let after_comma = &after_route_view[comma_pos + 1..];
                // Look for opening quote after the comma
                let trimmed = after_comma.trim_start();
                if let Some(first_char) = trimmed.chars().next() {
                    if first_char == '\'' || first_char == '"' {
                        let quote_char = first_char;
                        let quote_pos_in_after_comma = after_comma.find(quote_char).unwrap();
                        let start =
                            route_view_pos + 12 + comma_pos + 1 + quote_pos_in_after_comma + 1;
                        if start <= cursor {
                            let content = &before_cursor[start..];
                            if !content.contains(quote_char) {
                                matches.push((start - 1, quote_char, 1));
                            }
                        }
                    }
                }
            }
        }

        if matches.is_empty() {
            return None;
        }

        // Find the latest match (closest to cursor)
        let (pos, quote_char, pattern_len) = matches.into_iter().max_by_key(|(p, _, _)| *p)?;

        let start_pos = pos + pattern_len;

        // Check that there's no closing quote between start and cursor
        let after_pattern = &before_cursor[start_pos..];
        if after_pattern.contains(quote_char) {
            return None;
        }

        // Find where the string ends (closing quote or end of line)
        let end_col = Self::find_string_end(line_text, start_pos, quote_char);

        Some(StringContext {
            prefix: after_pattern.to_string(),
            start_col: start_pos as u32,
            end_col,
            quote_char,
        })
    }

    /// Check if cursor is inside a Blade component tag like `<x-...`
    /// Returns context with the partial component name and position info for text replacement
    ///
    /// Examples:
    /// - `<x-` returns Some(StringContext { prefix: "", start_col: 3, end_col: 3, ... })
    /// - `<x-button` returns Some(StringContext { prefix: "button", start_col: 3, end_col: 9, ... })
    /// - `<x-forms.` returns Some(StringContext { prefix: "forms.", start_col: 3, end_col: 9, ... })
    fn get_blade_component_context(line_text: &str, character: u32) -> Option<StringContext> {
        let cursor = character as usize;
        if cursor > line_text.len() {
            return None;
        }

        let before_cursor = &line_text[..cursor];

        // Look for <x- pattern
        // We need to find the last occurrence and ensure we're still in the tag name
        if let Some(pos) = before_cursor.rfind("<x-") {
            let start_pos = pos + 3; // After "<x-"

            // Get the text after <x-
            let after_prefix = &before_cursor[start_pos..];

            // Check that we haven't closed the tag or hit a space (which would mean attributes)
            // Component names can contain: letters, numbers, dots, and hyphens
            // If we hit a space, >, or /, we're past the component name
            if after_prefix.contains(' ')
                || after_prefix.contains('>')
                || after_prefix.contains('/')
            {
                return None;
            }

            // Validate that after_prefix only contains valid component name characters
            if after_prefix
                .chars()
                .all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == '_')
            {
                return Some(StringContext {
                    prefix: after_prefix.to_string(),
                    start_col: start_pos as u32,
                    end_col: cursor as u32,
                    quote_char: ' ', // Not applicable for tag syntax
                });
            }
        }

        None
    }

    /// Check if cursor is inside a Livewire component tag like `<livewire:...` or `@livewire('...')`
    /// Returns the partial component name typed so far (for filtering completions)
    ///
    /// Examples:
    /// - `<livewire:` returns Some("")
    /// - `<livewire:user-` returns Some("user-")
    /// - `@livewire('user-` returns Some("user-")
    fn get_livewire_component_context(line_text: &str, character: u32) -> Option<String> {
        let cursor = character as usize;
        if cursor > line_text.len() {
            return None;
        }

        let before_cursor = &line_text[..cursor];

        // Check for <livewire: pattern (HTML-style)
        if let Some(pos) = before_cursor.rfind("<livewire:") {
            let start_pos = pos + 10; // After "<livewire:"

            // Get the text after <livewire:
            let after_prefix = &before_cursor[start_pos..];

            // Check that we haven't closed the tag or hit a space (which would mean attributes)
            // Component names can contain: letters, numbers, dots, and hyphens
            if after_prefix.contains(' ')
                || after_prefix.contains('>')
                || after_prefix.contains('/')
            {
                return None;
            }

            // Validate that after_prefix only contains valid component name characters
            if after_prefix
                .chars()
                .all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == '_')
            {
                return Some(after_prefix.to_string());
            }
        }

        // Check for @livewire(' pattern (Blade directive style)
        let patterns: Vec<(&str, char, usize)> =
            vec![("@livewire('", '\'', 11), ("@livewire(\"", '"', 11)];

        for (pattern, quote_char, pattern_len) in patterns {
            if let Some(pos) = before_cursor.rfind(pattern) {
                let start_pos = pos + pattern_len;

                // Get the text after the opening quote
                let after_quote = &before_cursor[start_pos..];

                // Check that we haven't hit the closing quote
                if !after_quote.contains(quote_char) {
                    // Validate component name characters
                    if after_quote
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == '_')
                    {
                        return Some(after_quote.to_string());
                    }
                }
            }
        }

        None
    }

    /// Check if cursor is inside asset('...') call
    /// Returns context with position info for text replacement
    ///
    /// Examples:
    /// - `asset('` returns Some(StringContext{prefix: "", ...})
    /// - `asset('css/` returns Some(StringContext{prefix: "css/", ...})
    /// - `asset('images/logo` returns Some(StringContext{prefix: "images/logo", ...})
    fn get_asset_call_context(line_text: &str, character: u32) -> Option<StringContext> {
        let cursor = character as usize;
        if cursor > line_text.len() {
            return None;
        }

        let before_cursor = &line_text[..cursor];

        let patterns: Vec<(&str, char, usize)> = vec![("asset('", '\'', 7), ("asset(\"", '"', 7)];

        for (pattern, quote_char, pattern_len) in patterns {
            if let Some(pos) = before_cursor.rfind(pattern) {
                let start_pos = pos + pattern_len;
                let after_quote = &before_cursor[start_pos..];

                // Check that we haven't hit the closing quote
                if !after_quote.contains(quote_char) {
                    let end_col = Self::find_string_end(line_text, start_pos, quote_char);
                    return Some(StringContext {
                        prefix: after_quote.to_string(),
                        start_col: start_pos as u32,
                        end_col,
                        quote_char,
                    });
                }
            }
        }

        None
    }

    /// Check if cursor is inside @vite('...') or Vite::asset('...') call
    /// Returns context with position info for text replacement
    ///
    /// Examples:
    /// - `@vite('` returns Some(StringContext{prefix: "", ...})
    /// - `@vite('resources/js/` returns Some(StringContext{prefix: "resources/js/", ...})
    fn get_vite_call_context(line_text: &str, character: u32) -> Option<StringContext> {
        let cursor = character as usize;
        if cursor > line_text.len() {
            return None;
        }

        let before_cursor = &line_text[..cursor];

        let patterns: Vec<(&str, char, usize)> = vec![
            ("@vite('", '\'', 7),
            ("@vite(\"", '"', 7),
            ("@vite(['", '\'', 8),
            ("@vite([\"", '"', 8),
            ("Vite::asset('", '\'', 13),
            ("Vite::asset(\"", '"', 13),
        ];

        for (pattern, quote_char, pattern_len) in patterns {
            if let Some(pos) = before_cursor.rfind(pattern) {
                let start_pos = pos + pattern_len;
                let after_quote = &before_cursor[start_pos..];

                // Check that we haven't hit the closing quote
                if !after_quote.contains(quote_char) {
                    let end_col = Self::find_string_end(line_text, start_pos, quote_char);
                    return Some(StringContext {
                        prefix: after_quote.to_string(),
                        start_col: start_pos as u32,
                        end_col,
                        quote_char,
                    });
                }
            }
        }

        None
    }

    /// Check if cursor is inside a path helper call like app_path('...'), base_path('...'), etc.
    /// Returns (helper_name, partial_path) for filtering completions
    ///
    /// Examples:
    /// - `app_path('` returns Some(("app_path", ""))
    /// - `storage_path('logs/` returns Some(("storage_path", "logs/"))
    /// - `base_path('config/` returns Some(("base_path", "config/"))
    fn get_path_helper_context(line_text: &str, character: u32) -> Option<(&'static str, String)> {
        let cursor = character as usize;
        if cursor > line_text.len() {
            return None;
        }

        let before_cursor = &line_text[..cursor];

        // (pattern, quote_char, pattern_len, helper_name)
        let patterns: Vec<(&str, char, usize, &'static str)> = vec![
            ("app_path('", '\'', 10, "app_path"),
            ("app_path(\"", '"', 10, "app_path"),
            ("base_path('", '\'', 11, "base_path"),
            ("base_path(\"", '"', 11, "base_path"),
            ("config_path('", '\'', 13, "config_path"),
            ("config_path(\"", '"', 13, "config_path"),
            ("database_path('", '\'', 15, "database_path"),
            ("database_path(\"", '"', 15, "database_path"),
            ("lang_path('", '\'', 11, "lang_path"),
            ("lang_path(\"", '"', 11, "lang_path"),
            ("public_path('", '\'', 13, "public_path"),
            ("public_path(\"", '"', 13, "public_path"),
            ("resource_path('", '\'', 15, "resource_path"),
            ("resource_path(\"", '"', 15, "resource_path"),
            ("storage_path('", '\'', 14, "storage_path"),
            ("storage_path(\"", '"', 14, "storage_path"),
        ];

        for (pattern, quote_char, pattern_len, helper_name) in patterns {
            if let Some(pos) = before_cursor.rfind(pattern) {
                let start_pos = pos + pattern_len;
                let after_quote = &before_cursor[start_pos..];

                // Check that we haven't hit the closing quote
                if !after_quote.contains(quote_char) {
                    return Some((helper_name, after_quote.to_string()));
                }
            }
        }

        None
    }

    /// Check if cursor is inside app('...') or resolve('...') container binding calls
    /// Returns context with position info for text replacement
    ///
    /// Examples:
    /// - `app('` returns Some(StringContext{prefix: "", ...})
    /// - `app('cache` returns Some(StringContext{prefix: "cache", ...})
    /// - `resolve('log` returns Some(StringContext{prefix: "log", ...})
    fn get_binding_call_context(line_text: &str, character: u32) -> Option<StringContext> {
        let cursor = character as usize;
        if cursor > line_text.len() {
            return None;
        }

        let before_cursor = &line_text[..cursor];

        let patterns: Vec<(&str, char, usize)> = vec![
            ("app('", '\'', 5),
            ("app(\"", '"', 5),
            ("resolve('", '\'', 9),
            ("resolve(\"", '"', 9),
            ("App::make('", '\'', 11),
            ("App::make(\"", '"', 11),
        ];

        for (pattern, quote_char, pattern_len) in patterns {
            if let Some(pos) = before_cursor.rfind(pattern) {
                let start_pos = pos + pattern_len;
                let after_quote = &before_cursor[start_pos..];

                // Check that we haven't hit the closing quote
                if !after_quote.contains(quote_char) {
                    let end_col = Self::find_string_end(line_text, start_pos, quote_char);
                    return Some(StringContext {
                        prefix: after_quote.to_string(),
                        start_col: start_pos as u32,
                        end_col,
                        quote_char,
                    });
                }
            }
        }

        None
    }

    /// Check if cursor is inside Feature::active('...'), Feature::for($user)->active('...'), etc.
    /// Returns context with position info for text replacement
    ///
    /// Examples:
    /// - `Feature::active('` returns Some(StringContext{prefix: "", ...})
    /// - `Feature::active('new` returns Some(StringContext{prefix: "new", ...})
    /// - `Feature::for($user)->active('beta` returns Some(StringContext{prefix: "beta", ...})
    fn get_feature_call_context(line_text: &str, character: u32) -> Option<StringContext> {
        let cursor = character as usize;
        if cursor > line_text.len() {
            return None;
        }

        let before_cursor = &line_text[..cursor];

        // Pattern: (pattern_string, quote_char, pattern_length)
        let patterns: Vec<(&str, char, usize)> = vec![
            // Blade @feature directive (with optional space before paren)
            ("@feature ('", '\'', 11),
            ("@feature (\"", '"', 11),
            ("@feature('", '\'', 10),
            ("@feature(\"", '"', 10),
            // Direct Feature:: calls
            ("Feature::active('", '\'', 17),
            ("Feature::active(\"", '"', 17),
            ("Feature::inactive('", '\'', 19),
            ("Feature::inactive(\"", '"', 19),
            ("Feature::value('", '\'', 16),
            ("Feature::value(\"", '"', 16),
            ("Feature::when('", '\'', 15),
            ("Feature::when(\"", '"', 15),
            ("Feature::forget('", '\'', 17),
            ("Feature::forget(\"", '"', 17),
            ("Feature::purge('", '\'', 16),
            ("Feature::purge(\"", '"', 16),
            // Feature::allAreActive/someAreActive with array
            ("Feature::allAreActive(['", '\'', 24),
            ("Feature::allAreActive([\"", '"', 24),
            ("Feature::someAreActive(['", '\'', 25),
            ("Feature::someAreActive([\"", '"', 25),
            ("Feature::allAreInactive(['", '\'', 26),
            ("Feature::allAreInactive([\"", '"', 26),
            ("Feature::someAreInactive(['", '\'', 27),
            ("Feature::someAreInactive([\"", '"', 27),
            // Chained calls after Feature::for(...)
            (")->active('", '\'', 11),
            (")->active(\"", '"', 11),
            (")->inactive('", '\'', 13),
            (")->inactive(\"", '"', 13),
            (")->value('", '\'', 10),
            (")->value(\"", '"', 10),
            (")->when('", '\'', 9),
            (")->when(\"", '"', 9),
            // Array element patterns (for continuing a list)
            ("', '", '\'', 4),
            ("\", \"", '"', 4),
        ];

        // Find all matches and their positions
        let mut matches: Vec<(usize, char, usize)> = Vec::new();

        for (pattern, quote, len) in &patterns {
            if let Some(pos) = before_cursor.rfind(pattern) {
                matches.push((pos, *quote, *len));
            }
        }

        if matches.is_empty() {
            return None;
        }

        // Find the latest match (closest to cursor)
        let (pos, quote_char, pattern_len) = matches.into_iter().max_by_key(|(p, _, _)| *p)?;

        let start_pos = pos + pattern_len;

        // Check that there's no closing quote between start and cursor
        let after_pattern = &before_cursor[start_pos..];
        if after_pattern.contains(quote_char) {
            return None;
        }

        // For array patterns like "', '", verify we're actually in a Feature context
        // by checking if Feature:: appears earlier in the line
        if pattern_len <= 4 {
            let feature_indicators = [
                "Feature::allAreActive(",
                "Feature::someAreActive(",
                "Feature::allAreInactive(",
                "Feature::someAreInactive(",
            ];
            let has_feature_context = feature_indicators
                .iter()
                .any(|ind| before_cursor[..pos].contains(ind));
            if !has_feature_context {
                return None;
            }
        }

        // For chained patterns like ")->active(", verify Feature::for( appears earlier
        if pattern_len <= 13
            && before_cursor[..pos].contains(")->")
            && !before_cursor[..pos].contains("Feature::for(")
        {
            return None;
        }

        // Find where the string ends (closing quote or end of line)
        let end_col = Self::find_string_end(line_text, start_pos, quote_char);

        Some(StringContext {
            prefix: after_pattern.to_string(),
            start_col: start_pos as u32,
            end_col,
            quote_char,
        })
    }

    /// Check if cursor is after `->` on a model variable or static chain
    /// Returns (model_class_hint, typed_prefix) for property completions
    ///
    /// This detects two patterns:
    /// 1. Variable access: `$user->` where we need to resolve $user's type
    /// 2. Static chain: `User::find(1)->` or `User::where(...)->first()->` where we extract the class
    ///
    /// Examples:
    /// - `$user->` returns Some(("$user", ""))
    /// - `$user->na` returns Some(("$user", "na"))
    /// - `User::find(1)->` returns Some(("User", ""))
    /// - `User::find(1)->ema` returns Some(("User", "ema"))
    fn get_model_property_context(line_text: &str, character: u32) -> Option<(String, String)> {
        let cursor = character as usize;
        if cursor > line_text.len() {
            return None;
        }

        let before_cursor = &line_text[..cursor];

        // Find the last `->` before cursor
        let arrow_pos = before_cursor.rfind("->")?;

        // Extract what's typed after `->`
        let typed_prefix = before_cursor[arrow_pos + 2..].to_string();

        // Don't match if prefix contains invalid characters for a property name
        if typed_prefix.contains(|c: char| !c.is_alphanumeric() && c != '_') {
            return None;
        }

        // Get the part before `->`
        let before_arrow = &before_cursor[..arrow_pos];

        // Try to extract the class/variable that the arrow is on
        let class_hint = Self::extract_model_class_hint(before_arrow)?;

        Some((class_hint, typed_prefix))
    }

    /// Extract the model class hint from the expression before `->`
    ///
    /// Handles:
    /// - `$variable` -> returns "$variable" (caller will resolve type)
    /// - `User::find(1)` -> returns "User"
    /// - `User::where(...)->first()` -> returns "User"
    /// - `$this` -> returns "$this" (for use inside a model)
    fn extract_model_class_hint(before_arrow: &str) -> Option<String> {
        let trimmed = before_arrow.trim_end();

        // Case 1: Ends with a variable like $user or $this
        if let Some(var_match) = regex::Regex::new(r"\$([a-zA-Z_][a-zA-Z0-9_]*)$")
            .ok()
            .and_then(|re| re.find(trimmed))
        {
            return Some(var_match.as_str().to_string());
        }

        // Case 2: Ends with a method call on a static class like User::find(1) or User::where(...)->first()
        // Look for the class name at the start of a static chain
        // Pattern: ClassName::method(...) or ClassName::method(...)->other(...)
        if let Some(caps) = regex::Regex::new(r"([A-Z][a-zA-Z0-9_]*)::(?:find|findOrFail|first|firstOrFail|where|query|all|get|create|make|findOr|sole|firstOr|firstWhere|findMany)\s*\(")
            .ok()
            .and_then(|re| re.captures(trimmed))
        {
            if let Some(class) = caps.get(1) {
                return Some(class.as_str().to_string());
            }
        }

        // Case 3: Ends with a method call like ->first(), ->find(), etc. - try to find class earlier in chain
        if trimmed.ends_with(')') {
            // Look backwards for a class name in a static call pattern
            if let Some(caps) = regex::Regex::new(r"([A-Z][a-zA-Z0-9_]*)::(?:find|findOrFail|first|firstOrFail|where|query|all|get|create|make)\s*\(")
                .ok()
                .and_then(|re| re.captures(trimmed))
            {
                if let Some(class) = caps.get(1) {
                    return Some(class.as_str().to_string());
                }
            }
        }

        None
    }

    /// Resolve a variable name to its model class type by analyzing the file content
    ///
    /// Searches for:
    /// 1. Type hints in function parameters: `function show(User $user)`
    /// 2. Type hints in variable declarations: `User $user = ...`
    /// 3. PHPDoc annotations: `@var User $user` or `@param User $user`
    ///
    /// Returns the class name if found (e.g., "User")
    fn resolve_variable_type(content: &str, variable_name: &str) -> Option<String> {
        // Remove the $ from variable name for matching
        let var_without_dollar = variable_name.trim_start_matches('$');

        // Pattern 1: Type hint in function parameter
        // e.g., "function show(User $user)" or "(Request $request, User $user)"
        let param_pattern = format!(
            r"([A-Z][a-zA-Z0-9_\\]*)\s+\${}(?:\s*[,=)]|\s*$)",
            regex::escape(var_without_dollar)
        );
        if let Some(caps) = regex::Regex::new(&param_pattern)
            .ok()
            .and_then(|re| re.captures(content))
        {
            if let Some(class) = caps.get(1) {
                let class_name = class.as_str();
                // Get just the class name (last part after \)
                let simple_name = class_name.rsplit('\\').next().unwrap_or(class_name);
                return Some(simple_name.to_string());
            }
        }

        // Pattern 2: PHPDoc @var annotation
        // e.g., "/** @var User $user */" or "@var User $user"
        let var_pattern = format!(
            r"@var\s+([A-Z][a-zA-Z0-9_\\]*)\s+\${}(?:\s|$)",
            regex::escape(var_without_dollar)
        );
        if let Some(caps) = regex::Regex::new(&var_pattern)
            .ok()
            .and_then(|re| re.captures(content))
        {
            if let Some(class) = caps.get(1) {
                let class_name = class.as_str();
                let simple_name = class_name.rsplit('\\').next().unwrap_or(class_name);
                return Some(simple_name.to_string());
            }
        }

        // Pattern 3: PHPDoc @param annotation
        // e.g., "@param User $user"
        let param_doc_pattern = format!(
            r"@param\s+([A-Z][a-zA-Z0-9_\\]*)\s+\${}(?:\s|$)",
            regex::escape(var_without_dollar)
        );
        if let Some(caps) = regex::Regex::new(&param_doc_pattern)
            .ok()
            .and_then(|re| re.captures(content))
        {
            if let Some(class) = caps.get(1) {
                let class_name = class.as_str();
                let simple_name = class_name.rsplit('\\').next().unwrap_or(class_name);
                return Some(simple_name.to_string());
            }
        }

        // Pattern 4: Variable assignment with new
        // e.g., "$user = new User("
        let new_pattern = format!(
            r"\${}\s*=\s*new\s+([A-Z][a-zA-Z0-9_\\]*)\s*\(",
            regex::escape(var_without_dollar)
        );
        if let Some(caps) = regex::Regex::new(&new_pattern)
            .ok()
            .and_then(|re| re.captures(content))
        {
            if let Some(class) = caps.get(1) {
                let class_name = class.as_str();
                let simple_name = class_name.rsplit('\\').next().unwrap_or(class_name);
                return Some(simple_name.to_string());
            }
        }

        // Pattern 5: Variable assignment with static method that returns model
        // e.g., "$user = User::find(1)" or "$user = User::create([...])"
        let static_pattern = format!(
            r"\${}\s*=\s*([A-Z][a-zA-Z0-9_\\]*)::(?:find|findOrFail|create|first|firstOrFail|sole|make)\s*\(",
            regex::escape(var_without_dollar)
        );
        if let Some(caps) = regex::Regex::new(&static_pattern)
            .ok()
            .and_then(|re| re.captures(content))
        {
            if let Some(class) = caps.get(1) {
                let class_name = class.as_str();
                let simple_name = class_name.rsplit('\\').next().unwrap_or(class_name);
                return Some(simple_name.to_string());
            }
        }

        None
    }

    /// Resolve a Blade view variable's type by checking the source that provides it
    ///
    /// For a view like `resources/views/users/show.blade.php` with variable `$user`,
    /// checks these sources in order (higher priority sources override lower):
    /// 1. @props directive in the Blade file (lowest priority - fallback)
    /// 2. Controller methods that render this view
    /// 3. View component class (if this is a component view)
    /// 4. Livewire component (if this is a Livewire view) (highest priority)
    async fn resolve_blade_variable_type(&self, uri: &Url, variable_name: &str) -> Option<String> {
        // Decode the URI to a filesystem path. `uri.path()` returns the raw,
        // percent-encoded URI path — `to_file_path()` decodes it, which is
        // required for any directory or file name containing non-ASCII
        // characters (e.g. the ⚡ marker on Livewire 4 component directories).
        let file_path = uri.to_file_path().ok()?;
        let path = file_path.to_str()?;

        // Only process Blade files
        if !path.ends_with(".blade.php") {
            return None;
        }

        let root = {
            let root_guard = self.root_path.read().await;
            root_guard.clone()?
        };

        // Extract view name from path
        // e.g., /path/to/project/resources/views/users/show.blade.php -> users.show
        let view_name = self.extract_view_name_from_path(path, &root);
        let var_without_dollar = variable_name.trim_start_matches('$');

        // Read the Blade file content for @props check
        let blade_content = std::fs::read_to_string(path).ok();

        // Start with @props as the base (lowest priority)
        let mut resolved_type = blade_content
            .as_ref()
            .and_then(|content| Self::extract_props_type(content, var_without_dollar));

        // Check controllers (overrides @props)
        if let Some(ref vn) = view_name {
            if let Some(class) = self.check_controller_view_variable(&root, vn, var_without_dollar)
            {
                resolved_type = Some(class);
            }
        }

        // Check View component class (overrides controller)
        if let Some(class) = self.check_view_component_variable(&root, path, var_without_dollar) {
            resolved_type = Some(class);
        }

        // Check Livewire component (highest priority - overrides all).
        // Pass the blade file path so Volt SFC / MFC files resolve in addition to
        // the classic `resources/views/livewire/` -> `app/Livewire/` mapping.
        if let Some(class) = self.check_livewire_component_variable(&root, path, var_without_dollar)
        {
            resolved_type = Some(class);
        }

        // Fetch loop blocks via Salsa (memoized). Use std::path::PathBuf to send to the actor.
        let path_buf = std::path::PathBuf::from(path);
        let loops = self.salsa.get_loop_blocks(path_buf).await.ok().flatten();

        // Check Blade loop variables (@foreach / @forelse / @for).
        // This runs LAST as a fallback so that earlier sources (props, controller, etc.)
        // win when a foreach variable shadows an outer-scope variable name.
        if resolved_type.is_none() {
            if let Some(ref blocks) = loops {
                for block in blocks.iter() {
                    let is_value_var = block.variables.iter().any(|(n, _)| n == var_without_dollar);
                    if !is_value_var {
                        continue;
                    }

                    // Try to derive the element type from the iterable expression.
                    if let Some(iter_expr) = &block.iterable {
                        if let Some((elem_type, has_element)) = self
                            .resolve_iterable_type_info(&root, path, iter_expr)
                            .await
                        {
                            if has_element {
                                // Genuine element type — enables hover/autocomplete on $audit->...
                                resolved_type = Some(elem_type);
                                break;
                            } else {
                                // Iterable resolved but element type unknown (no PHPDoc generics).
                                // Return a non-None sentinel so the "Cannot resolve type" diagnostic
                                // is suppressed; user can add `@return Foo<Element>` to unlock typing.
                                resolved_type = Some("mixed".to_string());
                                break;
                            }
                        }
                    }

                    // Variable is defined by a loop but iterable couldn't be resolved.
                    // Suppress the diagnostic — the variable IS defined, just untyped.
                    resolved_type = Some("mixed".to_string());
                    break;
                }
            }
        }

        // $loop is a Blade synthetic — return "Loop" when the file has any loop blocks.
        // get_model_properties short-circuits on this type to return the hardcoded loop members.
        if resolved_type.is_none() && var_without_dollar == "loop" {
            if let Some(ref blocks) = loops {
                if !blocks.is_empty() {
                    resolved_type = Some("Loop".to_string());
                }
            }
        }

        // Check @php block assignments (`$outerLoop = $loop;` style).
        // Variables declared in @php remain in scope for the rest of the Blade file.
        if resolved_type.is_none() {
            let path_buf = std::path::PathBuf::from(path);
            if let Ok(Some(assignments)) = self.salsa.get_php_assignments(path_buf).await {
                for (name, ty) in assignments.iter() {
                    if name == var_without_dollar {
                        resolved_type = Some(ty.clone());
                        break;
                    }
                }
            }
        }

        resolved_type
    }

    /// Extract type from @props directive in a Blade file
    /// Supports formats:
    /// - @props(['user' => User::class])
    /// - @props(['user' => \App\Models\User::class])
    /// - @props(['user' => 'App\Models\User'])
    fn extract_props_type(content: &str, prop_name: &str) -> Option<String> {
        // Find the @props directive
        let props_re = regex::Regex::new(r"@props\s*\(\s*\[([^\]]*)\]\s*\)").ok()?;
        let caps = props_re.captures(content)?;
        let props_content = caps.get(1)?.as_str();

        // Look for 'propName' => Type::class or 'propName' => 'TypeName'
        // Pattern 1: 'propName' => ClassName::class
        let class_pattern = format!(
            r#"['"]{}['"]\s*=>\s*\\?([A-Za-z][A-Za-z0-9_\\]*)::class"#,
            regex::escape(prop_name)
        );
        if let Some(caps) = regex::Regex::new(&class_pattern)
            .ok()?
            .captures(props_content)
        {
            if let Some(m) = caps.get(1) {
                let full_type = m.as_str();
                // Get just the class name (last part after \)
                return Some(
                    full_type
                        .rsplit('\\')
                        .next()
                        .unwrap_or(full_type)
                        .to_string(),
                );
            }
        }

        // Pattern 2: 'propName' => 'ClassName' (string type hint)
        let string_pattern = format!(
            r#"['"]{}['"]\s*=>\s*['"]\\?([A-Za-z][A-Za-z0-9_\\]*)['"]"#,
            regex::escape(prop_name)
        );
        if let Some(caps) = regex::Regex::new(&string_pattern)
            .ok()?
            .captures(props_content)
        {
            if let Some(m) = caps.get(1) {
                let full_type = m.as_str();
                return Some(
                    full_type
                        .rsplit('\\')
                        .next()
                        .unwrap_or(full_type)
                        .to_string(),
                );
            }
        }

        None
    }

    /// Goto-definition fallback for Blade variables and property accesses.
    /// Called from `goto_definition` when the standard pattern matcher finds
    /// nothing at the cursor.
    ///
    /// - Cursor on `$form` → jumps to `public ContactForm $form;` inside the
    ///   Livewire component backing this view.
    /// - Cursor on `$form->name` → jumps to `public string $name;` inside
    ///   `ContactForm.php` (the resolved class file).
    ///
    /// Returns `None` if the cursor isn't on a variable reference, the variable
    /// doesn't resolve to a known component, or the declaration can't be located.
    async fn blade_variable_goto_definition(
        &self,
        uri: &Url,
        position: Position,
    ) -> Option<GotoDefinitionResponse> {
        let file_path = uri.to_file_path().ok()?;
        let path = file_path.to_str()?;

        let content = match self.documents.read().await.get(uri).cloned() {
            Some((c, _)) => c,
            None => std::fs::read_to_string(path).ok()?,
        };
        let line_text = content.lines().nth(position.line as usize)?.to_string();

        let (var_name, property) =
            extract_blade_variable_at_cursor(&line_text, position.character)?;

        let root = self.root_path.read().await.clone()?;

        let (target_path, declaration_position) = match property {
            // $var → component file + property declaration line for $var
            None => {
                let component_path = self.find_livewire_component_php(&root, path)?;
                let component_content = std::fs::read_to_string(&component_path).ok()?;
                let pos = laravel_lsp::php_class::find_property_declaration_position(
                    &component_content,
                    &var_name,
                )?;
                (component_path, pos)
            }
            // $var->prop → class file (whatever type $var resolves to) + prop line
            Some(prop_name) => {
                let var_type = self
                    .resolve_blade_variable_type(uri, &format!("${}", var_name))
                    .await?;
                let class_path = laravel_lsp::class_locator::find_php_class_file(&var_type, &root)?;
                let class_content = std::fs::read_to_string(&class_path).ok()?;
                let pos = laravel_lsp::php_class::find_property_declaration_position(
                    &class_content,
                    &prop_name,
                )?;
                (class_path, pos)
            }
        };

        let (line, start_col, end_col) = declaration_position;
        let target_uri = Url::from_file_path(&target_path).ok()?;

        Some(GotoDefinitionResponse::Scalar(Location {
            uri: target_uri,
            range: Range {
                start: Position {
                    line,
                    character: start_col,
                },
                end: Position {
                    line,
                    character: end_col,
                },
            },
        }))
    }

    /// Resolve the type of a property `prop` on a given class. Uses the class
    /// locator to find the class file, then scans for `public Type $prop`.
    /// Returns `None` when the class can't be found or the property isn't
    /// declared with a recognizable type.
    ///
    /// Kept available even though the hover handler no longer calls it
    /// directly — completion / diagnostic code may grow to use it, and the
    /// resolution logic is non-trivial enough that I don't want to delete it.
    #[allow(dead_code)]
    async fn resolve_property_type_on_class(
        &self,
        class_name: &str,
        property_name: &str,
    ) -> Option<String> {
        let root = self.root_path.read().await.clone()?;
        let class_path = laravel_lsp::class_locator::find_php_class_file(class_name, &root)?;
        let content = std::fs::read_to_string(&class_path).ok()?;
        laravel_lsp::php_class::find_property_type_in_content(&content, property_name)
    }

    /// Detect if user is typing a variable name (e.g., `$u`, `$user`)
    /// Returns the prefix they've typed (without $) if in variable name context
    /// Returns None if they're in `$var->` context (property access)
    fn get_variable_name_context(line_text: &str, cursor_col: u32) -> Option<String> {
        let cursor = cursor_col as usize;
        if cursor == 0 || cursor > line_text.len() {
            return None;
        }

        let before_cursor = &line_text[..cursor];

        // Check if we're after a $ and NOT in property access context
        // Look backwards to find $
        let mut dollar_pos = None;
        for (i, c) in before_cursor.char_indices().rev() {
            if c == '$' {
                dollar_pos = Some(i);
                break;
            }
            // If we hit non-identifier chars before finding $, we're not in variable context
            if !c.is_alphanumeric() && c != '_' {
                break;
            }
        }

        let dollar_pos = dollar_pos?;

        // Get the text after $
        let after_dollar = &before_cursor[dollar_pos + 1..];

        // Check it's a valid variable name prefix (only alphanumeric and _)
        if !after_dollar
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_')
        {
            return None;
        }

        // Make sure we're not in property access context ($var->)
        // Check if there's a -> after the variable name before cursor
        let _rest_of_line = &line_text[cursor..];

        // If the variable name is immediately followed by ->, skip
        // But we need to look at what's AFTER cursor to see if we're in $var| context vs $var->|
        // Actually, we just need to check if BEFORE cursor there's ->
        if before_cursor.contains("->") {
            // Find if the -> is after our $
            let after_dollar_to_cursor = &before_cursor[dollar_pos..];
            if after_dollar_to_cursor.contains("->") {
                return None; // We're in property access context
            }
        }

        Some(after_dollar.to_string())
    }

    /// Detect if user is typing a Blade directive (e.g., `@if`, `@foreach`)
    /// Returns the partial directive name typed so far (e.g., "fo" for "@fo|")
    fn get_blade_directive_context(line_text: &str, cursor_col: u32) -> Option<String> {
        let cursor = cursor_col as usize;
        if cursor == 0 || cursor > line_text.len() {
            return None;
        }

        let before_cursor = &line_text[..cursor];

        // Look backwards for @ that starts a directive
        let mut at_pos = None;
        for (i, c) in before_cursor.char_indices().rev() {
            if c == '@' {
                at_pos = Some(i);
                break;
            }
            // If we hit non-alphanumeric chars before finding @, we're not in directive context
            if !c.is_alphanumeric() {
                break;
            }
        }

        let at_pos = at_pos?;

        // Get the text after @
        let after_at = &before_cursor[at_pos + 1..];

        // Check it's a valid directive name prefix (only alphanumeric chars)
        if !after_at.chars().all(|c| c.is_alphanumeric()) {
            return None;
        }

        // Make sure we're not inside a string or already completed directive
        // Check if @ is at start of line or preceded by whitespace/bracket
        if at_pos > 0 {
            let char_before_at = before_cursor.chars().nth(at_pos - 1);
            if let Some(c) = char_before_at {
                // @ should be preceded by whitespace, start of tag, or at start of PHP block
                if !c.is_whitespace() && c != '>' && c != '(' && c != '{' && c != ';' && c != '\t' {
                    return None;
                }
            }
        }

        Some(after_at.to_string())
    }

    // Blade loop parsing functions live in `laravel_lsp::blade_loops` so the
    // Salsa actor can return them from a tracked query. Use them via the
    // module path (e.g. `laravel_lsp::blade_loops::find_loop_blocks(...)`).

    /// Get all available variables for a Blade file
    /// Collects variables from @props, controller, Livewire, view component, and loop directives
    fn get_blade_available_variables(
        &self,
        uri: &Url,
        content: Option<&str>,
        cursor_line: Option<u32>,
    ) -> Vec<BladeVariableInfo> {
        // Decode the URI to a filesystem path. See `resolve_blade_variable_type`
        // for the rationale — non-ASCII path segments (e.g. ⚡-prefixed Livewire
        // 4 component directories) require URI decoding before filesystem use.
        let Ok(file_path) = uri.to_file_path() else {
            return Vec::new();
        };
        let Some(path) = file_path.to_str() else {
            return Vec::new();
        };

        if !path.ends_with(".blade.php") {
            return Vec::new();
        }

        let root = match self.root_path.try_read().ok().and_then(|g| g.clone()) {
            Some(r) => r,
            None => return Vec::new(),
        };

        let mut variables = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        let view_name = self.extract_view_name_from_path(path, &root);

        // Use provided content or read from disk
        let owned_content: Option<String>;
        let blade_content: Option<&str> = match content {
            Some(c) => Some(c),
            None => {
                owned_content = std::fs::read_to_string(path).ok();
                owned_content.as_deref()
            }
        };

        // 1. Extract from @props directive
        if let Some(content_str) = blade_content {
            let props_vars = Self::extract_all_props_variables(content_str);
            for (name, php_type) in props_vars {
                if seen.insert(name.clone()) {
                    variables.push(BladeVariableInfo {
                        name,
                        php_type,
                        source: "props".to_string(),
                    });
                }
            }
        }

        // 2. Extract from controller (if this is a regular view)
        if let Some(ref vn) = view_name {
            let controller_vars = self.extract_controller_variables(&root, vn);
            for (name, php_type) in controller_vars {
                if seen.insert(name.clone()) {
                    variables.push(BladeVariableInfo {
                        name,
                        php_type,
                        source: "controller".to_string(),
                    });
                }
            }
        }

        // 3. Extract from View component class
        let component_vars = self.extract_component_variables(&root, path);
        for (name, php_type) in component_vars {
            if seen.insert(name.clone()) {
                variables.push(BladeVariableInfo {
                    name,
                    php_type,
                    source: "component".to_string(),
                });
            }
        }

        // 4. Extract from Livewire / Volt component (uses blade path so SFC + MFC resolve too).
        let livewire_vars = self.extract_livewire_variables(&root, path);
        for (name, php_type) in livewire_vars {
            if seen.insert(name.clone()) {
                variables.push(BladeVariableInfo {
                    name,
                    php_type,
                    source: "livewire".to_string(),
                });
            }
        }

        // 5. Extract loop variables from enclosing loop directives (scope-aware)
        if let (Some(content_str), Some(line)) = (blade_content, cursor_line) {
            let enclosing_loops =
                laravel_lsp::blade_loops::get_enclosing_loops(content_str, line as usize);

            // Add variables from each enclosing loop
            for loop_block in &enclosing_loops {
                for (name, php_type) in &loop_block.variables {
                    if seen.insert(name.clone()) {
                        let source = match loop_block.loop_type {
                            BladeLoopType::Foreach => "foreach",
                            BladeLoopType::Forelse => "forelse",
                            BladeLoopType::For => "for",
                            BladeLoopType::While => "while",
                        };
                        variables.push(BladeVariableInfo {
                            name: name.clone(),
                            php_type: php_type.clone(),
                            source: source.to_string(),
                        });
                    }
                }
            }

            // Add $loop variable if inside any loop (foreach, forelse, for, while)
            if !enclosing_loops.is_empty() && seen.insert("loop".to_string()) {
                variables.push(BladeVariableInfo {
                    name: "loop".to_string(),
                    php_type: "Loop".to_string(),
                    source: "loop".to_string(),
                });
            }
        }

        // 6. Extract @php block variable assignments (file-scoped — visible after @endphp).
        if let Some(content_str) = blade_content {
            let php_vars = laravel_lsp::blade_php_block::extract_php_block_assignments(content_str);
            for (name, php_type) in php_vars {
                if seen.insert(name.clone()) {
                    variables.push(BladeVariableInfo {
                        name,
                        php_type,
                        source: "php-block".to_string(),
                    });
                }
            }
        }

        // Add common Blade framework variables
        // Note: $slot, $attributes, $component are only meaningful in component files
        let is_component = Self::is_component_file(path);
        let framework_vars: Vec<(&str, &str, &str)> = if is_component {
            vec![
                ("errors", "MessageBag", "framework"),
                ("slot", "Slot", "framework"),
                ("attributes", "ComponentAttributeBag", "framework"),
                ("component", "Component", "framework"),
            ]
        } else {
            vec![("errors", "MessageBag", "framework")]
        };

        for (name, php_type, source) in framework_vars {
            if seen.insert(name.to_string()) {
                variables.push(BladeVariableInfo {
                    name: name.to_string(),
                    php_type: php_type.to_string(),
                    source: source.to_string(),
                });
            }
        }

        // 6. For component files, extract named slot variable usages
        // These are variables like $header, $footer that are used in the component
        // and should be provided via <x-slot:name> when the component is used
        if is_component {
            if let Some(content_str) = blade_content {
                let slot_vars = Self::extract_slot_variable_usages(content_str);
                for (name, php_type) in slot_vars {
                    // Only add if not already defined from other sources (props, controller, etc.)
                    if seen.insert(name.clone()) {
                        variables.push(BladeVariableInfo {
                            name,
                            php_type,
                            source: "slot".to_string(),
                        });
                    }
                }
            }
        }

        variables
    }

    /// Extract all props variable names and types from @props directive
    fn extract_all_props_variables(content: &str) -> Vec<(String, String)> {
        let mut vars = Vec::new();

        // Find the @props directive
        let props_re = match regex::Regex::new(r"@props\s*\(\s*\[([^\]]*)\]\s*\)") {
            Ok(re) => re,
            Err(_) => return vars,
        };

        let Some(caps) = props_re.captures(content) else {
            return vars;
        };
        let Some(props_content) = caps.get(1) else {
            return vars;
        };
        let props_str = props_content.as_str();

        // Match 'propName' => Type::class or 'propName' => 'Type' or just 'propName'
        // Pattern for typed props: 'name' => Type::class or 'name' => 'Type'
        let typed_re = regex::Regex::new(
            r#"['"]([a-zA-Z_][a-zA-Z0-9_]*)['"]\s*=>\s*(?:(?:\\?([A-Za-z][A-Za-z0-9_\\]*)::class)|['"]\\?([A-Za-z][A-Za-z0-9_\\]*)['"])"#
        ).ok();

        if let Some(re) = typed_re {
            for cap in re.captures_iter(props_str) {
                if let Some(name) = cap.get(1) {
                    let php_type = cap
                        .get(2)
                        .or(cap.get(3))
                        .map(|m| {
                            let t = m.as_str();
                            t.rsplit('\\').next().unwrap_or(t).to_string()
                        })
                        .unwrap_or_else(|| "mixed".to_string());
                    vars.push((name.as_str().to_string(), php_type));
                }
            }
        }

        // Pattern for untyped props: 'name' (not followed by =>)
        let untyped_re =
            regex::Regex::new(r#"['"]([a-zA-Z_][a-zA-Z0-9_]*)['"](?:\s*(?:,|\]|$))"#).ok();

        if let Some(re) = untyped_re {
            for cap in re.captures_iter(props_str) {
                if let Some(name) = cap.get(1) {
                    let name_str = name.as_str().to_string();
                    // Only add if not already in typed list
                    if !vars.iter().any(|(n, _)| n == &name_str) {
                        vars.push((name_str, "mixed".to_string()));
                    }
                }
            }
        }

        vars
    }

    /// Extract slot variable usages from a component blade file
    /// Looks for patterns like {{ $header }}, {{ $footer }}, $title->isEmpty(), etc.
    /// These are variables that should be provided via <x-slot:name>
    fn extract_slot_variable_usages(content: &str) -> Vec<(String, String)> {
        use lazy_static::lazy_static;
        use regex::Regex;

        lazy_static! {
            // Match variable usages in echo statements: {{ $varname }}
            // Also matches {!! $varname !!} and {{ $var->method() }}
            static ref ECHO_VAR_RE: Regex = Regex::new(
                r#"\{\{[^}]*\$([a-zA-Z_][a-zA-Z0-9_]*)"#
            ).unwrap();

            // Match variable method calls like $slot->isEmpty(), $header->attributes
            static ref VAR_METHOD_RE: Regex = Regex::new(
                r#"\$([a-zA-Z_][a-zA-Z0-9_]*)\s*->"#
            ).unwrap();
        }

        let mut vars = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        // Known non-slot variables to exclude
        let exclude_vars: std::collections::HashSet<&str> = [
            // Framework variables
            "errors",
            "slot",
            "attributes",
            "component",
            // Loop variables
            "loop",
            // Common non-slot variables
            "this",
            "self",
        ]
        .into_iter()
        .collect();

        // Find all variable usages in echo statements
        for cap in ECHO_VAR_RE.captures_iter(content) {
            if let Some(var_match) = cap.get(1) {
                let var_name = var_match.as_str();
                if !exclude_vars.contains(var_name) && seen.insert(var_name.to_string()) {
                    vars.push((var_name.to_string(), "Slot".to_string()));
                }
            }
        }

        // Find variable method calls (like $header->attributes)
        for cap in VAR_METHOD_RE.captures_iter(content) {
            if let Some(var_match) = cap.get(1) {
                let var_name = var_match.as_str();
                if !exclude_vars.contains(var_name) && seen.insert(var_name.to_string()) {
                    vars.push((var_name.to_string(), "Slot".to_string()));
                }
            }
        }

        vars
    }

    /// Check if a blade file is a component file (in resources/views/components/)
    fn is_component_file(path: &str) -> bool {
        path.contains("/components/") && path.ends_with(".blade.php")
    }

    /// Extract variables passed to a view from any PHP file that renders it
    /// Supports:
    /// - view('name', compact('a', 'b'))
    /// - view('name', ['key' => $value])
    /// - view('name')->with('key', $value)
    /// - view('name')->with(['key' => $value])
    /// - View::make('name')->with(...)
    fn extract_controller_variables(
        &self,
        root: &std::path::Path,
        view_name: &str,
    ) -> Vec<(String, String)> {
        let mut vars = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        // Search in common directories where views are called from
        let search_dirs = [
            root.join("app").join("Http").join("Controllers"),
            root.join("app").join("Livewire"),
            root.join("app").join("View").join("Components"),
            root.join("routes"),
        ];

        for dir in &search_dirs {
            if !dir.exists() {
                continue;
            }

            self.search_php_files_for_view_vars(dir, view_name, &mut vars, &mut seen);
        }

        vars
    }

    /// Recursively search PHP files in a directory for view variable assignments
    fn search_php_files_for_view_vars(
        &self,
        dir: &std::path::Path,
        view_name: &str,
        vars: &mut Vec<(String, String)>,
        seen: &mut std::collections::HashSet<String>,
    ) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };

        for entry in entries.flatten() {
            let path = entry.path();

            if path.is_dir() {
                // Recurse into subdirectories
                self.search_php_files_for_view_vars(&path, view_name, vars, seen);
                continue;
            }

            if !path.extension().map(|e| e == "php").unwrap_or(false) {
                continue;
            }

            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };

            // Check if this file references the view at all (quick check)
            if !content.contains(view_name) {
                continue;
            }

            self.extract_view_vars_from_content(&content, view_name, vars, seen);
        }
    }

    /// Extract variables from file content for a specific view
    fn extract_view_vars_from_content(
        &self,
        content: &str,
        view_name: &str,
        vars: &mut Vec<(String, String)>,
        seen: &mut std::collections::HashSet<String>,
    ) {
        let escaped_name = regex::escape(view_name);

        // Pattern 1: view('name', compact('a', 'b', ...))
        let compact_in_view = format!(
            r#"view\s*\(\s*['"]{}['"]\s*,\s*compact\s*\(([^)]+)\)"#,
            escaped_name
        );
        if let Ok(re) = regex::Regex::new(&compact_in_view) {
            for cap in re.captures_iter(content) {
                if let Some(compact_args) = cap.get(1) {
                    self.extract_vars_from_compact(content, compact_args.as_str(), vars, seen);
                }
            }
        }

        // Pattern 2: view('name', ['key' => ...])
        let array_in_view = format!(r#"view\s*\(\s*['"]{}['"]\s*,\s*\[([^\]]+)\]"#, escaped_name);
        if let Ok(re) = regex::Regex::new(&array_in_view) {
            for cap in re.captures_iter(content) {
                if let Some(array_content) = cap.get(1) {
                    self.extract_vars_from_array(content, array_content.as_str(), vars, seen);
                }
            }
        }

        // Pattern 3a: view('name')->with(['key' => $value, ...]) - array notation
        // Handle this separately to better capture array contents
        let with_array_pattern = format!(
            r#"view\s*\(\s*['"]{}['"]\s*\)\s*->\s*with\s*\(\s*\[([^\]]+)\]"#,
            escaped_name
        );
        if let Ok(re) = regex::Regex::new(&with_array_pattern) {
            for cap in re.captures_iter(content) {
                if let Some(array_content) = cap.get(1) {
                    self.extract_vars_from_array(content, array_content.as_str(), vars, seen);
                }
            }
        }

        // Pattern 3b: view('name')->with('key', $value) - single key-value or chained
        let with_single_pattern = format!(
            r#"view\s*\(\s*['"]{}['"]\s*\)(\s*->\s*with\s*\(\s*['"][^'"]+['"]\s*,[^)]+\))+"#,
            escaped_name
        );
        if let Ok(re) = regex::Regex::new(&with_single_pattern) {
            for cap in re.captures_iter(content) {
                let full_match = cap.get(0).map(|m| m.as_str()).unwrap_or("");
                self.extract_vars_from_with_chain(content, full_match, vars, seen);
            }
        }

        // Pattern 3c: view('name')->with(compact(...))
        let with_compact_pattern = format!(
            r#"view\s*\(\s*['"]{}['"]\s*\)\s*->\s*with\s*\(\s*compact\s*\(([^)]+)\)"#,
            escaped_name
        );
        if let Ok(re) = regex::Regex::new(&with_compact_pattern) {
            for cap in re.captures_iter(content) {
                if let Some(compact_args) = cap.get(1) {
                    self.extract_vars_from_compact(content, compact_args.as_str(), vars, seen);
                }
            }
        }

        // Pattern 4: View::make('name')->with(...) or View::make('name', [...])
        let view_make_pattern = format!(
            r#"View::make\s*\(\s*['"]{}['"]\s*(?:,\s*([^\)]+))?\)(\s*->\s*with\s*\([^)]+\))*"#,
            escaped_name
        );
        if let Ok(re) = regex::Regex::new(&view_make_pattern) {
            // Hoist the compact() inner regex out of the loop to avoid
            // recompiling it on every iteration.
            let compact_re = regex::Regex::new(r#"compact\s*\(([^)]+)\)"#).ok();

            for cap in re.captures_iter(content) {
                // Check for second argument (array or compact)
                if let Some(second_arg) = cap.get(1) {
                    let arg_str = second_arg.as_str();
                    if arg_str.contains("compact") {
                        if let Some(compact_match) =
                            compact_re.as_ref().and_then(|re| re.captures(arg_str))
                        {
                            if let Some(compact_args) = compact_match.get(1) {
                                self.extract_vars_from_compact(
                                    content,
                                    compact_args.as_str(),
                                    vars,
                                    seen,
                                );
                            }
                        }
                    } else if arg_str.starts_with('[') || arg_str.contains("=>") {
                        self.extract_vars_from_array(content, arg_str, vars, seen);
                    }
                }

                // Check for ->with chain
                let full_match = cap.get(0).map(|m| m.as_str()).unwrap_or("");
                if full_match.contains("->with") {
                    self.extract_vars_from_with_chain(content, full_match, vars, seen);
                }
            }
        }
    }

    /// Extract variable names from compact('a', 'b', 'c')
    fn extract_vars_from_compact(
        &self,
        content: &str,
        compact_args: &str,
        vars: &mut Vec<(String, String)>,
        seen: &mut std::collections::HashSet<String>,
    ) {
        let var_re = regex::Regex::new(r#"['"]([a-zA-Z_][a-zA-Z0-9_]*)['"]"#).ok();
        if let Some(re) = var_re {
            for var_cap in re.captures_iter(compact_args) {
                if let Some(var_name) = var_cap.get(1) {
                    let name = var_name.as_str().to_string();
                    if seen.insert(name.clone()) {
                        let var_type =
                            Self::find_variable_type_in_content(content, var_name.as_str())
                                .unwrap_or_else(|| "mixed".to_string());
                        vars.push((name, var_type));
                    }
                }
            }
        }
    }

    /// Extract variable names from ['key' => $value, ...] or ['key' => $this->property, ...]
    fn extract_vars_from_array(
        &self,
        content: &str,
        array_content: &str,
        vars: &mut Vec<(String, String)>,
        seen: &mut std::collections::HashSet<String>,
    ) {
        // Match 'key' => $this->property or 'key' => $variable
        let array_re = regex::Regex::new(
            r#"['"]([a-zA-Z_][a-zA-Z0-9_]*)['"]\s*=>\s*(?:\$this->([a-zA-Z_][a-zA-Z0-9_]*)|\$([a-zA-Z_][a-zA-Z0-9_]*))"#
        ).ok();

        if let Some(re) = array_re {
            for arr_cap in re.captures_iter(array_content) {
                if let Some(key) = arr_cap.get(1) {
                    let name = key.as_str().to_string();
                    if seen.insert(name.clone()) {
                        // Check if it's $this->property (group 2) or $variable (group 3)
                        let var_type = if let Some(prop_match) = arr_cap.get(2) {
                            // It's $this->property
                            laravel_lsp::php_class::find_property_type_in_content(
                                content,
                                prop_match.as_str(),
                            )
                        } else if let Some(var_match) = arr_cap.get(3) {
                            // It's $variable
                            Self::find_variable_type_in_content(content, var_match.as_str())
                        } else {
                            None
                        }
                        .unwrap_or_else(|| "mixed".to_string());
                        vars.push((name, var_type));
                    }
                }
            }
        }

        // Also handle keys without explicit value assignment (just the key name)
        let key_only_re =
            regex::Regex::new(r#"['"]([a-zA-Z_][a-zA-Z0-9_]*)['"](?:\s*,|\s*\])"#).ok();
        if let Some(re) = key_only_re {
            for cap in re.captures_iter(array_content) {
                if let Some(key) = cap.get(1) {
                    let name = key.as_str().to_string();
                    if seen.insert(name.clone()) {
                        vars.push((name, "mixed".to_string()));
                    }
                }
            }
        }
    }

    /// Extract variable names from ->with('key', $value) or ->with(['key' => $value]) chains
    fn extract_vars_from_with_chain(
        &self,
        content: &str,
        chain_str: &str,
        vars: &mut Vec<(String, String)>,
        seen: &mut std::collections::HashSet<String>,
    ) {
        // Match ->with('key', $value) or ->with('key', $this->property) pattern
        let single_with_re = regex::Regex::new(
            r#"->\s*with\s*\(\s*['"]([a-zA-Z_][a-zA-Z0-9_]*)['"]\s*,\s*(\$this->([a-zA-Z_][a-zA-Z0-9_]*)|\$([a-zA-Z_][a-zA-Z0-9_]*))"#
        ).ok();

        if let Some(re) = single_with_re {
            for cap in re.captures_iter(chain_str) {
                if let Some(key) = cap.get(1) {
                    let name = key.as_str().to_string();
                    if seen.insert(name.clone()) {
                        // Check if it's $this->property (group 3) or $variable (group 4)
                        let var_type = if let Some(prop_match) = cap.get(3) {
                            // It's $this->property - look up the property type
                            laravel_lsp::php_class::find_property_type_in_content(
                                content,
                                prop_match.as_str(),
                            )
                        } else if let Some(var_match) = cap.get(4) {
                            // It's $variable - look up the variable type
                            Self::find_variable_type_in_content(content, var_match.as_str())
                        } else {
                            None
                        }
                        .unwrap_or_else(|| "mixed".to_string());
                        vars.push((name, var_type));
                    }
                }
            }
        }

        // Match ->with(['key' => $value, ...]) pattern
        let array_with_re = regex::Regex::new(r#"->\s*with\s*\(\s*\[([^\]]+)\]\s*\)"#).ok();
        if let Some(re) = array_with_re {
            for cap in re.captures_iter(chain_str) {
                if let Some(array_content) = cap.get(1) {
                    self.extract_vars_from_array(content, array_content.as_str(), vars, seen);
                }
            }
        }

        // Match ->with(compact('a', 'b')) pattern
        let compact_with_re =
            regex::Regex::new(r#"->\s*with\s*\(\s*compact\s*\(([^)]+)\)\s*\)"#).ok();
        if let Some(re) = compact_with_re {
            for cap in re.captures_iter(chain_str) {
                if let Some(compact_args) = cap.get(1) {
                    self.extract_vars_from_compact(content, compact_args.as_str(), vars, seen);
                }
            }
        }
    }

    /// Find a variable's type by tracing its declaration in the content
    /// Handles multiple patterns:
    /// - Type hints: `User $user = ...`
    /// - New expressions: `$user = new User(...)`
    /// - Static methods: `$user = User::find(1)`, `$users = User::all()`
    /// - Query chains: `$user = User::where(...)->first()`
    /// - Function parameters: `function show(User $user)`
    /// - PHPDoc: `@var User $user` or `@param User $user`
    fn find_variable_type_in_content(content: &str, var_name: &str) -> Option<String> {
        let escaped_var = regex::escape(var_name);

        // 1. PHPDoc @var annotation: /** @var User $user */
        let phpdoc_var_pattern = format!(r#"@var\s+\\?([A-Z][a-zA-Z0-9_\\]*)\s+\${}"#, escaped_var);
        if let Some(caps) = regex::Regex::new(&phpdoc_var_pattern)
            .ok()
            .and_then(|re| re.captures(content))
        {
            if let Some(type_match) = caps.get(1) {
                return Some(laravel_lsp::php_class::simplify_type(type_match.as_str()));
            }
        }

        // 2. Type-hinted variable declaration: User $user = ...
        let type_hint_pattern =
            format!(r#"(\?)?\\?([A-Z][a-zA-Z0-9_\\]*)\s+\${}\s*="#, escaped_var);
        if let Some(caps) = regex::Regex::new(&type_hint_pattern)
            .ok()
            .and_then(|re| re.captures(content))
        {
            if let Some(type_match) = caps.get(2) {
                let nullable = caps.get(1).is_some();
                let type_name = laravel_lsp::php_class::simplify_type(type_match.as_str());
                return Some(if nullable {
                    format!("?{}", type_name)
                } else {
                    type_name
                });
            }
        }

        // 3. Function/method parameter type hint: function show(User $user)
        let param_pattern = format!(
            r#"function\s+\w+\s*\([^)]*(\?)?\\?([A-Z][a-zA-Z0-9_\\]*)\s+\${}"#,
            escaped_var
        );
        if let Some(caps) = regex::Regex::new(&param_pattern)
            .ok()
            .and_then(|re| re.captures(content))
        {
            if let Some(type_match) = caps.get(2) {
                let nullable = caps.get(1).is_some();
                let type_name = laravel_lsp::php_class::simplify_type(type_match.as_str());
                return Some(if nullable {
                    format!("?{}", type_name)
                } else {
                    type_name
                });
            }
        }

        // 4. New expression: $user = new User(...)
        let new_pattern = format!(r#"\${}\s*=\s*new\s+\\?([A-Z][a-zA-Z0-9_\\]*)"#, escaped_var);
        if let Some(caps) = regex::Regex::new(&new_pattern)
            .ok()
            .and_then(|re| re.captures(content))
        {
            if let Some(class) = caps.get(1) {
                return Some(laravel_lsp::php_class::simplify_type(class.as_str()));
            }
        }

        // 5. Static method returning single model: Model::find(), ::first(), ::findOrFail(), etc.
        let single_model_pattern = format!(
            r#"\${}\s*=\s*\\?([A-Z][a-zA-Z0-9_\\]*)::(?:find|findOrFail|first|firstOrFail|sole|firstWhere|create|updateOrCreate|firstOrCreate|firstOrNew)\s*\("#,
            escaped_var
        );
        if let Some(caps) = regex::Regex::new(&single_model_pattern)
            .ok()
            .and_then(|re| re.captures(content))
        {
            if let Some(model) = caps.get(1) {
                return Some(laravel_lsp::php_class::simplify_type(model.as_str()));
            }
        }

        // 6. Static method returning collection: Model::all(), ::get(), etc.
        let collection_pattern = format!(
            r#"\${}\s*=\s*\\?([A-Z][a-zA-Z0-9_\\]*)::(?:all|get|paginate|simplePaginate|cursor)\s*\("#,
            escaped_var
        );
        if let Some(caps) = regex::Regex::new(&collection_pattern)
            .ok()
            .and_then(|re| re.captures(content))
        {
            if let Some(model) = caps.get(1) {
                return Some(format!(
                    "Collection<{}>",
                    laravel_lsp::php_class::simplify_type(model.as_str())
                ));
            }
        }

        // 7. Query chain ending in first/find: Model::where(...)->first()
        let chain_single_pattern = format!(
            r#"\${}\s*=\s*\\?([A-Z][a-zA-Z0-9_\\]*)::.*->(?:first|find|sole|firstOrFail|findOrFail)\s*\("#,
            escaped_var
        );
        if let Some(caps) = regex::Regex::new(&chain_single_pattern)
            .ok()
            .and_then(|re| re.captures(content))
        {
            if let Some(model) = caps.get(1) {
                return Some(laravel_lsp::php_class::simplify_type(model.as_str()));
            }
        }

        // 8. Query chain ending in get/all: Model::where(...)->get()
        let chain_collection_pattern = format!(
            r#"\${}\s*=\s*\\?([A-Z][a-zA-Z0-9_\\]*)::.*->(?:get|all|paginate)\s*\("#,
            escaped_var
        );
        if let Some(caps) = regex::Regex::new(&chain_collection_pattern)
            .ok()
            .and_then(|re| re.captures(content))
        {
            if let Some(model) = caps.get(1) {
                return Some(format!(
                    "Collection<{}>",
                    laravel_lsp::php_class::simplify_type(model.as_str())
                ));
            }
        }

        // 9. Common Laravel helpers
        // auth()->user() returns User (typically)
        let auth_user_pattern = format!(
            r#"\${}\s*=\s*(?:auth\(\)|Auth::user\(\))->user\s*\(\)"#,
            escaped_var
        );
        if regex::Regex::new(&auth_user_pattern)
            .ok()
            .map(|re| re.is_match(content))
            .unwrap_or(false)
        {
            return Some("User".to_string());
        }

        // request() or $request typically is Request
        if var_name == "request" {
            return Some("Request".to_string());
        }

        None
    }

    // `simplify_type`, `find_property_type_in_content`, `extract_method_return_type`,
    // `normalize_generic_type`, `parse_generic_args`, and `iterable_element_type` live
    // in `laravel_lsp::php_class` so the Salsa actor can call them.

    /// Extract variables from a View component class
    fn extract_component_variables(
        &self,
        root: &std::path::Path,
        blade_path: &str,
    ) -> Vec<(String, String)> {
        let vars = Vec::new();

        // Check if this is a component view (in resources/views/components/)
        let components_marker = format!("{}resources/views/components/", std::path::MAIN_SEPARATOR);
        let components_marker_alt = "resources/views/components/";

        if !blade_path.contains(&components_marker) && !blade_path.contains(components_marker_alt) {
            return vars;
        }

        // Extract component name from path
        // e.g., resources/views/components/button.blade.php -> Button
        // e.g., resources/views/components/forms/input.blade.php -> Forms/Input
        let relative = if let Some(idx) = blade_path.find("components/") {
            &blade_path[idx + 11..] // Skip "components/"
        } else {
            return vars;
        };

        let without_ext = relative.strip_suffix(".blade.php").unwrap_or(relative);
        let class_name = Self::path_to_class_name(without_ext);

        // Find the component class
        let class_path = root
            .join("app")
            .join("View")
            .join("Components")
            .join(format!("{}.php", class_name));

        if !class_path.exists() {
            // Try nested structure
            let parts: Vec<&str> = without_ext.split('/').collect();
            if parts.len() > 1 {
                let nested_path = root
                    .join("app")
                    .join("View")
                    .join("Components")
                    .join(parts[..parts.len() - 1].join("/"))
                    .join(format!(
                        "{}.php",
                        Self::kebab_to_pascal(parts[parts.len() - 1])
                    ));
                if nested_path.exists() {
                    return self.parse_component_class_vars(&nested_path);
                }
            }
            return vars;
        }

        self.parse_component_class_vars(&class_path)
    }

    /// Parse a component class file for public properties
    fn parse_component_class_vars(&self, class_path: &std::path::Path) -> Vec<(String, String)> {
        let mut vars = Vec::new();

        let Ok(content) = std::fs::read_to_string(class_path) else {
            return vars;
        };

        // Extract public properties
        let prop_re = regex::Regex::new(
            r#"public\s+(?:(\?)?([A-Z][a-zA-Z0-9_\\]*)\s+)?\$([a-zA-Z_][a-zA-Z0-9_]*)"#,
        )
        .ok();

        if let Some(re) = prop_re {
            for cap in re.captures_iter(&content) {
                if let Some(name) = cap.get(3) {
                    let php_type = cap
                        .get(2)
                        .map(|t| {
                            let type_str = t.as_str();
                            type_str.rsplit('\\').next().unwrap_or(type_str).to_string()
                        })
                        .unwrap_or_else(|| "mixed".to_string());
                    vars.push((name.as_str().to_string(), php_type));
                }
            }
        }

        // Extract constructor parameters (they become available in the view)
        let constructor_re =
            regex::Regex::new(r#"public\s+function\s+__construct\s*\(([^)]*)\)"#).ok();

        if let Some(re) = constructor_re {
            if let Some(cap) = re.captures(&content) {
                if let Some(params) = cap.get(1) {
                    let param_re = regex::Regex::new(
                        r#"(?:public\s+)?(?:(\?)?([A-Z][a-zA-Z0-9_\\]*)\s+)?\$([a-zA-Z_][a-zA-Z0-9_]*)"#
                    ).ok();

                    if let Some(pre) = param_re {
                        for pcap in pre.captures_iter(params.as_str()) {
                            if let Some(name) = pcap.get(3) {
                                let php_type = pcap
                                    .get(2)
                                    .map(|t| {
                                        let type_str = t.as_str();
                                        type_str.rsplit('\\').next().unwrap_or(type_str).to_string()
                                    })
                                    .unwrap_or_else(|| "mixed".to_string());
                                // Avoid duplicates
                                let name_str = name.as_str().to_string();
                                if !vars.iter().any(|(n, _)| n == &name_str) {
                                    vars.push((name_str, php_type));
                                }
                            }
                        }
                    }
                }
            }
        }

        vars
    }

    /// Convert path like "forms/input" to class name "Forms\Input"
    fn path_to_class_name(path: &str) -> String {
        path.split('/')
            .map(Self::kebab_to_pascal)
            .collect::<Vec<_>>()
            .join("/")
    }

    /// Extract public properties from the Livewire component backing a Blade view.
    /// Works for Livewire 4 single-file components, multi-file components, and
    /// class-based components (the Livewire 3 carry-over format).
    ///
    /// Untyped declarations (`public $foo;`) get their type refined from a matching
    /// `mount()` parameter type-hint when one is available — mirroring Livewire's
    /// runtime auto-assignment behavior. The property declaration is always required;
    /// mount params alone never synthesize a property.
    fn extract_livewire_variables(
        &self,
        root: &std::path::Path,
        blade_path: &str,
    ) -> Vec<(String, String)> {
        let mut vars: Vec<(String, String)> = Vec::new();

        let Some(component_path) = self.find_livewire_component_php(root, blade_path) else {
            return vars;
        };
        let Ok(content) = std::fs::read_to_string(&component_path) else {
            return vars;
        };

        // Public property declarations. The regex scans the whole source, so SFCs with
        // surrounding Blade markup match through.
        let prop_re = regex::Regex::new(
            r#"public\s+(?:(\?)?([A-Z][a-zA-Z0-9_\\]*)\s+)?\$([a-zA-Z_][a-zA-Z0-9_]*)"#,
        )
        .ok();

        if let Some(re) = prop_re {
            for cap in re.captures_iter(&content) {
                if let Some(name) = cap.get(3) {
                    let var_name = name.as_str().to_string();
                    if vars.iter().any(|(n, _)| n == &var_name) {
                        continue;
                    }

                    let php_type = match cap.get(2) {
                        Some(t) => {
                            let type_str = t.as_str();
                            type_str.rsplit('\\').next().unwrap_or(type_str).to_string()
                        }
                        // Untyped property → mount param refinement, then route binding,
                        // then fall back to "mixed". Same three-tier order as the
                        // single-variable lookup in `check_livewire_component_variable`.
                        None => laravel_lsp::php_class::find_mount_param_type(&content, &var_name)
                            .or_else(|| {
                                laravel_lsp::route_binding::find_route_binding_type(
                                    std::path::Path::new(blade_path),
                                    root,
                                    &var_name,
                                )
                            })
                            .unwrap_or_else(|| "mixed".to_string()),
                    };

                    vars.push((var_name, php_type));
                }
            }
        }

        vars
    }

    /// Extract all variable property accesses from Blade content
    /// Returns (variable_name, line, column, end_column) for each $var-> pattern
    fn extract_blade_variable_accesses(content: &str) -> Vec<VariableAccess> {
        let mut accesses = Vec::new();

        // Match $variable-> pattern (with property access)
        // We only want to flag variables that are being accessed for properties/methods
        let re = regex::Regex::new(r"\$([a-zA-Z_][a-zA-Z0-9_]*)->").unwrap();

        for (line_num, line) in content.lines().enumerate() {
            for cap in re.captures_iter(line) {
                if let Some(var_match) = cap.get(1) {
                    let var_name = var_match.as_str().to_string();

                    // Skip common framework variables that don't need type resolution
                    let framework_vars = [
                        "this",
                        "loop",
                        "errors",
                        "slot",
                        "component",
                        "attributes",
                        "__env",
                        "__data",
                        "obLevel",
                        "app",
                        "request",
                    ];
                    if framework_vars.contains(&var_name.as_str()) {
                        continue;
                    }

                    // Calculate the full match position (including $ prefix)
                    let full_match = cap.get(0).unwrap();
                    let column = full_match.start() as u32;
                    // End column is just before the -> (at the end of the variable name)
                    let end_column = (full_match.start() + 1 + var_name.len()) as u32;

                    accesses.push(VariableAccess {
                        variable_name: format!("${}", var_name),
                        line: line_num as u32,
                        column,
                        end_column,
                    });
                }
            }
        }

        accesses
    }

    /// Extract view name from file path
    /// e.g., /project/resources/views/users/show.blade.php -> users.show
    fn extract_view_name_from_path(&self, path: &str, root: &std::path::Path) -> Option<String> {
        let views_dir = root.join("resources").join("views");
        let views_str = views_dir.to_string_lossy();

        if let Some(relative) = path.strip_prefix(views_str.as_ref()) {
            let relative = relative.trim_start_matches('/').trim_start_matches('\\');
            let without_ext = relative.strip_suffix(".blade.php")?;
            Some(without_ext.replace(['/', '\\'], "."))
        } else {
            None
        }
    }

    /// Resolve a Blade-scoped variable (e.g. `$form`) against the Livewire component backing
    /// this view. Returns the declared type for explicit `public Type $var;` properties.
    ///
    /// Type resolution falls through three tiers, all gated on the property being declared
    /// (Livewire never synthesizes properties from mount params or route bindings alone):
    ///   1. Explicit property type (`public Post $post;`)
    ///   2. Matching `mount()` parameter's type-hint (Livewire auto-assigns the param value)
    ///   3. Matching `Route::livewire(...)` parameter, with the model class inferred from
    ///      the param name via Laravel's PascalCase convention (`{post}` → `Post`)
    fn check_livewire_component_variable(
        &self,
        root: &std::path::Path,
        blade_path: &str,
        var_name: &str,
    ) -> Option<String> {
        let component_path = self.find_livewire_component_php(root, blade_path)?;
        let content = std::fs::read_to_string(&component_path).ok()?;

        if !Self::has_public_property(&content, var_name) {
            return None;
        }

        if let Some(type_name) = Self::extract_property_type(&content, var_name) {
            return Some(type_name);
        }

        if let Some(type_name) = laravel_lsp::php_class::find_mount_param_type(&content, var_name) {
            return Some(type_name);
        }

        laravel_lsp::route_binding::find_route_binding_type(
            std::path::Path::new(blade_path),
            root,
            var_name,
        )
    }

    /// Map a Blade view path under `resources/views/livewire/` to its Livewire component PHP file.
    /// Handles nested namespaces: `resources/views/livewire/decision-cloud/audits/index.blade.php`
    /// -> `app/Livewire/DecisionCloud/Audits/Index.php` (or `app/Http/Livewire/...` for v2).
    fn view_path_to_livewire_class_path(
        &self,
        root: &std::path::Path,
        view_path: &str,
    ) -> Option<std::path::PathBuf> {
        let livewire_views = root.join("resources").join("views").join("livewire");
        let views_str = livewire_views.to_string_lossy();

        let relative = view_path
            .strip_prefix(views_str.as_ref())?
            .trim_start_matches('/')
            .trim_start_matches('\\');
        let without_ext = relative.strip_suffix(".blade.php")?;

        // Convert each path segment from kebab-case to PascalCase
        let class_parts: Vec<String> = without_ext.split('/').map(Self::kebab_to_pascal).collect();
        let class_relative = class_parts.join("/");

        // Try Livewire v3 (app/Livewire) first, then v2 (app/Http/Livewire)
        for base in ["Livewire", "Http/Livewire"] {
            let path = root
                .join("app")
                .join(base)
                .join(format!("{}.php", class_relative));
            if path.exists() {
                return Some(path);
            }
        }

        None
    }

    /// Locate the PHP source containing the Livewire component definition for a Blade view.
    /// Covers all three Livewire 4 component formats; resolution order matches Livewire's
    /// own component-discovery preference (co-located formats win when both apply):
    ///   1. **Multi-File Component (MFC)** — sibling `.php` file with the same stem
    ///      containing `new class extends Component` (typical layout: `⚡foo/foo.blade.php`
    ///      paired with `⚡foo/foo.php`).
    ///   2. **Single-File Component (SFC)** — the same `.blade.php` file containing
    ///      inline `<?php new class extends Component ...; ?>`.
    ///   3. **Class-based** (Livewire 3 carry-over) —
    ///      `resources/views/livewire/foo.blade.php` mapped to `app/Livewire/Foo.php`.
    ///
    /// For SFC the returned path is the blade file itself; the property-extraction regexes
    /// match through the surrounding Blade content because they pattern-match `public Type $name`
    /// without caring about file framing.
    fn find_livewire_component_php(
        &self,
        root: &std::path::Path,
        blade_path: &str,
    ) -> Option<std::path::PathBuf> {
        let blade = std::path::Path::new(blade_path);

        if let Some(sibling) = mfc_sibling(blade) {
            return Some(sibling);
        }

        if blade_contains_inline_class(blade) {
            return Some(blade.to_path_buf());
        }

        self.view_path_to_livewire_class_path(root, blade_path)
    }

    /// Given a Livewire-view iterable expression (e.g. "$this->audits") and the view's path,
    /// resolve the iterable's declared type, then extract its element type if generics are present.
    /// Returns:
    ///   - `Some(("Audit", true))` when an element type was extracted from generics
    ///   - `Some((type_str, false))` when the iterable resolved but no element type could be inferred
    ///     (caller may treat this as "iterable but element unknown" — suppress diagnostic, no autocomplete)
    ///   - `None` when the expression couldn't be resolved at all
    async fn resolve_iterable_type_info(
        &self,
        root: &std::path::Path,
        view_path: &str,
        iterable_expr: &str,
    ) -> Option<(String, bool)> {
        // Only handle `$this->X` against a Livewire component for now.
        // `X` can be a public property or a #[Computed] method.
        let trimmed = iterable_expr.trim();
        let member = trimmed.strip_prefix("$this->")?;

        // Reject anything with further chaining/calls — we only resolve simple $this->name.
        // Method-call form $this->name() is also accepted (drop trailing ()).
        let member_name = member.trim_end_matches("()");
        if member_name.contains(['-', '>', '(', ')', '.', '[', ']', ' ', ':']) {
            return None;
        }

        let component_path = self.find_livewire_component_php(root, view_path)?;

        // Dispatch through Salsa. The actor auto-registers the file as a SourceFile
        // and invalidates on mtime change (the file isn't typically open in the editor).
        let resolved = self
            .salsa
            .resolve_livewire_member(component_path, member_name.to_string())
            .await
            .ok()
            .flatten()?;

        let element = laravel_lsp::php_class::iterable_element_type(&resolved);
        match element {
            Some(elem) => Some((elem, true)),
            None => Some((resolved, false)),
        }
    }

    /// Hardcoded properties of Laravel's `$loop` Blade variable.
    /// Reference: https://laravel.com/docs/blade#the-loop-variable
    fn loop_var_properties() -> Vec<ModelPropertyCompletion> {
        let entries = [
            ("index", "int"),
            ("iteration", "int"),
            ("remaining", "int"),
            ("count", "int"),
            ("first", "bool"),
            ("last", "bool"),
            ("even", "bool"),
            ("odd", "bool"),
            ("depth", "int"),
            ("parent", "?stdClass"),
        ];
        entries
            .iter()
            .map(|(name, ty)| ModelPropertyCompletion {
                name: (*name).to_string(),
                php_type: (*ty).to_string(),
                source: "blade-loop".to_string(),
            })
            .collect()
    }

    /// Check if variable is defined in a View component class
    fn check_view_component_variable(
        &self,
        root: &std::path::Path,
        blade_path: &str,
        var_name: &str,
    ) -> Option<String> {
        // View components can be:
        // 1. Anonymous: resources/views/components/button.blade.php (no class)
        // 2. Class-based: app/View/Components/Button.php with resources/views/components/button.blade.php

        // Check if this is in the components directory
        if !blade_path.contains("/components/") {
            return None;
        }

        // Extract component name from path
        // resources/views/components/forms/input.blade.php -> Forms/Input
        let views_components = root.join("resources").join("views").join("components");
        let views_str = views_components.to_string_lossy();

        let relative = blade_path
            .strip_prefix(views_str.as_ref())?
            .trim_start_matches('/');
        let without_ext = relative.strip_suffix(".blade.php")?;

        // Convert to class path: forms/input -> Forms/Input
        let class_path_parts: Vec<String> =
            without_ext.split('/').map(Self::kebab_to_pascal).collect();
        let class_relative = class_path_parts.join("/");

        let class_path = root
            .join("app")
            .join("View")
            .join("Components")
            .join(format!("{}.php", class_relative));

        if class_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&class_path) {
                // Check constructor parameters and public properties
                if let Some(type_name) = Self::extract_property_type(&content, var_name) {
                    return Some(type_name);
                }
                if let Some(type_name) = Self::extract_constructor_param_type(&content, var_name) {
                    return Some(type_name);
                }
            }
        }

        None
    }

    /// Check if variable is passed from a controller to this view
    fn check_controller_view_variable(
        &self,
        root: &std::path::Path,
        view_name: &str,
        var_name: &str,
    ) -> Option<String> {
        // Scan controllers for view() calls that match this view
        let controllers_dir = root.join("app").join("Http").join("Controllers");
        if !controllers_dir.exists() {
            return None;
        }

        // Walk through all controller files
        for entry in walkdir::WalkDir::new(&controllers_dir)
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .map(|ext| ext == "php")
                    .unwrap_or(false)
            })
        {
            if let Ok(content) = std::fs::read_to_string(entry.path()) {
                if let Some(type_name) =
                    Self::extract_view_variable_type(&content, view_name, var_name)
                {
                    return Some(type_name);
                }
            }
        }

        None
    }

    /// Check whether a `public $name` declaration exists in `content` — regardless of
    /// whether the declaration carries an explicit type. Used to gate mount-param type
    /// refinement: a mount() param's type must not be surfaced as a property's type
    /// unless the property itself is declared (Livewire never synthesizes properties
    /// from mount() params alone).
    fn has_public_property(content: &str, property_name: &str) -> bool {
        let escaped = regex::escape(property_name);
        let pattern = format!(
            r"public\s+(?:\??\\?[A-Za-z_][A-Za-z0-9_\\]*\s+)?\${}\b",
            escaped
        );
        regex::Regex::new(&pattern)
            .ok()
            .map(|re| re.is_match(content))
            .unwrap_or(false)
    }

    /// Extract property type from a PHP class (public properties with type hints)
    fn extract_property_type(content: &str, property_name: &str) -> Option<String> {
        // Match: public TypeName $propertyName or public ?TypeName $propertyName
        let pattern = format!(
            r"public\s+\??([A-Z][a-zA-Z0-9_\\]*)\s+\${}(?:\s*[;=])",
            regex::escape(property_name)
        );

        regex::Regex::new(&pattern)
            .ok()
            .and_then(|re| re.captures(content))
            .and_then(|caps| caps.get(1))
            .map(|m| {
                let full_type = m.as_str();
                // Get just the class name (last part after \)
                full_type
                    .rsplit('\\')
                    .next()
                    .unwrap_or(full_type)
                    .to_string()
            })
    }

    /// Extract constructor parameter type
    fn extract_constructor_param_type(content: &str, param_name: &str) -> Option<String> {
        // Match: public function __construct(TypeName $paramName or __construct(..., TypeName $paramName
        let pattern = format!(
            r"__construct\s*\([^)]*\??([A-Z][a-zA-Z0-9_\\]*)\s+\${}",
            regex::escape(param_name)
        );

        regex::Regex::new(&pattern)
            .ok()
            .and_then(|re| re.captures(content))
            .and_then(|caps| caps.get(1))
            .map(|m| {
                let full_type = m.as_str();
                full_type
                    .rsplit('\\')
                    .next()
                    .unwrap_or(full_type)
                    .to_string()
            })
    }

    /// Extract variable type from a controller method that renders a specific view
    fn extract_view_variable_type(
        content: &str,
        view_name: &str,
        var_name: &str,
    ) -> Option<String> {
        // Look for view('view.name', [...]) or view('view.name')->with([...])
        // and extract the type of the variable being passed

        // First, find if this controller renders the target view
        let view_pattern = format!(r#"view\s*\(\s*['"]{}['"]"#, regex::escape(view_name));
        let view_re = regex::Regex::new(&view_pattern).ok()?;

        if !view_re.is_match(content) {
            return None;
        }

        // Find the method that contains this view call and extract variable types
        // This is complex because we need to:
        // 1. Find the method containing the view() call
        // 2. Look for type hints on variables passed to the view

        // Simplified approach: look for compact('varName') or 'varName' => $varName patterns
        // and then find the type of $varName in the same method

        // Pattern for compact('varName') in view call
        let compact_pattern = format!(
            r#"view\s*\(\s*['"]{}['"]\s*,\s*compact\s*\([^)]*['"]{}['"]"#,
            regex::escape(view_name),
            regex::escape(var_name)
        );

        // Pattern for 'varName' => $varName in view call
        let array_pattern = format!(
            r#"view\s*\(\s*['"]{}['"]\s*,\s*\[[^\]]*['"]{}['"]\s*=>"#,
            regex::escape(view_name),
            regex::escape(var_name)
        );

        let has_var = regex::Regex::new(&compact_pattern).ok()?.is_match(content)
            || regex::Regex::new(&array_pattern).ok()?.is_match(content);

        if !has_var {
            return None;
        }

        // Now find the type of $varName in this file
        // Check for type hints on method parameters or variable declarations
        Self::resolve_variable_type(content, &format!("${}", var_name))
    }

    /// Convert kebab-case to PascalCase
    /// user-settings -> UserSettings
    fn kebab_to_pascal(s: &str) -> String {
        s.split('-')
            .map(|part| {
                let mut chars = part.chars();
                match chars.next() {
                    Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            })
            .collect()
    }

    /// Check if cursor is inside __('...'), trans('...'), or other translation-related calls
    /// Returns context with position info for text replacement
    ///
    /// Examples:
    /// - `__('messages.` returns Some(StringContext{prefix: "messages.", ...})
    /// - `trans('auth.` returns Some(StringContext{prefix: "auth.", ...})
    /// - `Lang::get('` returns Some(StringContext{prefix: "", ...})
    fn get_translation_call_context(line_text: &str, character: u32) -> Option<StringContext> {
        let cursor = character as usize;
        if cursor > line_text.len() {
            return None;
        }

        let before_cursor = &line_text[..cursor];

        // Look for various translation patterns before cursor
        // Pattern: (pattern_string, quote_char, pattern_length)
        let patterns: Vec<(&str, char, usize)> = vec![
            // __(' or __("
            ("__('", '\'', 4),
            ("__(\"", '"', 4),
            // trans(' or trans("
            ("trans('", '\'', 7),
            ("trans(\"", '"', 7),
            // trans_choice(' or trans_choice("
            ("trans_choice('", '\'', 14),
            ("trans_choice(\"", '"', 14),
            // Lang::get('
            ("Lang::get('", '\'', 11),
            ("Lang::get(\"", '"', 11),
            // Lang::has('
            ("Lang::has('", '\'', 11),
            ("Lang::has(\"", '"', 11),
            // Lang::choice('
            ("Lang::choice('", '\'', 14),
            ("Lang::choice(\"", '"', 14),
            // Lang::hasForLocale('
            ("Lang::hasForLocale('", '\'', 20),
            ("Lang::hasForLocale(\"", '"', 20),
            // @lang(' - Blade directive
            ("@lang('", '\'', 7),
            ("@lang(\"", '"', 7),
        ];

        // Find all matches and their positions
        let mut matches: Vec<(usize, char, usize)> = Vec::new();

        for (pattern, quote, len) in &patterns {
            if let Some(pos) = before_cursor.rfind(pattern) {
                matches.push((pos, *quote, *len));
            }
        }

        if matches.is_empty() {
            return None;
        }

        // Find the latest match (closest to cursor)
        let (pos, quote_char, pattern_len) = matches.into_iter().max_by_key(|(p, _, _)| *p)?;

        let start_pos = pos + pattern_len;

        // Check that there's no closing quote between start and cursor
        let after_pattern = &before_cursor[start_pos..];
        if after_pattern.contains(quote_char) {
            return None;
        }

        // Find where the string ends (closing quote or end of line)
        let end_col = Self::find_string_end(line_text, start_pos, quote_char);

        Some(StringContext {
            prefix: after_pattern.to_string(),
            start_col: start_pos as u32,
            end_col,
            quote_char,
        })
    }

    // ========================================================================
    // Validation Rule Parameter Context Detection
    // ========================================================================

    /// Check if cursor is inside a validation rule parameter context (after the colon)
    /// e.g., "exists:█" or "after:start_█" or "unique:users,█"
    fn get_validation_param_context(
        line_text: &str,
        character: u32,
        surrounding_lines: &[&str],
        cached_rules: &[String],
    ) -> Option<ValidationParamContext> {
        let cursor = character as usize;
        if cursor > line_text.len() {
            info!(
                "      ⚠️  Cursor {} > line length {}",
                cursor,
                line_text.len()
            );
            return None;
        }

        let before_cursor = &line_text[..cursor];
        info!("      📍 before_cursor: '{}'", before_cursor);

        // Use context-aware detection to determine what type of array we're in
        // This prevents triggering validation completions in $casts and other non-validation arrays
        let context = Self::detect_array_context(line_text, surrounding_lines);
        info!("      🔍 Array context: {:?}", context);

        let in_validation_context = match context {
            // Explicit validation context
            ArrayContext::Validation => true,
            // Explicit non-validation contexts
            ArrayContext::Casts
            | ArrayContext::MassAssignment
            | ArrayContext::Visibility
            | ArrayContext::Relationships => false,
            // Unknown - fall back to pattern matching
            ArrayContext::Unknown => {
                let line_has_context = Self::is_validation_context(line_text, cached_rules);
                let surrounding_has_context = surrounding_lines
                    .iter()
                    .any(|l| Self::is_validation_context(l, cached_rules));
                line_has_context || surrounding_has_context
            }
        };

        if !in_validation_context {
            info!("      ❌ Not in validation context");
            return None;
        }

        // Find if we're inside a quoted string with a colon (rule parameter)
        // Look for pattern like: 'rule_name:param' or "rule_name:param"
        let result = Self::extract_param_context(before_cursor);
        info!(
            "      📋 extract_param_context result: {:?}",
            result.as_ref().map(|c| (&c.rule_name, &c.current_param))
        );
        result
    }

    /// Extract the rule name and current parameter from before the cursor
    /// Returns None if not in a rule parameter context
    ///
    /// For input like `'after:field|exists:'` with cursor at end:
    /// - rule_name = "exists" (the rule immediately before cursor's colon)
    /// - current_param = "" (nothing after the colon yet)
    fn extract_param_context(before_cursor: &str) -> Option<ValidationParamContext> {
        // Check if we're inside a quoted string
        let single_quotes = before_cursor.matches('\'').count();
        let double_quotes = before_cursor.matches('"').count();
        let in_single_quoted = single_quotes % 2 == 1;
        let in_double_quoted = double_quotes % 2 == 1;

        if !in_single_quoted && !in_double_quoted {
            return None;
        }

        // Find the opening quote
        let quote_char = if in_single_quoted { '\'' } else { '"' };
        let last_quote_pos = before_cursor.rfind(quote_char)?;

        // Get content inside the quote (after the opening quote)
        let inside_quote = &before_cursor[last_quote_pos + 1..];

        // Find the LAST colon - this is the one immediately before cursor
        let last_colon_pos = inside_quote.rfind(':')?;

        // Extract rule name: find the start of the current rule segment
        // It's either after the last pipe before this colon, or the start of the string
        let before_last_colon = &inside_quote[..last_colon_pos];
        let rule_start = before_last_colon.rfind('|').map(|p| p + 1).unwrap_or(0);
        let rule_name = &before_last_colon[rule_start..];

        // Extract current parameter (after the last colon)
        let after_colon = &inside_quote[last_colon_pos + 1..];

        // Count commas to determine param_index, find text after last comma
        let (current_param, param_index) = if let Some(comma_pos) = after_colon.rfind(',') {
            // After a comma - we're on a subsequent parameter
            let param_count = after_colon.matches(',').count();
            (after_colon[comma_pos + 1..].to_string(), param_count)
        } else {
            // No comma - we're on the first parameter
            (after_colon.to_string(), 0)
        };

        Some(ValidationParamContext {
            rule_name: rule_name.to_string(),
            current_param,
            full_params: after_colon.to_string(),
            param_index,
        })
    }

    /// Extract field names from a validation array for field reference completions
    /// This finds all `'field_name' =>` or `"field_name" =>` patterns in the validation array
    /// Excludes the current field (the one being validated on cursor_line)
    fn extract_validation_fields(content: &str, cursor_line: usize) -> Vec<String> {
        let lines: Vec<&str> = content.lines().collect();
        info!(
            "      📄 extract_validation_fields: {} total lines, cursor at {}",
            lines.len(),
            cursor_line
        );
        if cursor_line >= lines.len() {
            info!(
                "      ❌ cursor_line {} >= lines.len() {}",
                cursor_line,
                lines.len()
            );
            return Vec::new();
        }

        // Extract the current field name from the cursor line to exclude it
        let current_line = lines[cursor_line];
        let current_field = Self::extract_field_name_from_line(current_line);
        info!(
            "      📍 Current line: '{}', current_field: {:?}",
            current_line.trim(),
            current_field
        );

        // Search backwards to find array start
        let mut bracket_count = 0;
        let mut array_start = cursor_line;
        let mut found_array_start = false;

        for i in (0..=cursor_line).rev() {
            let line = lines[i];
            // Count brackets (simplified - doesn't handle strings perfectly but good enough)
            for ch in line.chars().rev() {
                match ch {
                    ']' => bracket_count += 1,
                    '[' => {
                        if bracket_count > 0 {
                            bracket_count -= 1;
                        } else {
                            // Found the opening bracket of our array
                            array_start = i;
                            found_array_start = true;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if found_array_start {
                break;
            }
        }
        info!(
            "      🔍 Array boundaries: start={}, found={}",
            array_start, found_array_start
        );

        // Search forward to find array end
        bracket_count = 0;
        let mut array_end = cursor_line;
        let mut started = false;

        for (i, line) in lines.iter().enumerate().skip(array_start) {
            for ch in line.chars() {
                match ch {
                    '[' => {
                        started = true;
                        bracket_count += 1;
                    }
                    ']' => {
                        bracket_count -= 1;
                        if started && bracket_count == 0 {
                            array_end = i;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if started && bracket_count == 0 {
                break;
            }
        }
        info!(
            "      🔍 Array range: lines {} to {}",
            array_start, array_end
        );

        // Extract field names from the array range
        let mut fields = Vec::new();
        let field_pattern =
            regex::Regex::new(r#"['"]([a-zA-Z_][a-zA-Z0-9_.*]*)['"][ \t]*=>"#).unwrap();

        let end = array_end.min(lines.len().saturating_sub(1));
        for line in &lines[array_start..=end] {
            for caps in field_pattern.captures_iter(line) {
                if let Some(field_match) = caps.get(1) {
                    let field_name = field_match.as_str().to_string();

                    // Skip the current field (we don't want to suggest the field we're validating)
                    if let Some(ref current) = current_field {
                        if &field_name == current {
                            continue;
                        }
                    }

                    // Add both the exact field and any wildcard parent
                    if !fields.contains(&field_name) {
                        fields.push(field_name.clone());
                    }
                    // For nested fields like "items.*.price", also suggest "items"
                    if let Some(dot_pos) = field_name.find('.') {
                        let parent = field_name[..dot_pos].to_string();
                        if !fields.contains(&parent) && Some(&parent) != current_field.as_ref() {
                            fields.push(parent);
                        }
                    }
                }
            }
        }

        fields
    }

    /// Extract field name from a validation array line
    /// e.g., "'end_date' => 'after:'" returns Some("end_date")
    fn extract_field_name_from_line(line: &str) -> Option<String> {
        let field_pattern =
            regex::Regex::new(r#"['"]([a-zA-Z_][a-zA-Z0-9_.*]*)['"][ \t]*=>"#).unwrap();
        if let Some(caps) = field_pattern.captures(line) {
            if let Some(field_match) = caps.get(1) {
                return Some(field_match.as_str().to_string());
            }
        }
        None
    }

    /// Get completion items for validation rule parameters
    /// Based on the rule type, returns appropriate options (field refs, database tables, etc.)
    async fn get_validation_param_completions(
        &self,
        context: &ValidationParamContext,
        content: &str,
        cursor_line: usize,
        uri: &Url,
        position: Position,
    ) -> Vec<CompletionItem> {
        use laravel_lsp::validation_rules::{LaravelRulesParser, ParamType};

        // Get project root for vendor parsing
        let root = {
            let root_guard = self.root_path.read().await;
            match root_guard.as_ref() {
                Some(r) => r.clone(),
                None => return Vec::new(),
            }
        };

        let parser = LaravelRulesParser::new(root.clone());

        // Parse validation rules to determine rule type
        let rules = parser.parse_validation_rules();
        info!("   📖 Parsed {} validation rules from Laravel", rules.len());

        let rule_info = rules.iter().find(|r| r.name == context.rule_name);

        let param_type = match rule_info {
            Some(info) => {
                info!(
                    "   ✅ Found rule '{}' with param type {:?}",
                    context.rule_name, info.param_type
                );
                info.param_type.clone()
            }
            None => {
                // Unknown rule - check if it looks like a field reference rule
                // (common pattern for custom rules)
                info!(
                    "   ⚠️  Rule '{}' not found, defaulting to Custom",
                    context.rule_name
                );
                ParamType::Custom
            }
        };

        let prefix = &context.current_param;
        let prefix_lower = prefix.to_lowercase();

        match param_type {
            ParamType::None => {
                info!("   ℹ️  Rule has no parameters");
                Vec::new()
            }

            ParamType::FieldRef => {
                // Get fields from the validation array
                info!(
                    "   🔍 Extracting fields from content ({} chars) at line {}",
                    content.len(),
                    cursor_line
                );
                let fields = Self::extract_validation_fields(content, cursor_line);
                info!(
                    "   📋 Extracted {} fields from validation array",
                    fields.len()
                );
                if fields.is_empty() {
                    debug!("warn:  No fields found - check if cursor is inside a validation array");
                    // Show first few lines around cursor for debugging
                    let lines: Vec<&str> = content.lines().collect();
                    if cursor_line > 0 && cursor_line < lines.len() {
                        info!(
                            "   📄 Line {}: '{}'",
                            cursor_line - 1,
                            lines.get(cursor_line - 1).unwrap_or(&"")
                        );
                        info!(
                            "   📄 Line {}: '{}'",
                            cursor_line,
                            lines.get(cursor_line).unwrap_or(&"")
                        );
                        info!(
                            "   📄 Line {}: '{}'",
                            cursor_line + 1,
                            lines.get(cursor_line + 1).unwrap_or(&"")
                        );
                    }
                } else {
                    info!("   📋 Fields: {:?}", fields);
                }

                fields
                    .into_iter()
                    .filter(|f| f.to_lowercase().starts_with(&prefix_lower))
                    .map(|field| CompletionItem {
                        label: field.clone(),
                        kind: Some(CompletionItemKind::FIELD),
                        detail: Some("validation field".to_string()),
                        documentation: Some(Documentation::String(
                            "Reference to another field in the validation array".to_string(),
                        )),
                        ..Default::default()
                    })
                    .collect()
            }

            ParamType::Database => {
                // For database rules (exists:, unique:):
                //
                // exists: syntax (https://laravel.com/docs/12.x/validation#rule-exists):
                //   exists:table
                //   exists:table,column
                //   exists:connection.table,column
                //   exists:App\Models\User,column
                //
                // unique: syntax (https://laravel.com/docs/12.x/validation#rule-unique):
                //   unique:table
                //   unique:table,column
                //   unique:table,column,ignore_id
                //   unique:table,column,ignore_id,id_column
                //   unique:connection.table,column
                //
                // Param index meanings:
                //   0 = table (or connection.table, or Model class)
                //   1 = column name
                //   2 = ignore ID (unique only, no autocomplete)
                //   3 = ID column name (unique only)
                let schema_guard = self.database_schema.read().await;
                if let Some(ref provider) = *schema_guard {
                    // Try to get tables (this triggers connection if not cached)
                    let tables = provider.get_tables().await;

                    // If tables is empty and there was a connection error, publish diagnostic
                    if tables.is_empty() {
                        if let Some(error) = provider.get_last_error().await {
                            info!("   ❌ Database connection error: {}", error.message);

                            // Check if we've already shown this diagnostic
                            let mut shown = self.database_diagnostic_shown.write().await;
                            if !*shown {
                                *shown = true;

                                // Publish diagnostic at the validation rule position
                                let diagnostic = Diagnostic {
                                    range: Range {
                                        start: position,
                                        end: Position {
                                            line: position.line,
                                            character: position.character + context.rule_name.len() as u32 + 1, // +1 for colon
                                        },
                                    },
                                    severity: Some(DiagnosticSeverity::INFORMATION),
                                    code: None,
                                    source: Some("laravel".to_string()),
                                    message: format!(
                                        "Database connection failed: {}\n\nConfigure these in .env for exists:/unique: autocomplete:\n- DB_CONNECTION\n- DB_HOST\n- DB_DATABASE\n- DB_USERNAME\n- DB_PASSWORD",
                                        error.message
                                    ),
                                    related_information: None,
                                    tags: None,
                                    code_description: None,
                                    data: None,
                                };

                                self.client
                                    .publish_diagnostics(uri.clone(), vec![diagnostic], None)
                                    .await;
                            }

                            // Return empty - no completions available
                            return Vec::new();
                        }
                    }

                    // Helper to extract table name from first param
                    // Handles: "users", "connection.users", "App\Models\User"
                    let extract_table_name = |first_param: &str| -> String {
                        let trimmed = first_param.trim();

                        // Check for connection.table syntax (period but no backslash before it)
                        if let Some(dot_pos) = trimmed.find('.') {
                            let before_dot = &trimmed[..dot_pos];
                            // If no backslash, it's connection.table
                            if !before_dot.contains('\\') {
                                return trimmed[dot_pos + 1..].to_string();
                            }
                        }

                        // Check for Model class syntax (contains backslash)
                        if trimmed.contains('\\') {
                            // Try to infer table name from model class name
                            // App\Models\User -> users (pluralize + lowercase)
                            if let Some(class_name) = trimmed.rsplit('\\').next() {
                                // Simple pluralization: add 's' and lowercase
                                // This is a simplification - Laravel uses Str::plural()
                                return format!("{}s", class_name.to_lowercase());
                            }
                        }

                        // Plain table name
                        trimmed.to_string()
                    };

                    match context.param_index {
                        0 => {
                            // First param: table name (or connection.table, or Model)
                            // Check if user is typing after a period (connection.█)
                            if context.current_param.contains('.')
                                && !context.current_param.contains('\\')
                            {
                                // User typed "connection." - show tables for that connection
                                // For now, we only support default connection, so show tables
                                let parts: Vec<&str> =
                                    context.current_param.splitn(2, '.').collect();
                                let table_prefix =
                                    parts.get(1).unwrap_or(&"").trim().to_lowercase();

                                info!("   🗄️  Connection.table syntax: prefix='{}'", table_prefix);

                                tables
                                    .into_iter()
                                    .filter(|t| t.to_lowercase().starts_with(&table_prefix))
                                    .map(|table| CompletionItem {
                                        label: table.clone(),
                                        kind: Some(CompletionItemKind::CLASS),
                                        detail: Some("database table".to_string()),
                                        documentation: Some(Documentation::String(format!(
                                            "Table: {}",
                                            table
                                        ))),
                                        ..Default::default()
                                    })
                                    .collect()
                            } else {
                                // Show both connection names and table names
                                let connections = provider.get_connections();
                                info!(
                                    "   🗄️  Database connections: {} found, tables: {} found",
                                    connections.len(),
                                    tables.len()
                                );

                                let mut items: Vec<CompletionItem> = Vec::new();

                                // Add connection names (user types '.' after to trigger table completion)
                                for conn in connections {
                                    if conn.to_lowercase().starts_with(&prefix_lower) {
                                        items.push(CompletionItem {
                                            label: conn.clone(),
                                            kind: Some(CompletionItemKind::FOLDER),
                                            detail: Some("database connection".to_string()),
                                            documentation: Some(Documentation::String(format!(
                                                "Connection: {} (type '.' for tables)",
                                                conn
                                            ))),
                                            ..Default::default()
                                        });
                                    }
                                }

                                // Add table names
                                for table in tables {
                                    if table.to_lowercase().starts_with(&prefix_lower) {
                                        items.push(CompletionItem {
                                            label: table.clone(),
                                            kind: Some(CompletionItemKind::CLASS),
                                            detail: Some("database table".to_string()),
                                            documentation: Some(Documentation::String(format!(
                                                "Table: {}",
                                                table
                                            ))),
                                            ..Default::default()
                                        });
                                    }
                                }

                                items
                            }
                        }
                        1 | 3 => {
                            // Param 1: column name for the table
                            // Param 3 (unique only): ID column name
                            let first_param =
                                context.full_params.split(',').next().unwrap_or("").trim();

                            let table_name = extract_table_name(first_param);
                            info!(
                                "   🗄️  Column completion for table='{}', prefix='{}'",
                                table_name, prefix
                            );

                            if table_name.is_empty() {
                                Vec::new()
                            } else {
                                let columns = provider.get_columns(&table_name).await;
                                info!(
                                    "   🗄️  Columns for '{}': {} found",
                                    table_name,
                                    columns.len()
                                );

                                columns
                                    .into_iter()
                                    .filter(|c| c.to_lowercase().starts_with(&prefix_lower))
                                    .map(|col| CompletionItem {
                                        label: col.clone(),
                                        kind: Some(CompletionItemKind::FIELD),
                                        detail: Some(format!("{}.{}", table_name, col)),
                                        documentation: Some(Documentation::String(format!(
                                            "Column: {}.{}",
                                            table_name, col
                                        ))),
                                        ..Default::default()
                                    })
                                    .collect()
                            }
                        }
                        2 => {
                            // Param 2 (unique only): ignore ID - no autocomplete
                            // This is typically a value like $user->id or a number
                            info!("   🗄️  Param 2 (ignore ID) - no autocomplete");
                            Vec::new()
                        }
                        _ => {
                            // Beyond param 3 - no autocomplete
                            Vec::new()
                        }
                    }
                } else {
                    debug!("warn:  Database schema provider not initialized");

                    // Publish diagnostic at the validation rule position
                    let mut shown = self.database_diagnostic_shown.write().await;
                    if !*shown {
                        *shown = true;

                        let diagnostic = Diagnostic {
                            range: Range {
                                start: position,
                                end: Position {
                                    line: position.line,
                                    character: position.character + context.rule_name.len() as u32 + 1, // +1 for colon
                                },
                            },
                            severity: Some(DiagnosticSeverity::INFORMATION),
                            code: None,
                            source: Some("laravel".to_string()),
                            message: "Database not configured. Set DB_CONNECTION, DB_HOST, DB_DATABASE, DB_USERNAME, DB_PASSWORD in .env for exists:/unique: autocomplete.".to_string(),
                            related_information: None,
                            tags: None,
                            code_description: None,
                            data: None,
                        };

                        self.client
                            .publish_diagnostics(uri.clone(), vec![diagnostic], None)
                            .await;
                    }

                    // Return empty - no completions available
                    Vec::new()
                }
            }

            ParamType::Dimensions => {
                // Get dimension options from Dimensions.php or fallback
                let options: Vec<String> = parser.parse_dimension_options();

                options
                    .into_iter()
                    .filter(|opt: &String| opt.to_lowercase().starts_with(&prefix_lower))
                    .map(|opt: String| {
                        // Dimension options need = suffix (e.g., "min_width=100")
                        let label = format!("{}=", opt);
                        CompletionItem {
                            label: label.clone(),
                            insert_text: Some(label),
                            kind: Some(CompletionItemKind::PROPERTY),
                            detail: Some("dimension constraint".to_string()),
                            documentation: Some(Documentation::String(format!(
                                "Set {} constraint for image dimensions",
                                opt
                            ))),
                            ..Default::default()
                        }
                    })
                    .collect()
            }

            ParamType::MimeExtensions => {
                // Get MIME extensions from Symfony MimeTypes.php or fallback
                let (extensions, _mime_types): (Vec<String>, Vec<String>) =
                    parser.parse_mime_types();

                extensions
                    .into_iter()
                    .filter(|ext: &String| ext.to_lowercase().starts_with(&prefix_lower))
                    .map(|ext: String| CompletionItem {
                        label: ext.clone(),
                        kind: Some(CompletionItemKind::VALUE),
                        detail: Some("file extension".to_string()),
                        documentation: Some(Documentation::String(format!(
                            "Allow files with .{} extension",
                            ext
                        ))),
                        ..Default::default()
                    })
                    .collect()
            }

            ParamType::MimeTypes => {
                // Get MIME types from Symfony MimeTypes.php or fallback
                let (_extensions, mime_types): (Vec<String>, Vec<String>) =
                    parser.parse_mime_types();

                mime_types
                    .into_iter()
                    .filter(|mt: &String| mt.to_lowercase().starts_with(&prefix_lower))
                    .map(|mt: String| CompletionItem {
                        label: mt.clone(),
                        kind: Some(CompletionItemKind::VALUE),
                        detail: Some("MIME type".to_string()),
                        documentation: Some(Documentation::String(format!(
                            "Allow files with {} MIME type",
                            mt
                        ))),
                        ..Default::default()
                    })
                    .collect()
            }

            ParamType::Timezone => {
                // Get timezone identifiers
                let timezones: Vec<String> = LaravelRulesParser::get_timezone_identifiers();

                timezones
                    .into_iter()
                    .filter(|tz: &String| tz.to_lowercase().starts_with(&prefix_lower))
                    .map(|tz: String| CompletionItem {
                        label: tz.clone(),
                        kind: Some(CompletionItemKind::VALUE),
                        detail: Some("timezone".to_string()),
                        documentation: Some(Documentation::String(format!(
                            "Timezone identifier: {}",
                            tz
                        ))),
                        ..Default::default()
                    })
                    .collect()
            }

            ParamType::Custom => {
                // Custom rules don't have predefined options
                Vec::new()
            }
        }
    }

    // ========================================================================
    // Validation Rule Context Detection (for rule name completion)
    // ========================================================================

    /// Detect what type of array context we're in by scanning surrounding lines
    /// Looks for property/method definitions that indicate the array's purpose
    fn detect_array_context(current_line: &str, surrounding_lines: &[&str]) -> ArrayContext {
        // Combine current line with surrounding lines for analysis
        // Order doesn't matter - we just check all lines for context markers
        let all_lines: Vec<&str> = std::iter::once(current_line)
            .chain(surrounding_lines.iter().copied())
            .collect();

        info!(
            "      🔎 detect_array_context: checking {} lines",
            all_lines.len()
        );
        info!(
            "         Current line: '{}'",
            current_line.chars().take(80).collect::<String>()
        );

        // Scan all lines for context markers
        for line in &all_lines {
            if let Some(context) = Self::identify_array_context_from_line(line) {
                info!(
                    "      ✅ Detected {:?} from line: '{}'",
                    context,
                    line.chars().take(60).collect::<String>()
                );
                return context;
            }
        }

        info!(
            "      ⚠️  No specific context found in {} lines, returning ArrayContext::Unknown",
            all_lines.len()
        );
        ArrayContext::Unknown
    }

    /// Check a single line for array context markers and return the context type if found
    fn identify_array_context_from_line(line: &str) -> Option<ArrayContext> {
        let line_lower = line.to_lowercase();
        let line_trimmed = line.trim();

        // === VALIDATION CONTEXTS ===

        // Request/controller validation
        if line_lower.contains("->validate(")
            || line_lower.contains("->validatewithbag(")
            || line_lower.contains("validator::make(")
            || line_lower.contains("validator(")
        {
            return Some(ArrayContext::Validation);
        }

        // $rules property or variable
        if line_lower.contains("$rules") && (line_lower.contains("=") || line_lower.contains("[")) {
            return Some(ArrayContext::Validation);
        }

        // rules() method definition (Form Request, Livewire)
        if line_lower.contains("function rules(") || line_lower.contains("function rules (") {
            return Some(ArrayContext::Validation);
        }

        // Livewire validation attributes
        if line_trimmed.starts_with("#[Rule(") || line_trimmed.starts_with("#[Validate(") {
            return Some(ArrayContext::Validation);
        }

        // === NON-VALIDATION CONTEXTS ===

        // Casts context: $casts property or casts() method
        if line_lower.contains("$casts")
            || line_lower.contains("function casts(")
            || line_lower.contains("function casts (")
        {
            return Some(ArrayContext::Casts);
        }

        // Mass assignment: $fillable, $guarded
        if line_lower.contains("$fillable") || line_lower.contains("$guarded") {
            return Some(ArrayContext::MassAssignment);
        }

        // Visibility: $hidden, $visible, $appends
        if line_lower.contains("$hidden")
            || line_lower.contains("$visible")
            || line_lower.contains("$appends")
        {
            return Some(ArrayContext::Visibility);
        }

        // Relationships: $with, $withCount
        if (line_lower.contains("$with") && !line_lower.contains("$without"))
            || line_lower.contains("$withcount")
        {
            return Some(ArrayContext::Relationships);
        }

        None
    }

    /// Check if cursor is inside a validation rule context
    /// Returns the partial rule text typed so far (for filtering completions)
    ///
    /// Detects validation contexts in Laravel and Livewire:
    /// - $request->validate([...])
    /// - Validator::make($data, [...])
    /// - validator($data, [...])
    /// - $rules = [...] or protected $rules = [...]
    /// - function rules() { return [...]; }
    /// - #[Rule('...')] or #[Validate('...')]
    /// - $this->validate([...])
    ///
    /// Explicitly excludes non-validation contexts:
    /// - $casts / casts() - Eloquent attribute casting
    /// - $fillable / $guarded - Mass assignment
    /// - $hidden / $visible / $appends - Model serialization
    /// - $with / $withCount - Eager loading
    ///
    /// `surrounding_lines` provides context from previous lines (for multi-line arrays)
    fn get_validation_rule_context(
        line_text: &str,
        character: u32,
        surrounding_lines: &[&str],
        cached_rules: &[String],
    ) -> Option<String> {
        let cursor = character as usize;
        if cursor > line_text.len() {
            return None;
        }

        let before_cursor = &line_text[..cursor];

        // First, extract the partial rule text if we're in a string
        let partial_rule = Self::extract_partial_validation_rule(before_cursor)?;

        // Use context-aware detection to determine what type of array we're in
        // This prevents triggering validation completions in $casts and other non-validation arrays
        let context = Self::detect_array_context(line_text, surrounding_lines);

        match context {
            // Explicit validation context - allow completions
            ArrayContext::Validation => Some(partial_rule),

            // Explicit non-validation contexts - no completions
            ArrayContext::Casts
            | ArrayContext::MassAssignment
            | ArrayContext::Visibility
            | ArrayContext::Relationships => None,

            // Unknown context - fall back to pattern matching for validation indicators
            ArrayContext::Unknown => {
                // Check if line or surrounding lines have validation indicators
                if Self::is_validation_context(line_text, cached_rules) {
                    return Some(partial_rule);
                }

                for surrounding in surrounding_lines {
                    if Self::is_validation_context(surrounding, cached_rules) {
                        return Some(partial_rule);
                    }
                }

                None
            }
        }
    }

    /// Extract the partial validation rule text from before the cursor
    /// Returns None if cursor is not inside a quoted string that's a VALUE (not a key)
    fn extract_partial_validation_rule(before_cursor: &str) -> Option<String> {
        // IMPORTANT: We need to distinguish between:
        // - 'field_name' => ... (cursor in KEY position - NO completion)
        // - 'field_name' => 'rule█' (cursor in VALUE position - YES completion)
        //
        // The key insight: if we're in a VALUE, there must be a `=>` before our current string

        // Check for pipe-delimited format: '...|█' or "...|█"
        // This is always a VALUE position (inside a rule string)
        if let Some(last_pipe) = before_cursor.rfind('|') {
            let before_pipe = &before_cursor[..last_pipe];
            let single_quotes = before_pipe.matches('\'').count();
            let double_quotes = before_pipe.matches('"').count();

            // If odd number of quotes, we're inside a string
            if single_quotes % 2 == 1 || double_quotes % 2 == 1 {
                let after_pipe = &before_cursor[last_pipe + 1..];
                if !after_pipe.contains('\'') && !after_pipe.contains('"') {
                    return Some(after_pipe.to_string());
                }
            }
        }

        // For non-pipe patterns, we need to ensure we're in VALUE position
        // VALUE patterns (after =>)
        let value_patterns: &[(&str, char)] = &[
            // Arrow patterns (for 'field' => '█') - these are definitely VALUES
            ("=> '", '\''),
            ("=> \"", '"'),
            ("=>'", '\''),
            ("=>\"", '"'),
            // Attribute patterns for #[Rule('█')] and #[Validate('█')]
            ("Rule('", '\''),
            ("Rule(\"", '"'),
            ("Validate('", '\''),
            ("Validate(\"", '"'),
        ];

        for (pattern, quote_char) in value_patterns {
            if let Some(pos) = before_cursor.rfind(pattern) {
                let start_pos = pos + pattern.len();
                let after_pattern = &before_cursor[start_pos..];

                // Make sure there's no closing quote
                if !after_pattern.contains(*quote_char) {
                    return Some(after_pattern.to_string());
                }
            }
        }

        // For array element patterns like ['rule'] or [, 'rule'], we need to check
        // if there's NO `=>` after the pattern - that means it's NOT a key
        let array_patterns: &[(&str, char)] = &[
            (", '", '\''),
            (", \"", '"'),
            (",'", '\''),
            (",\"", '"'),
            ("['", '\''),
            ("[\"", '"'),
        ];

        for (pattern, quote_char) in array_patterns {
            if let Some(pos) = before_cursor.rfind(pattern) {
                let start_pos = pos + pattern.len();
                let after_pattern = &before_cursor[start_pos..];

                // Make sure there's no closing quote
                if after_pattern.contains(*quote_char) {
                    continue;
                }

                // Check if there's a `=>` AFTER this pattern start but BEFORE cursor
                // If there IS a `=>`, this quoted string is a KEY, not a value
                // e.g., ['field_name' => ...] - 'field_name' has => after it
                let text_after_pattern_start = &before_cursor[pos..];
                if text_after_pattern_start.contains("=>") {
                    // This is a KEY position, not a value - skip
                    continue;
                }

                // No => found, so this could be a rule in array format like ['required', 'email']
                return Some(after_pattern.to_string());
            }
        }

        None
    }

    /// Check if cursor is inside a cast type context (VALUE position in $casts or casts())
    /// Returns the partial cast type text typed so far (for filtering completions)
    ///
    /// Detects cast contexts in Eloquent models:
    /// - protected $casts = ['field' => '█']
    /// - protected function casts(): array { return ['field' => '█']; }
    ///
    /// `surrounding_lines` provides context from previous lines (for multi-line arrays)
    fn get_cast_type_context(
        line_text: &str,
        character: u32,
        surrounding_lines: &[&str],
    ) -> Option<String> {
        let cursor = character as usize;
        if cursor > line_text.len() {
            return None;
        }

        let before_cursor = &line_text[..cursor];

        // First, check if we're in a cast context at all
        let context = Self::detect_array_context(line_text, surrounding_lines);

        if context != ArrayContext::Casts {
            return None;
        }

        // Extract the partial cast type if we're in VALUE position (after =>)
        Self::extract_partial_cast_type(before_cursor)
    }

    /// Extract the partial cast type text from before the cursor
    /// Returns None if cursor is not inside a quoted string in VALUE position
    fn extract_partial_cast_type(before_cursor: &str) -> Option<String> {
        // Cast types are always values (right side of =>)
        // Pattern: 'field_name' => '█' or 'field_name' => "█"

        let value_patterns: &[(&str, char)] = &[
            // Arrow patterns (for 'field' => '█')
            ("=> '", '\''),
            ("=> \"", '"'),
            ("=>'", '\''),
            ("=>\"", '"'),
        ];

        for (pattern, quote_char) in value_patterns {
            if let Some(pos) = before_cursor.rfind(pattern) {
                let start_pos = pos + pattern.len();
                let after_pattern = &before_cursor[start_pos..];

                // Make sure there's no closing quote
                if !after_pattern.contains(*quote_char) {
                    return Some(after_pattern.to_string());
                }
            }
        }

        None
    }

    /// Check if the line appears to be in a validation context
    /// This verifies we're not just in any random string
    /// `cached_rules` - optional slice of rule names (with colon suffix) from Laravel framework
    fn is_validation_context(line_text: &str, cached_rules: &[String]) -> bool {
        let line_lower = line_text.to_lowercase();
        let line_trimmed = line_text.trim();

        // 1. Request validation: $request->validate(, $this->validate(, ->validateWithBag(
        if line_lower.contains("->validate(") || line_lower.contains("->validatewithbag(") {
            return true;
        }

        // 2. Validator facade/helper: Validator::make(, validator(
        if line_lower.contains("validator::make(") || line_lower.contains("validator(") {
            return true;
        }

        // 3. Rules variable: $rules = [...] or $rules[
        if line_lower.contains("$rules") && (line_lower.contains("=") || line_lower.contains("[")) {
            return true;
        }

        // 4. Rules property: protected $rules, public $rules, private $rules
        //    Also: protected array $rules
        if (line_lower.contains("protected")
            || line_lower.contains("public")
            || line_lower.contains("private"))
            && line_lower.contains("$rules")
        {
            return true;
        }

        // 5. Function rules() definition (Form Request pattern)
        //    Matches: function rules(), public function rules(): array
        if line_lower.contains("function rules(") || line_lower.contains("function rules (") {
            return true;
        }

        // 6. Return statement with array (likely inside rules() method)
        //    Pattern: return [
        if line_trimmed.starts_with("return") && line_lower.contains("[") {
            return true;
        }

        // 7. Inside array with => that contains actual validation rule names
        //    Only trigger if we find actual validation rules (from cached Laravel rules)
        //    Avoids false positives with cast types like 'string', 'integer', 'boolean', etc.
        if line_lower.contains("=>") {
            // Check if this looks like a validation rule using cached rules from Laravel framework
            for rule in cached_rules {
                if line_lower.contains(rule) {
                    return true;
                }
            }
            // Note: We intentionally do NOT trigger on generic patterns like `=> '`
            // because that would cause false positives in $casts and other arrays.
            // The smart array context detection should handle most cases.
        }

        // 8. Livewire #[Rule(...)] or #[Validate(...)] attributes
        if line_trimmed.starts_with("#[Rule(") || line_trimmed.starts_with("#[Validate(") {
            return true;
        }

        // 9. Inside what looks like a rules array (has pipe-delimited rules)
        if line_text.contains("|") {
            let validation_rules = [
                "required",
                "nullable",
                "string",
                "integer",
                "email",
                "max",
                "min",
                "unique",
                "exists",
                "in",
                "between",
                "confirmed",
                "accepted",
            ];
            for rule in validation_rules {
                if line_lower.contains(rule) {
                    return true;
                }
            }
        }

        // 10. Inside a validation messages or attributes method (related context)
        if line_lower.contains("function messages(") || line_lower.contains("function attributes(")
        {
            return true;
        }

        false
    }

    /// Get all config keys from config/*.php files for autocomplete
    async fn get_all_config_keys(&self) -> Vec<ConfigKeyCompletion> {
        let root = match self.root_path.read().await.clone() {
            Some(r) => r,
            None => return Vec::new(),
        };

        let config_dir = root.join("config");
        if !config_dir.exists() {
            return Vec::new();
        }

        // Get env vars for resolving env() references
        let env_vars: std::collections::HashMap<String, String> =
            match self.salsa.get_all_parsed_env_vars().await {
                Ok(vars) => vars
                    .into_iter()
                    .filter(|v| !v.is_commented)
                    .map(|v| (v.name, v.value))
                    .collect(),
                Err(_) => std::collections::HashMap::new(),
            };

        let mut completions = Vec::new();

        // Read all PHP files in config directory
        if let Ok(entries) = std::fs::read_dir(&config_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "php") {
                    if let Some(file_name) = path.file_stem().and_then(|s| s.to_str()) {
                        let base_key = file_name.to_string();
                        let source = format!("config/{}.php", file_name);

                        if let Ok(content) = std::fs::read_to_string(&path) {
                            // Parse the config file and extract keys
                            let keys = Self::parse_config_keys(&content, &base_key, &env_vars);
                            for (key, value) in keys {
                                completions.push(ConfigKeyCompletion {
                                    key,
                                    value,
                                    source: source.clone(),
                                });
                            }
                        }
                    }
                }
            }
        }

        // Sort by key for consistent ordering
        completions.sort_by(|a, b| a.key.cmp(&b.key));
        completions
    }

    /// Get all view names from resources/views for autocomplete
    async fn get_all_view_names(&self) -> Vec<ViewNameCompletion> {
        let root = match self.root_path.read().await.clone() {
            Some(r) => r,
            None => return Vec::new(),
        };

        // Get view paths from cached config
        let view_paths = match self.cached_config.read().await.as_ref() {
            Some(config) => config.view_paths.clone(),
            None => {
                // Default to resources/views if no config
                vec![root.join("resources/views")]
            }
        };

        let mut completions = Vec::new();

        for view_path in view_paths {
            if !view_path.exists() {
                continue;
            }

            // Walk the directory recursively
            for entry in walkdir::WalkDir::new(&view_path)
                .follow_links(true)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_type().is_file() && e.path().extension().is_some_and(|ext| ext == "php")
                })
            {
                let path = entry.into_path();

                // Convert file path to view name
                if let Ok(relative) = path.strip_prefix(&view_path) {
                    let relative_str = relative.to_string_lossy();

                    // Remove .blade.php or .php extension
                    let view_name = if relative_str.ends_with(".blade.php") {
                        relative_str.trim_end_matches(".blade.php")
                    } else if relative_str.ends_with(".php") {
                        relative_str.trim_end_matches(".php")
                    } else {
                        continue;
                    };

                    // Convert path separators to dots
                    let view_name = view_name.replace(['/', '\\'], ".");

                    // Get relative path from project root for display
                    let display_path = path
                        .strip_prefix(&root)
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|_| path.to_string_lossy().to_string());

                    completions.push(ViewNameCompletion {
                        name: view_name.to_string(),
                        path: display_path,
                    });
                }
            }
        }

        // Add package views from view_namespaces
        if let Some(config) = self.cached_config.read().await.as_ref() {
            for (namespace, package_path) in &config.view_namespaces {
                if !package_path.exists() {
                    continue;
                }

                for entry in walkdir::WalkDir::new(package_path)
                    .follow_links(true)
                    .into_iter()
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        e.file_type().is_file()
                            && e.path().extension().is_some_and(|ext| ext == "php")
                    })
                {
                    let path = entry.into_path();

                    if let Ok(relative) = path.strip_prefix(package_path) {
                        let relative_str = relative.to_string_lossy();

                        let view_name = if relative_str.ends_with(".blade.php") {
                            relative_str.trim_end_matches(".blade.php")
                        } else if relative_str.ends_with(".php") {
                            relative_str.trim_end_matches(".php")
                        } else {
                            continue;
                        };

                        // Convert path separators to dots and prefix with namespace::
                        let view_name =
                            format!("{}::{}", namespace, view_name.replace(['/', '\\'], "."));

                        let display_path = path.to_string_lossy().to_string();

                        completions.push(ViewNameCompletion {
                            name: view_name,
                            path: display_path,
                        });
                    }
                }
            }
        }

        // Sort by name for consistent ordering
        completions.sort_by(|a, b| a.name.cmp(&b.name));
        completions
    }

    /// Get all Blade component names from component directories for autocomplete
    async fn get_all_blade_components(&self) -> Vec<BladeComponentCompletion> {
        let root = match self.root_path.read().await.clone() {
            Some(r) => r,
            None => return Vec::new(),
        };

        // Get component paths from cached config
        let component_paths = match self.cached_config.read().await.as_ref() {
            Some(config) => config.component_paths.clone(),
            None => {
                // Default to resources/views/components if no config
                vec![(String::new(), root.join("resources/views/components"))]
            }
        };

        let mut completions = Vec::new();

        for (namespace, component_path) in component_paths {
            if !component_path.exists() {
                continue;
            }

            // Walk the directory recursively
            for entry in walkdir::WalkDir::new(&component_path)
                .follow_links(true)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_type().is_file() && e.path().extension().is_some_and(|ext| ext == "php")
                })
            {
                let path = entry.into_path();

                // Convert file path to component name
                if let Ok(relative) = path.strip_prefix(&component_path) {
                    let relative_str = relative.to_string_lossy();

                    // Remove .blade.php or .php extension
                    let component_name = if relative_str.ends_with(".blade.php") {
                        relative_str.trim_end_matches(".blade.php")
                    } else if relative_str.ends_with(".php") {
                        relative_str.trim_end_matches(".php")
                    } else {
                        continue;
                    };

                    // Convert path separators to dots for nested components
                    let component_name = component_name.replace(['/', '\\'], ".");

                    // Convert to kebab-case (Laravel convention for Blade components)
                    // PascalCase files become kebab-case: "Button" -> "button", "AlertDialog" -> "alert-dialog"
                    let component_name = Self::to_kebab_case(&component_name);

                    // Add namespace prefix if present
                    let full_name = if namespace.is_empty() {
                        component_name
                    } else {
                        format!("{}::{}", namespace, component_name)
                    };

                    // Get relative path from project root for display
                    let display_path = path
                        .strip_prefix(&root)
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|_| path.to_string_lossy().to_string());

                    completions.push(BladeComponentCompletion {
                        name: full_name,
                        path: display_path,
                    });
                }
            }
        }

        // Add package components from view_namespaces
        // Package anonymous components live in {package_view_path}/components/
        if let Some(config) = self.cached_config.read().await.as_ref() {
            for (namespace, package_path) in &config.view_namespaces {
                let components_path = package_path.join("components");
                if !components_path.exists() {
                    continue;
                }

                for entry in walkdir::WalkDir::new(&components_path)
                    .follow_links(true)
                    .into_iter()
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        e.file_type().is_file()
                            && e.path().extension().is_some_and(|ext| ext == "php")
                    })
                {
                    let path = entry.into_path();

                    if let Ok(relative) = path.strip_prefix(&components_path) {
                        let relative_str = relative.to_string_lossy();

                        // Remove .blade.php or .php extension
                        let component_name = if relative_str.ends_with(".blade.php") {
                            relative_str.trim_end_matches(".blade.php")
                        } else if relative_str.ends_with(".php") {
                            relative_str.trim_end_matches(".php")
                        } else {
                            continue;
                        };

                        // Convert path separators to dots for nested components
                        let component_name = component_name.replace(['/', '\\'], ".");

                        // Convert to kebab-case
                        let component_name = Self::to_kebab_case(&component_name);

                        // Package components use namespace::component format
                        let full_name = format!("{}::{}", namespace, component_name);

                        // For display, show relative to package path or absolute
                        let display_path = path.to_string_lossy().to_string();

                        completions.push(BladeComponentCompletion {
                            name: full_name,
                            path: display_path,
                        });
                    }
                }
            }
        }

        // Sort by name for consistent ordering
        completions.sort_by(|a, b| a.name.cmp(&b.name));
        completions
    }

    /// Get all Livewire component names from app/Livewire directory for autocomplete
    ///
    /// Returns a list of Livewire component names in kebab-case (as used in Blade templates)
    /// along with their file paths.
    async fn get_all_livewire_components(&self) -> Vec<LivewireComponentCompletion> {
        let root = match self.root_path.read().await.clone() {
            Some(r) => r,
            None => return Vec::new(),
        };

        // Get Livewire path from cached config
        let livewire_path = match self.cached_config.read().await.as_ref() {
            Some(config) => match &config.livewire_path {
                Some(path) => root.join(path),
                None => return Vec::new(), // Livewire not configured
            },
            None => {
                // Default to app/Livewire if no config
                let v3_path = root.join("app/Livewire");
                let v2_path = root.join("app/Http/Livewire");
                if v3_path.exists() {
                    v3_path
                } else if v2_path.exists() {
                    v2_path
                } else {
                    return Vec::new();
                }
            }
        };

        if !livewire_path.exists() {
            return Vec::new();
        }

        let mut completions = Vec::new();

        // Walk the Livewire directory recursively
        for entry in walkdir::WalkDir::new(&livewire_path)
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_type().is_file() && e.path().extension().is_some_and(|ext| ext == "php")
            })
        {
            let path = entry.into_path();

            // Convert file path to component name
            if let Ok(relative) = path.strip_prefix(&livewire_path) {
                let relative_str = relative.to_string_lossy();

                // Remove .php extension
                let component_name = if relative_str.ends_with(".php") {
                    relative_str.trim_end_matches(".php")
                } else {
                    continue;
                };

                // Convert path separators to dots for nested components
                // e.g., "Admin/Dashboard.php" -> "admin.dashboard"
                let component_name = component_name.replace(['/', '\\'], ".");

                // Convert PascalCase to kebab-case
                // e.g., "UserProfile" -> "user-profile", "Admin.Dashboard" -> "admin.dashboard"
                let component_name = Self::to_kebab_case(&component_name);

                // Get relative path from project root for display
                let display_path = path
                    .strip_prefix(&root)
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| path.to_string_lossy().to_string());

                completions.push(LivewireComponentCompletion {
                    name: component_name,
                    path: display_path,
                });
            }
        }

        // Sort by name for consistent ordering
        completions.sort_by(|a, b| a.name.cmp(&b.name));
        completions
    }

    /// Get all files in a directory for autocomplete (used by asset, vite, path helpers)
    ///
    /// Returns relative paths from the base directory.
    /// Optionally filters by file extensions.
    async fn get_directory_files(
        &self,
        base_dir: &std::path::Path,
        extensions: Option<&[&str]>,
    ) -> Vec<FilePathCompletion> {
        if !base_dir.exists() {
            return Vec::new();
        }

        let mut completions = Vec::new();

        for entry in walkdir::WalkDir::new(base_dir)
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let path = entry.path();

            // Filter by extension if specified
            if let Some(exts) = extensions {
                let has_valid_ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| exts.iter().any(|ext| e.eq_ignore_ascii_case(ext)))
                    .unwrap_or(false);

                if !has_valid_ext {
                    continue;
                }
            }

            // Get relative path from base directory
            if let Ok(relative) = path.strip_prefix(base_dir) {
                let relative_str = relative.to_string_lossy().to_string();
                // Normalize path separators to forward slashes
                let normalized = relative_str.replace('\\', "/");
                completions.push(FilePathCompletion { path: normalized });
            }
        }

        // Sort by path for consistent ordering
        completions.sort_by(|a, b| a.path.cmp(&b.path));
        completions
    }

    /// Get the base directory for a path helper
    fn get_path_helper_base_dir(&self, helper: &str, root: &std::path::Path) -> std::path::PathBuf {
        match helper {
            "app_path" => root.join("app"),
            "base_path" => root.to_path_buf(),
            "config_path" => root.join("config"),
            "database_path" => root.join("database"),
            "lang_path" => root.join("lang"),
            "public_path" => root.join("public"),
            "resource_path" => root.join("resources"),
            "storage_path" => root.join("storage"),
            _ => root.to_path_buf(),
        }
    }

    /// Convert a string to kebab-case
    /// "Button" -> "button"
    /// "AlertDialog" -> "alert-dialog"
    /// "forms.Input" -> "forms.input"
    fn to_kebab_case(s: &str) -> String {
        let mut result = String::new();
        for (i, c) in s.chars().enumerate() {
            if c == '.' {
                // Preserve dots for nested components
                result.push('.');
            } else if c.is_uppercase() {
                if i > 0 && !result.ends_with('.') && !result.ends_with('-') {
                    result.push('-');
                }
                result.push(c.to_lowercase().next().unwrap());
            } else {
                result.push(c);
            }
        }
        result
    }

    /// Heuristic check: does `content` define a class that extends Laravel's
    /// `Model`? Used by `get_class_properties` to decide whether to run the
    /// Eloquent-rich flow (DB schema, casts, relationships) or fall back to
    /// the generic public-property scan that works for any PHP class.
    fn content_extends_model(content: &str) -> bool {
        let pattern = r"class\s+\w+\s+extends\s+\\?(?:Illuminate\\Database\\Eloquent\\)?Model\b";
        regex::Regex::new(pattern)
            .ok()
            .map(|re| re.is_match(content))
            .unwrap_or(false)
    }

    /// Locate any PHP class file under `app/` (or `src/`) and return its
    /// `public Type $foo` declarations. Used as the generic fallback when a
    /// class isn't an Eloquent model — Livewire Forms, Livewire components,
    /// DTOs, value objects all flow through here.
    fn extract_generic_class_properties(
        root: &std::path::Path,
        class_name: &str,
    ) -> Vec<ModelPropertyCompletion> {
        let Some(class_path) = laravel_lsp::class_locator::find_php_class_file(class_name, root)
        else {
            return Vec::new();
        };
        let Ok(content) = std::fs::read_to_string(&class_path) else {
            return Vec::new();
        };

        let mut props: Vec<ModelPropertyCompletion> =
            laravel_lsp::php_class::extract_class_properties(&content)
                .into_iter()
                .map(|(name, php_type)| ModelPropertyCompletion {
                    name,
                    php_type,
                    source: "class".to_string(),
                })
                .collect();
        props.sort_by(|a, b| a.name.cmp(&b.name));
        props
    }

    /// Find the model file path for a given class name
    /// Searches in app/Models/ directory
    fn find_model_file(
        &self,
        root: &std::path::Path,
        class_name: &str,
    ) -> Option<std::path::PathBuf> {
        // Try app/Models/ClassName.php first (Laravel 8+)
        let model_path = root
            .join("app")
            .join("Models")
            .join(format!("{}.php", class_name));
        if model_path.exists() {
            return Some(model_path);
        }

        // Try app/ClassName.php (older Laravel)
        let old_model_path = root.join("app").join(format!("{}.php", class_name));
        if old_model_path.exists() {
            return Some(old_model_path);
        }

        None
    }

    /// Get all properties for a class. Returns Eloquent-rich data (DB columns,
    /// casts, accessors, relationships) when the class lives in a standard
    /// model location AND extends `Model`; otherwise falls back to a generic
    /// `public Type $foo` scan that works for Livewire components, Livewire
    /// Forms, DTOs, value objects — any class with public properties.
    async fn get_class_properties(&self, class_name: &str) -> Vec<ModelPropertyCompletion> {
        use laravel_lsp::model_analyzer::{
            map_cast_to_php_type, relationship_to_php_type, ModelMetadata,
        };

        // Synthetic Blade types — short-circuit before any filesystem lookup so they
        // never collide with user-defined models of the same name.
        if class_name == "Loop" {
            return Self::loop_var_properties();
        }

        let root = match self.root_path.read().await.clone() {
            Some(r) => r,
            None => return Vec::new(),
        };

        // Try the model paths first (`app/Models/X.php`, `app/X.php`). When the
        // file is there AND it actually extends `Model`, run the full Eloquent
        // resolution. Anything else (Livewire Forms, DTOs, etc.) falls through
        // to the generic public-property scan below.
        let model_path = self.find_model_file(&root, class_name);
        let appears_to_be_model = model_path
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|content| Self::content_extends_model(&content))
            .unwrap_or(false);

        if !appears_to_be_model {
            return Self::extract_generic_class_properties(&root, class_name);
        }

        let model_path = match model_path {
            Some(p) => p,
            None => return Vec::new(),
        };

        // Read and parse the model file
        let content = match std::fs::read_to_string(&model_path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };

        let metadata = ModelMetadata::from_content(&content);

        // Determine the table name (either from $table property or by convention)
        let table_name = metadata.table_name.clone().unwrap_or_else(|| {
            // Laravel convention: Model name -> plural snake_case
            // User -> users, BlogPost -> blog_posts
            let snake = ModelMetadata::pascal_to_snake(class_name);
            // Simple pluralization (add 's')
            format!("{}s", snake)
        });

        let mut properties: Vec<ModelPropertyCompletion> = Vec::new();
        let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();

        // 1. Get database columns with types (lowest priority - can be overridden)
        if let Some(ref db) = *self.database_schema.read().await {
            let columns: Vec<(String, String)> = db.get_columns_with_types(&table_name).await;
            for (col_name, php_type) in columns {
                if seen_names.insert(col_name.clone()) {
                    properties.push(ModelPropertyCompletion {
                        name: col_name,
                        php_type,
                        source: "database".to_string(),
                    });
                }
            }
        }

        // 2. Apply casts (override database types)
        for (prop_name, cast_type) in &metadata.casts {
            let php_type = map_cast_to_php_type(cast_type);
            if let Some(existing) = properties.iter_mut().find(|p| &p.name == prop_name) {
                existing.php_type = php_type;
                existing.source = "cast".to_string();
            } else if seen_names.insert(prop_name.clone()) {
                properties.push(ModelPropertyCompletion {
                    name: prop_name.clone(),
                    php_type,
                    source: "cast".to_string(),
                });
            }
        }

        // 3. Add accessors (computed properties)
        for accessor in &metadata.accessors {
            if seen_names.insert(accessor.property_name.clone()) {
                let php_type = accessor
                    .return_type
                    .clone()
                    .unwrap_or_else(|| "mixed".to_string());
                properties.push(ModelPropertyCompletion {
                    name: accessor.property_name.clone(),
                    php_type,
                    source: "accessor".to_string(),
                });
            }
        }

        // 4. Add relationships
        for rel in &metadata.relationships {
            if seen_names.insert(rel.method_name.clone()) {
                let php_type =
                    relationship_to_php_type(&rel.relationship_type, rel.related_model.as_deref());
                properties.push(ModelPropertyCompletion {
                    name: rel.method_name.clone(),
                    php_type,
                    source: "relationship".to_string(),
                });
            }
        }

        // Sort by name for consistent ordering
        properties.sort_by(|a, b| a.name.cmp(&b.name));
        properties
    }

    /// Get all route names from routes/*.php files for autocomplete
    async fn get_all_route_names(&self) -> Vec<RouteNameCompletion> {
        let root = match self.root_path.read().await.clone() {
            Some(r) => r,
            None => return Vec::new(),
        };

        let routes_dir = root.join("routes");
        if !routes_dir.exists() {
            return Vec::new();
        }

        let mut completions = Vec::new();
        let route_files = ["web.php", "api.php", "channels.php", "console.php"];

        // Regex to match ->name('route.name') or ->name("route.name")
        let name_pattern = regex::Regex::new(r#"->name\s*\(\s*['"]([^'"]+)['"]\s*\)"#).unwrap();

        // Regex to match Route::resource('name', Controller::class) with optional modifiers
        // Captures: 1=resource name, 2=rest of the chain (for only/except parsing)
        let resource_pattern =
            regex::Regex::new(r#"Route::resource\s*\(\s*['"]([^'"]+)['"]\s*,[^)]+\)([^;]*)"#)
                .unwrap();

        // Regex to match Route::apiResource('name', Controller::class) with optional modifiers
        let api_resource_pattern =
            regex::Regex::new(r#"Route::apiResource\s*\(\s*['"]([^'"]+)['"]\s*,[^)]+\)([^;]*)"#)
                .unwrap();

        // Regex to extract ->only([...]) actions
        let only_pattern = regex::Regex::new(r#"->only\s*\(\s*\[([^\]]*)\]"#).unwrap();

        // Regex to extract ->except([...]) actions
        let except_pattern = regex::Regex::new(r#"->except\s*\(\s*\[([^\]]*)\]"#).unwrap();

        // Standard resource actions
        let resource_actions = [
            "index", "create", "store", "show", "edit", "update", "destroy",
        ];
        // API resource actions (no create/edit - those are for forms)
        let api_resource_actions = ["index", "store", "show", "update", "destroy"];

        for file_name in route_files {
            let route_file = routes_dir.join(file_name);
            if route_file.exists() {
                if let Ok(content) = std::fs::read_to_string(&route_file) {
                    let source = format!("routes/{}", file_name);

                    // Find all ->name('...') patterns
                    for caps in name_pattern.captures_iter(&content) {
                        if let Some(name_match) = caps.get(1) {
                            completions.push(RouteNameCompletion {
                                name: name_match.as_str().to_string(),
                                source: source.clone(),
                            });
                        }
                    }

                    // Find all Route::resource() patterns
                    for caps in resource_pattern.captures_iter(&content) {
                        if let Some(resource_name) = caps.get(1) {
                            let chain = caps.get(2).map(|m| m.as_str()).unwrap_or("");
                            let actions = Self::get_resource_actions(
                                chain,
                                &resource_actions,
                                &only_pattern,
                                &except_pattern,
                            );

                            for action in actions {
                                completions.push(RouteNameCompletion {
                                    name: format!("{}.{}", resource_name.as_str(), action),
                                    source: source.clone(),
                                });
                            }
                        }
                    }

                    // Find all Route::apiResource() patterns
                    for caps in api_resource_pattern.captures_iter(&content) {
                        if let Some(resource_name) = caps.get(1) {
                            let chain = caps.get(2).map(|m| m.as_str()).unwrap_or("");
                            let actions = Self::get_resource_actions(
                                chain,
                                &api_resource_actions,
                                &only_pattern,
                                &except_pattern,
                            );

                            for action in actions {
                                completions.push(RouteNameCompletion {
                                    name: format!("{}.{}", resource_name.as_str(), action),
                                    source: source.clone(),
                                });
                            }
                        }
                    }
                }
            }
        }

        // Sort by name for consistent ordering
        completions.sort_by(|a, b| a.name.cmp(&b.name));

        // Remove duplicates (same route name from different files - keep first occurrence)
        completions.dedup_by(|a, b| a.name == b.name);

        completions
    }

    /// Parse ->only() and ->except() modifiers to determine which resource actions to include
    fn get_resource_actions<'a>(
        chain: &str,
        all_actions: &'a [&'a str],
        only_pattern: &regex::Regex,
        except_pattern: &regex::Regex,
    ) -> Vec<&'a str> {
        // Check for ->only([...])
        if let Some(only_caps) = only_pattern.captures(chain) {
            if let Some(only_list) = only_caps.get(1) {
                let only_actions: Vec<&str> = only_list
                    .as_str()
                    .split(',')
                    .map(|s| s.trim().trim_matches('\'').trim_matches('"'))
                    .filter(|s| !s.is_empty())
                    .collect();

                return all_actions
                    .iter()
                    .filter(|a| only_actions.contains(a))
                    .copied()
                    .collect();
            }
        }

        // Check for ->except([...])
        if let Some(except_caps) = except_pattern.captures(chain) {
            if let Some(except_list) = except_caps.get(1) {
                let except_actions: Vec<&str> = except_list
                    .as_str()
                    .split(',')
                    .map(|s| s.trim().trim_matches('\'').trim_matches('"'))
                    .filter(|s| !s.is_empty())
                    .collect();

                return all_actions
                    .iter()
                    .filter(|a| !except_actions.contains(a))
                    .copied()
                    .collect();
            }
        }

        // No modifiers - return all actions
        all_actions.to_vec()
    }

    /// Get all translation keys from lang/*.php files for autocomplete
    async fn get_all_translation_keys(&self) -> Vec<TranslationKeyCompletion> {
        let root = match self.root_path.read().await.clone() {
            Some(r) => r,
            None => return Vec::new(),
        };

        // Laravel 9+ uses lang/, older versions use resources/lang/
        let lang_dirs = [root.join("lang"), root.join("resources").join("lang")];

        let lang_dir = lang_dirs.iter().find(|d| d.exists());
        let lang_dir = match lang_dir {
            Some(d) => d,
            None => return Vec::new(),
        };

        let mut completions = Vec::new();

        // Find the default locale directory (usually 'en')
        // We'll use the first locale we find
        if let Ok(entries) = std::fs::read_dir(lang_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let locale = path.file_name().and_then(|n| n.to_str()).unwrap_or("en");

                    // Read all PHP files in this locale directory
                    if let Ok(files) = std::fs::read_dir(&path) {
                        for file_entry in files.flatten() {
                            let file_path = file_entry.path();
                            if file_path.extension().is_some_and(|e| e == "php") {
                                if let Some(file_name) =
                                    file_path.file_stem().and_then(|s| s.to_str())
                                {
                                    let base_key = file_name.to_string();
                                    let source = format!("lang/{}/{}.php", locale, file_name);

                                    if let Ok(content) = std::fs::read_to_string(&file_path) {
                                        let keys =
                                            Self::parse_translation_keys(&content, &base_key);
                                        for (key, value) in keys {
                                            completions.push(TranslationKeyCompletion {
                                                key,
                                                value,
                                                source: source.clone(),
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Only use the first locale directory found
                    break;
                }
            }
        }

        // Sort by key for consistent ordering
        completions.sort_by(|a, b| a.key.cmp(&b.key));

        // Remove duplicates
        completions.dedup_by(|a, b| a.key == b.key);

        completions
    }

    /// Parse a PHP translation file to extract all keys and values
    /// Returns a list of (key, value) tuples with dot-notation keys
    fn parse_translation_keys(content: &str, base_key: &str) -> Vec<(String, String)> {
        let mut results = Vec::new();

        // Simple regex-based parsing for Laravel translation files
        // This handles: 'key' => 'value', or "key" => "value"
        let key_pattern = regex::Regex::new(r#"['"]([a-zA-Z_][a-zA-Z0-9_]*)['"][\s]*=>"#).unwrap();

        // Track nesting depth and current key path
        let mut key_stack: Vec<String> = vec![base_key.to_string()];
        let mut in_array_depth = 0;
        let mut pending_key: Option<String> = None;

        for line in content.lines() {
            let trimmed = line.trim();

            // Skip comments and empty lines
            if trimmed.is_empty()
                || trimmed.starts_with("//")
                || trimmed.starts_with("/*")
                || trimmed.starts_with("*")
            {
                continue;
            }

            // Handle array opening
            if trimmed.contains("[") && !trimmed.contains("=>") {
                in_array_depth += 1;
                if let Some(key) = pending_key.take() {
                    key_stack.push(key);
                }
                continue;
            }

            // Handle key => [ (nested array on same line)
            if let Some(caps) = key_pattern.captures(trimmed) {
                let key_name = caps.get(1).unwrap().as_str();

                if trimmed.contains("=> [") || trimmed.ends_with("=> [") {
                    // This is a nested array
                    pending_key = Some(key_name.to_string());
                    in_array_depth += 1;
                    key_stack.push(key_name.to_string());
                } else {
                    // This is a simple key => value
                    let full_key = format!("{}.{}", key_stack.join("."), key_name);

                    // Extract value
                    let value = Self::extract_translation_value(trimmed);
                    results.push((full_key, value));
                }
            }

            // Handle array closing
            let close_count = trimmed.matches(']').count();
            for _ in 0..close_count {
                if in_array_depth > 0 {
                    in_array_depth -= 1;
                    if key_stack.len() > 1 {
                        key_stack.pop();
                    }
                }
            }
        }

        results
    }

    /// Extract the value from a translation line like "'key' => 'value',"
    fn extract_translation_value(line: &str) -> String {
        if let Some(arrow_pos) = line.find("=>") {
            let after_arrow = &line[arrow_pos + 2..];
            let value = after_arrow.trim().trim_end_matches(',').trim();

            // Remove quotes and truncate
            let unquoted = value
                .trim_start_matches('\'')
                .trim_start_matches('"')
                .trim_end_matches('\'')
                .trim_end_matches('"');

            // Truncate long values for display
            if unquoted.len() > 50 {
                format!("{}...", &unquoted[..47])
            } else {
                unquoted.to_string()
            }
        } else {
            String::new()
        }
    }

    /// Get all validation rules (built-in + custom from app/Rules/)
    async fn get_all_validation_rules(&self) -> Vec<ValidationRuleInfo> {
        let mut rules = get_laravel_validation_rules();

        // Add custom rules from app/Rules/ directory
        let root = match self.root_path.read().await.clone() {
            Some(r) => r,
            None => return rules,
        };

        let rules_dir = root.join("app").join("Rules");
        if rules_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&rules_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().is_some_and(|e| e == "php") {
                        if let Some(file_name) = path.file_stem().and_then(|s| s.to_str()) {
                            // Convert PascalCase to snake_case for the rule name
                            let rule_name = Self::pascal_to_snake_case(file_name);
                            let source = format!("app/Rules/{}.php", file_name);

                            rules.push(ValidationRuleInfo {
                                name: rule_name,
                                description: format!("Custom rule: {}", file_name),
                                has_params: false, // Custom rules typically don't have inline params
                                source,
                            });
                        }
                    }
                }
            }
        }

        // Sort alphabetically
        rules.sort_by(|a, b| a.name.cmp(&b.name));
        rules
    }

    /// Convert PascalCase to snake_case
    /// e.g., "Uppercase" -> "uppercase", "ValidEmail" -> "valid_email"
    fn pascal_to_snake_case(s: &str) -> String {
        let mut result = String::new();
        for (i, c) in s.chars().enumerate() {
            if c.is_uppercase() {
                if i > 0 {
                    result.push('_');
                }
                result.push(c.to_lowercase().next().unwrap());
            } else {
                result.push(c);
            }
        }
        result
    }

    /// Parse a PHP config file to extract all keys and values
    /// Returns a list of (key, value) tuples with dot-notation keys
    fn parse_config_keys(
        content: &str,
        base_key: &str,
        env_vars: &std::collections::HashMap<String, String>,
    ) -> Vec<(String, String)> {
        let mut results = Vec::new();

        // Simple regex-based parsing for Laravel config files
        // This handles the common patterns: 'key' => value, or "key" => value
        // Note: Allows hyphens in keys (kebab-case is common in Laravel configs)
        let key_pattern = regex::Regex::new(r#"['"]([a-zA-Z_][a-zA-Z0-9_-]*)['"][\s]*=>"#).unwrap();

        // Track nesting depth and current key path
        let mut key_stack: Vec<String> = vec![base_key.to_string()];
        let mut in_array_depth = 0;
        let mut pending_key: Option<String> = None;

        for line in content.lines() {
            let trimmed = line.trim();

            // Skip comments and empty lines
            if trimmed.is_empty()
                || trimmed.starts_with("//")
                || trimmed.starts_with("/*")
                || trimmed.starts_with("*")
            {
                continue;
            }

            // Handle array opening
            if trimmed.contains("[") && !trimmed.contains("=>") {
                in_array_depth += 1;
                if let Some(key) = pending_key.take() {
                    key_stack.push(key);
                }
                continue;
            }

            // Handle key => [ (nested array on same line)
            if let Some(caps) = key_pattern.captures(trimmed) {
                let key_name = caps.get(1).unwrap().as_str();

                if trimmed.contains("=> [") || trimmed.ends_with("=> [") {
                    // This is a nested array
                    pending_key = Some(key_name.to_string());
                    in_array_depth += 1;
                    key_stack.push(key_name.to_string());
                } else {
                    // This is a simple key => value
                    let full_key = format!("{}.{}", key_stack.join("."), key_name);

                    // Extract value and resolve env() references
                    let value = Self::extract_config_value(trimmed, env_vars);
                    results.push((full_key, value));
                }
            }

            // Handle array closing
            let close_count = trimmed.matches(']').count();
            for _ in 0..close_count {
                if in_array_depth > 0 {
                    in_array_depth -= 1;
                    if key_stack.len() > 1 {
                        key_stack.pop();
                    }
                }
            }
        }

        results
    }

    /// Extract the value from a config line like "'key' => value,"
    /// Resolves env() references using the provided env_vars map
    fn extract_config_value(
        line: &str,
        env_vars: &std::collections::HashMap<String, String>,
    ) -> String {
        if let Some(arrow_pos) = line.find("=>") {
            let after_arrow = &line[arrow_pos + 2..];
            let value = after_arrow.trim().trim_end_matches(',').trim();

            // Check for env() call pattern: env('VAR_NAME') or env('VAR_NAME', 'default')
            let resolved = Self::resolve_env_value(value, env_vars);

            // Truncate long values for display
            let display_value = if resolved.len() > 50 {
                format!("{}...", &resolved[..47])
            } else {
                resolved
            };

            display_value
        } else {
            String::new()
        }
    }

    /// Resolve an env() call to its actual value
    /// Handles: env('VAR'), env('VAR', 'default'), env('VAR', default_value)
    fn resolve_env_value(
        value: &str,
        env_vars: &std::collections::HashMap<String, String>,
    ) -> String {
        // Match env('VAR_NAME') or env('VAR_NAME', default) or env("VAR_NAME", default)
        let env_pattern =
            regex::Regex::new(r#"env\s*\(\s*['"]([A-Z_][A-Z0-9_]*)['"](?:\s*,\s*(.+))?\s*\)"#)
                .unwrap();

        if let Some(caps) = env_pattern.captures(value) {
            let var_name = caps.get(1).unwrap().as_str();

            // Try to get value from env vars
            if let Some(env_value) = env_vars.get(var_name) {
                return env_value.clone();
            }

            // Fall back to default if provided
            if let Some(default_match) = caps.get(2) {
                let default = default_match.as_str().trim();
                // Clean up quotes from default value
                return default.trim_matches('\'').trim_matches('"').to_string();
            }

            // No value found, return the var name as placeholder
            return format!("${{{}}}", var_name);
        }

        // Check for (bool) env(...) pattern
        let bool_env_pattern = regex::Regex::new(
            r#"\(bool\)\s*env\s*\(\s*['"]([A-Z_][A-Z0-9_]*)['"](?:\s*,\s*(.+))?\s*\)"#,
        )
        .unwrap();

        if let Some(caps) = bool_env_pattern.captures(value) {
            let var_name = caps.get(1).unwrap().as_str();

            if let Some(env_value) = env_vars.get(var_name) {
                return env_value.clone();
            }

            if let Some(default_match) = caps.get(2) {
                let default = default_match.as_str().trim();
                return default.to_string();
            }

            return format!("${{{}}}", var_name);
        }

        // Not an env() call, clean up and return as-is
        value.trim_matches('\'').trim_matches('"').to_string()
    }

    // ========================================================================
    // Code Action helpers
    // ========================================================================

    /// Extract the expected file path from a diagnostic message
    /// e.g., "View file not found: 'welcome'\nExpected at: /path/to/view.blade.php"
    /// Returns: Some("/path/to/view.blade.php")
    fn extract_expected_path(message: &str) -> Option<&str> {
        // Look for "Expected at: " and extract the path after it
        const MARKER: &str = "\nExpected at: ";
        if let Some(idx) = message.find(MARKER) {
            let after = &message[idx + MARKER.len()..];
            // Path ends at newline or end of string
            let end = after.find('\n').unwrap_or(after.len());
            Some(&after[..end])
        } else {
            None
        }
    }

    /// Extract a name from a diagnostic message between prefix and suffix
    /// e.g., extract_name_from_diagnostic("View file not found: 'welcome'", "View file not found: '", "'")
    /// Returns: Some("welcome")
    fn extract_name_from_diagnostic<'a>(
        message: &'a str,
        prefix: &str,
        suffix: &str,
    ) -> Option<&'a str> {
        if let Some(start) = message.find(prefix) {
            let after_prefix = &message[start + prefix.len()..];
            if let Some(end) = after_prefix.find(suffix) {
                return Some(&after_prefix[..end]);
            }
        }
        None
    }

    /// Extract the "Copy from:" path from a diagnostic message (for .env.example → .env)
    /// e.g., "...\nCopy from: /path/to/.env.example"
    /// Returns: Some(PathBuf("/path/to/.env.example"))
    fn extract_copy_from_path(message: &str) -> Option<PathBuf> {
        const MARKER: &str = "\nCopy from: ";
        if let Some(idx) = message.find(MARKER) {
            let after = &message[idx + MARKER.len()..];
            // Path ends at newline or end of string
            let end = after.find('\n').unwrap_or(after.len());
            Some(PathBuf::from(&after[..end]))
        } else {
            None
        }
    }

    /// Get the content for a new file using Laravel's stub system
    /// Priority: 1. stubs/*.stub (user customized)
    ///           2. vendor/.../stubs/*.stub (framework/package default)
    ///           3. Fallback template
    async fn get_stub_content(&self, action: &FileAction) -> String {
        // These types don't use stubs - they use simple templates or generate their own
        if matches!(
            action.action_type,
            FileActionType::TranslationPhp
                | FileActionType::TranslationJson
                | FileActionType::ConfigPhp
                | FileActionType::EnvVar
                | FileActionType::BladeComponentWithClass
        ) {
            return Self::fallback_template(action);
        }

        let root = self.root_path.read().await;

        // Get stub paths based on action type
        let (custom_stub, framework_stub): (&str, Option<&str>) = match action.action_type {
            FileActionType::View => (
                "stubs/view.stub",
                Some("vendor/laravel/framework/src/Illuminate/Foundation/Console/stubs/view.stub"),
            ),
            FileActionType::BladeComponent => (
                "stubs/component.stub",
                None, // No framework stub for anonymous components
            ),
            FileActionType::Livewire => (
                "stubs/livewire.stub",
                Some("vendor/livewire/livewire/src/Commands/stubs/component.stub"),
            ),
            FileActionType::Middleware => (
                "stubs/middleware.stub",
                Some(
                    "vendor/laravel/framework/src/Illuminate/Routing/Console/stubs/middleware.stub",
                ),
            ),
            FileActionType::Feature => (
                "stubs/feature.stub",
                Some("vendor/laravel/pennant/stubs/feature.stub"),
            ),
            // These types handled above (early return)
            FileActionType::TranslationPhp
            | FileActionType::TranslationJson
            | FileActionType::ConfigPhp
            | FileActionType::EnvVar
            | FileActionType::BladeComponentWithClass => {
                return Self::fallback_template(action);
            }
        };

        if let Some(root) = root.as_ref() {
            // 1. Check user's customized stub
            let custom_path = root.join(custom_stub);
            if custom_path.exists() {
                if let Ok(content) = std::fs::read_to_string(&custom_path) {
                    return Self::replace_stub_placeholders(&content, action);
                }
            }

            // 2. Check framework/package stub
            if let Some(fw_stub) = framework_stub {
                let fw_path = root.join(fw_stub);
                if fw_path.exists() {
                    if let Ok(content) = std::fs::read_to_string(&fw_path) {
                        return Self::replace_stub_placeholders(&content, action);
                    }
                }
            }
        }

        // 3. Fallback to built-in template
        Self::fallback_template(action)
    }

    /// Replace common stub placeholders with actual values
    fn replace_stub_placeholders(content: &str, action: &FileAction) -> String {
        let class_name = match action.action_type {
            FileActionType::Feature => feature_key_to_class_name(&action.name),
            _ => Self::kebab_to_pascal_case(&action.name),
        };
        let view_name = action.name.replace('.', "/");

        // Determine namespace based on action type
        let namespace = match action.action_type {
            FileActionType::Feature => "App\\Features".to_string(),
            FileActionType::Livewire => "App\\Livewire".to_string(),
            FileActionType::Middleware => "App\\Http\\Middleware".to_string(),
            _ => "App".to_string(),
        };

        content
            .replace("{{ class }}", &class_name)
            .replace("{{class}}", &class_name)
            .replace("{{ name }}", &action.name)
            .replace("{{name}}", &action.name)
            .replace("{{ view }}", &view_name)
            .replace("{{view}}", &view_name)
            .replace("{{ namespace }}", &namespace)
            .replace("{{namespace}}", &namespace)
    }

    /// Get fallback template when no stub is available
    fn fallback_template(action: &FileAction) -> String {
        match action.action_type {
            FileActionType::View => "<div>\n    \n</div>\n".to_string(),
            FileActionType::BladeComponent => {
                "@props([])\n\n<div>\n    {{ $slot }}\n</div>\n".to_string()
            }
            FileActionType::Livewire => {
                // For nested components like "admin.dashboard" or "admin.user-profile":
                // - Class name: last segment in PascalCase ("Dashboard", "UserProfile")
                // - Namespace: App\Livewire + intermediate segments ("App\Livewire\Admin")
                // - View: dots preserved ("livewire.admin.dashboard")
                let parts: Vec<&str> = action.name.split('.').collect();
                let class_name =
                    Self::kebab_to_pascal_case(parts.last().unwrap_or(&action.name.as_str()));

                // Build namespace from intermediate segments
                let namespace = if parts.len() > 1 {
                    let namespace_parts: Vec<String> = parts[..parts.len() - 1]
                        .iter()
                        .map(|p| Self::kebab_to_pascal_case(p))
                        .collect();
                    format!("App\\Livewire\\{}", namespace_parts.join("\\"))
                } else {
                    "App\\Livewire".to_string()
                };

                // View name keeps dots (e.g., "admin.dashboard" -> "livewire.admin.dashboard")
                let view_name = &action.name;

                format!(
                    r#"<?php

namespace {};

use Livewire\Component;

class {} extends Component
{{
    public function render()
    {{
        return view('livewire.{}');
    }}
}}
"#,
                    namespace, class_name, view_name
                )
            }
            FileActionType::Middleware => {
                let class_name = Self::kebab_to_pascal_case(&action.name);
                format!(
                    r#"<?php

namespace App\Http\Middleware;

use Closure;
use Illuminate\Http\Request;
use Symfony\Component\HttpFoundation\Response;

class {}
{{
    /**
     * Handle an incoming request.
     */
    public function handle(Request $request, Closure $next): Response
    {{
        return $next($request);
    }}
}}
"#,
                    class_name
                )
            }
            FileActionType::Feature => {
                // Convert feature key (e.g., "new-api") to class name (e.g., "NewApi")
                let class_name = feature_key_to_class_name(&action.name);
                format!(
                    r#"<?php

namespace App\Features;

class {}
{{
    /**
     * Resolve the feature's initial value.
     */
    public function resolve(mixed $scope): mixed
    {{
        return false;
    }}
}}
"#,
                    class_name
                )
            }
            FileActionType::TranslationPhp => {
                // For PHP files, the key is the nested key (e.g., "welcome" from "messages.welcome")
                let key = action.name.split('.').next_back().unwrap_or(&action.name);
                let escaped_key = key.replace('\\', "\\\\").replace('\'', "\\'");
                format!(
                    r#"<?php

return [
    '{}' => '{}',
];
"#,
                    escaped_key, escaped_key
                )
            }
            FileActionType::TranslationJson => {
                // For JSON files, use the full key as both key and value
                let escaped_key = action.name.replace('\\', "\\\\").replace('"', "\\\"");
                format!(
                    r#"{{
    "{}": "{}"
}}
"#,
                    escaped_key, escaped_key
                )
            }
            FileActionType::ConfigPhp => {
                // For config files, use nested key with empty string value
                let key = action.name.split('.').next_back().unwrap_or(&action.name);
                let escaped_key = key.replace('\\', "\\\\").replace('\'', "\\'");
                format!(
                    r#"<?php

return [
    '{}' => '',
];
"#,
                    escaped_key
                )
            }
            FileActionType::EnvVar => {
                // For .env files, just the KEY= line
                format!("{}=\n", action.name)
            }
            // This type generates its own template in build_code_action()
            FileActionType::BladeComponentWithClass => String::new(),
        }
    }

    // ========================================================================
    // Translation validation helpers
    // ========================================================================

    /// Check if a translation file exists for the given key
    ///
    /// Dotted keys like "validation.required" look in lang/en/validation.php
    /// Text keys like "Welcome to our app" look in lang/en.json
    fn check_translation_file(root: &Path, translation_key: &str) -> TranslationCheck {
        let is_dotted_key = translation_key.contains('.') && !translation_key.contains(' ');
        let is_multi_word = translation_key.contains(' ');

        let mut exists = false;
        let mut expected_path: Option<PathBuf> = None;
        let mut file_exists = false;
        let mut nested_key: Option<String> = None;

        if is_multi_word || (!is_dotted_key && !translation_key.contains('.')) {
            // Text key: check JSON files for the KEY, not just file existence
            let json_paths = [
                root.join("lang/en.json"),
                root.join("resources/lang/en.json"),
            ];

            // Set the expected path to the first option (preferred location)
            expected_path = Some(json_paths[0].clone());
            nested_key = Some(translation_key.to_string());

            for json_path in &json_paths {
                if json_path.exists() {
                    file_exists = true;
                    expected_path = Some(json_path.clone());
                    // Parse JSON and check if key exists
                    if let Ok(content) = std::fs::read_to_string(json_path) {
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                            if json.get(translation_key).is_some() {
                                exists = true;
                                break;
                            }
                        }
                    }
                    break; // Use the first existing file
                }
            }
        } else if is_dotted_key {
            // Dotted key: check PHP file based on first segment
            let parts: Vec<&str> = translation_key.split('.').collect();
            if !parts.is_empty() {
                let file_name = parts[0];
                // The nested key is everything after the first dot
                nested_key = Some(parts[1..].join("."));

                let php_paths = [
                    root.join("lang/en").join(format!("{}.php", file_name)),
                    root.join("resources/lang/en")
                        .join(format!("{}.php", file_name)),
                ];

                // Set the expected path to the first option (preferred location)
                expected_path = Some(php_paths[0].clone());

                for php_path in &php_paths {
                    if php_path.exists() {
                        file_exists = true;
                        exists = true; // For PHP, we only check file existence currently
                        expected_path = Some(php_path.clone());
                        break;
                    }
                }
            }
        }

        TranslationCheck {
            exists,
            is_dotted_key,
            expected_path,
            file_exists,
            nested_key,
        }
    }

    /// Create a diagnostic for a missing translation
    ///
    /// - `dotted_severity`: Severity for dotted keys (ERROR in PHP, WARNING in @lang)
    /// - Text keys always get INFORMATION severity
    fn create_translation_diagnostic(
        translation_key: &str,
        check: &TranslationCheck,
        line: u32,
        column: u32,
        end_column: u32,
        dotted_severity: DiagnosticSeverity,
    ) -> Diagnostic {
        let expected_path_str = check
            .expected_path
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        let (severity, message) = if check.is_dotted_key {
            let action_hint = if check.file_exists {
                format!(
                    "\nKey '{}' not found in file",
                    check.nested_key.as_deref().unwrap_or(translation_key)
                )
            } else {
                "\nFile does not exist".to_string()
            };
            (
                dotted_severity,
                format!(
                    "Translation not found: '{}'\nExpected at: {}{}",
                    translation_key, expected_path_str, action_hint
                ),
            )
        } else {
            let action_hint = if check.file_exists {
                format!("\nKey '{}' not found in file", translation_key)
            } else {
                "\nFile does not exist".to_string()
            };
            (
                DiagnosticSeverity::INFORMATION,
                format!(
                    "Translation not found: '{}'\nExpected at: {}{}",
                    translation_key, expected_path_str, action_hint
                ),
            )
        };

        Diagnostic {
            range: Range {
                start: Position {
                    line,
                    character: column,
                },
                end: Position {
                    line,
                    character: end_column,
                },
            },
            severity: Some(severity),
            code: None,
            source: Some("laravel".to_string()),
            message,
            related_information: None,
            tags: None,
            code_description: None,
            data: None,
        }
    }

    // ========================================================================
    // Config validation helpers
    // ========================================================================

    /// Check if a config file/key exists for the given key
    ///
    /// Config keys like "app.name" look in config/app.php
    fn check_config_file(root: &Path, config_key: &str) -> ConfigCheck {
        // Config keys are always dotted (e.g., "app.name", "database.connections.mysql")
        let parts: Vec<&str> = config_key.split('.').collect();

        if parts.is_empty() {
            return ConfigCheck {
                exists: false,
                expected_path: None,
                file_exists: false,
                nested_key: None,
            };
        }

        let file_name = parts[0];
        let nested_key = if parts.len() > 1 {
            Some(parts[1..].join("."))
        } else {
            None
        };

        let config_path = root.join("config").join(format!("{}.php", file_name));
        let file_exists = config_path.exists();

        // For now, we only check file existence, not key existence within the file
        // (Parsing PHP arrays to check for keys would be complex)
        ConfigCheck {
            exists: file_exists,
            expected_path: Some(config_path),
            file_exists,
            nested_key,
        }
    }

    /// Create a diagnostic for a missing config
    fn create_config_diagnostic(
        config_key: &str,
        check: &ConfigCheck,
        line: u32,
        column: u32,
        end_column: u32,
    ) -> Diagnostic {
        let expected_path_str = check
            .expected_path
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        let action_hint = if check.file_exists {
            format!(
                "\nKey '{}' not found in file",
                check.nested_key.as_deref().unwrap_or(config_key)
            )
        } else {
            "\nFile does not exist".to_string()
        };

        let message = format!(
            "Config not found: '{}'\nExpected at: {}{}",
            config_key, expected_path_str, action_hint
        );

        Diagnostic {
            range: Range {
                start: Position {
                    line,
                    character: column,
                },
                end: Position {
                    line,
                    character: end_column,
                },
            },
            severity: Some(DiagnosticSeverity::WARNING),
            code: None,
            source: Some("laravel".to_string()),
            message,
            related_information: None,
            tags: None,
            code_description: None,
            data: None,
        }
    }

    // ========================================================================
    // Salsa-based helper functions (for cached pattern data)
    // ========================================================================

    /// Create LocationLink for a view reference from Salsa data
    async fn create_view_location_from_salsa(
        &self,
        view: &ViewReferenceData,
    ) -> Option<GotoDefinitionResponse> {
        let config = self.get_cached_config().await?;
        let possible_paths = config.resolve_view_path(&view.name);

        for path in possible_paths {
            if self.file_exists_cached(&path).await {
                if let Ok(target_uri) = Url::from_file_path(&path) {
                    let origin_selection_range = Range {
                        start: Position {
                            line: view.line,
                            character: view.column,
                        },
                        end: Position {
                            line: view.line,
                            character: view.end_column,
                        },
                    };
                    return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                        origin_selection_range: Some(origin_selection_range),
                        target_uri,
                        target_range: Range::default(),
                        target_selection_range: Range::default(),
                    }]));
                }
            }
        }
        None
    }

    /// Create LocationLink for a component reference from Salsa data
    async fn create_component_location_from_salsa(
        &self,
        comp: &ComponentReferenceData,
    ) -> Option<GotoDefinitionResponse> {
        let config = self.get_cached_config().await?;
        let possible_paths = config.resolve_component_path(&comp.name);

        for path in possible_paths {
            if self.file_exists_cached(&path).await {
                if let Ok(target_uri) = Url::from_file_path(&path) {
                    let origin_selection_range = Range {
                        start: Position {
                            line: comp.line,
                            character: comp.column,
                        },
                        end: Position {
                            line: comp.line,
                            character: comp.end_column,
                        },
                    };
                    return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                        origin_selection_range: Some(origin_selection_range),
                        target_uri,
                        target_range: Range::default(),
                        target_selection_range: Range::default(),
                    }]));
                }
            }
        }
        None
    }

    /// Create LocationLink for an `<x-slot:name>` tag.
    ///
    /// Slots aren't components, so they don't appear in the position index.
    /// This handler runs as a fallback when no pattern matches the cursor.
    ///
    /// Resolution:
    /// 1. Check whether the cursor actually sits on a named slot tag.
    /// 2. Walk the Blade AST upward to find the enclosing `<x-component>` tag.
    /// 3. Resolve that parent component to a view file path using the existing
    ///    component resolver (which already consults the alias map).
    /// 4. Search the resolved file for `{{ $slot_name }}` so the jump lands on
    ///    the relevant line instead of the file's top.
    async fn create_slot_location(
        &self,
        uri: &Url,
        position: Position,
    ) -> Option<GotoDefinitionResponse> {
        use laravel_lsp::slot_navigation::{
            find_enclosing_parent_component, find_slot_at_position, locate_slot_in_view,
        };

        let source = {
            let docs = self.documents.read().await;
            let (text, _) = docs.get(uri)?;
            text.clone()
        };

        let slot = find_slot_at_position(&source, position.line, position.character)?;
        let parent = find_enclosing_parent_component(&source, slot.byte_start)?;

        let config = self.get_cached_config().await?;
        let possible_paths = config.resolve_component_path(&parent.name);

        for path in possible_paths {
            if !self.file_exists_cached(&path).await {
                continue;
            }
            let Ok(target_uri) = Url::from_file_path(&path) else {
                continue;
            };

            let (target_line, target_col) =
                locate_slot_in_view(&path, &slot.name).unwrap_or((0, 0));

            let target_range = Range {
                start: Position {
                    line: target_line,
                    character: target_col,
                },
                end: Position {
                    line: target_line,
                    character: target_col + slot.name.len() as u32 + 1,
                },
            };

            return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                origin_selection_range: None,
                target_uri,
                target_range,
                target_selection_range: target_range,
            }]));
        }

        None
    }

    /// Create LocationLink for a Livewire reference from Salsa data
    async fn create_livewire_location_from_salsa(
        &self,
        lw: &LivewireReferenceData,
    ) -> Option<GotoDefinitionResponse> {
        // Routes through the new resolver so v4 SFC and MFC components
        // resolve correctly (the old `LaravelConfigData::resolve_livewire_path`
        // only knew about `app/Livewire/{Pascal}.php`).
        let path = self.resolve_livewire_primary_path(&lw.name).await?;

        if self.file_exists_cached(&path).await {
            if let Ok(target_uri) = Url::from_file_path(&path) {
                let origin_selection_range = Range {
                    start: Position {
                        line: lw.line,
                        character: lw.column,
                    },
                    end: Position {
                        line: lw.line,
                        character: lw.end_column,
                    },
                };
                return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                    origin_selection_range: Some(origin_selection_range),
                    target_uri,
                    target_range: Range::default(),
                    target_selection_range: Range::default(),
                }]));
            }
        }
        None
    }

    /// Create LocationLink for a directive reference from Salsa data
    async fn create_directive_location_from_salsa(
        &self,
        dir: &DirectiveReferenceData,
    ) -> Option<GotoDefinitionResponse> {
        let arguments = dir.arguments.as_ref()?;
        let config = self.get_cached_config().await?;

        // Directives where first argument is a view name
        let view_directives_first_arg =
            ["extends", "include", "includeIf", "includeUnless", "each"];

        // Directives where second argument is a view name (after a condition)
        let view_directives_second_arg = ["includeWhen"];

        // @component directive - resolves to component file
        if dir.name == "component" {
            if let Some(component_name) = Self::extract_view_from_directive_args(arguments) {
                // Try as component path (resources/views/components/...)
                let component_path = format!("components.{}", component_name);
                let possible_paths = config.resolve_view_path(&component_path);

                for path in possible_paths {
                    if self.file_exists_cached(&path).await {
                        return self.create_location_link(dir, &path);
                    }
                }

                // Also try direct view path
                let possible_paths = config.resolve_view_path(&component_name);
                for path in possible_paths {
                    if self.file_exists_cached(&path).await {
                        return self.create_location_link(dir, &path);
                    }
                }
            }
        }

        // Handle view directives (first argument is view name)
        if view_directives_first_arg.contains(&dir.name.as_str()) {
            if let Some(view_name) = Self::extract_view_from_directive_args(arguments) {
                let possible_paths = config.resolve_view_path(&view_name);

                for path in possible_paths {
                    if self.file_exists_cached(&path).await {
                        return self.create_location_link(dir, &path);
                    }
                }
            }
        }

        // Handle @includeWhen($condition, 'view') - second arg is view
        if view_directives_second_arg.contains(&dir.name.as_str()) {
            if let Some(view_name) = Self::extract_second_string_arg(arguments) {
                let possible_paths = config.resolve_view_path(&view_name);

                for path in possible_paths {
                    if self.file_exists_cached(&path).await {
                        return self.create_location_link(dir, &path);
                    }
                }
            }
        }

        // Handle @includeFirst(['view1', 'view2']) - array of views
        if dir.name == "includeFirst" {
            let view_names = Self::extract_array_string_args(arguments);
            for view_name in view_names {
                let possible_paths = config.resolve_view_path(&view_name);
                for path in possible_paths {
                    if self.file_exists_cached(&path).await {
                        return self.create_location_link(dir, &path);
                    }
                }
            }
        }

        // Note: @lang is now handled as Translation patterns (see parse_file_patterns in salsa_impl.rs)
        // Note: @vite is handled as Asset patterns, not Directive patterns
        // See parse_file_patterns in salsa_impl.rs

        // Handle @livewire('component-name') - Livewire component directive
        // Navigates to the Blade view using view_path resolution
        if dir.name == "livewire" {
            if let Some(component_name) = Self::extract_view_from_directive_args(arguments) {
                // Resolve using view path (e.g., 'navigation-menu' -> resources/views/navigation-menu.blade.php)
                let possible_paths = config.resolve_view_path(&component_name);

                for path in possible_paths {
                    if self.file_exists_cached(&path).await {
                        // Use string_column/string_end_column for the clickable range (just the component name)
                        return self.create_location_link_with_string_range(dir, &path);
                    }
                }
            }
        }

        // Handle @feature('feature-name') - Laravel Pennant feature directive
        if dir.name == "feature" {
            if let Some(feature_name) = Self::extract_view_from_directive_args(arguments) {
                let root = self.root_path.read().await;
                if let Some(root) = root.as_ref() {
                    // First check scanned features for custom $name property matches
                    let scanned_features = scan_feature_classes(root);
                    let feature_path = if let Some(feature_info) = scanned_features
                        .iter()
                        .find(|f| f.feature_key == feature_name)
                    {
                        // Found a feature class with matching $name property or derived key
                        root.join("app/Features")
                            .join(format!("{}.php", feature_info.class_name))
                    } else {
                        // Fallback: Convert feature key to class name and build path
                        let class_name = feature_key_to_class_name(&feature_name);
                        root.join(format!("app/Features/{}.php", class_name))
                    };

                    if self.file_exists_cached(&feature_path).await {
                        // Use string_column/string_end_column for the clickable range (just the feature name)
                        return self.create_location_link_with_string_range(dir, &feature_path);
                    }
                }
            }
        }

        None
    }

    /// Helper to create a LocationLink for a directive
    fn create_location_link(
        &self,
        dir: &DirectiveReferenceData,
        path: &std::path::Path,
    ) -> Option<GotoDefinitionResponse> {
        let target_uri = Url::from_file_path(path).ok()?;
        let origin_selection_range = Range {
            start: Position {
                line: dir.line,
                character: dir.column,
            },
            end: Position {
                line: dir.line,
                character: dir.end_column,
            },
        };
        Some(GotoDefinitionResponse::Link(vec![LocationLink {
            origin_selection_range: Some(origin_selection_range),
            target_uri,
            target_range: Range::default(),
            target_selection_range: Range::default(),
        }]))
    }

    /// Helper to create a LocationLink using string_column/string_end_column range
    /// This highlights just the string content inside quotes, not the entire directive
    fn create_location_link_with_string_range(
        &self,
        dir: &DirectiveReferenceData,
        path: &std::path::Path,
    ) -> Option<GotoDefinitionResponse> {
        let target_uri = Url::from_file_path(path).ok()?;
        let origin_selection_range = Range {
            start: Position {
                line: dir.line,
                character: dir.string_column,
            },
            end: Position {
                line: dir.line,
                character: dir.string_end_column,
            },
        };
        Some(GotoDefinitionResponse::Link(vec![LocationLink {
            origin_selection_range: Some(origin_selection_range),
            target_uri,
            target_range: Range::default(),
            target_selection_range: Range::default(),
        }]))
    }

    /// Extract the second string argument from directive args
    /// For @includeWhen($condition, 'view.name', $data)
    fn extract_second_string_arg(arguments: &str) -> Option<String> {
        // Find second quoted string after a comma
        let mut in_string = false;
        let mut quote_char = ' ';
        let mut found_first = false;
        let mut result = String::new();
        let mut capturing = false;

        for ch in arguments.chars() {
            if !in_string {
                if ch == '\'' || ch == '"' {
                    if found_first {
                        // Start capturing second string
                        in_string = true;
                        quote_char = ch;
                        capturing = true;
                    } else {
                        // Skip first string
                        in_string = true;
                        quote_char = ch;
                    }
                }
            } else if ch == quote_char {
                in_string = false;
                if capturing {
                    return Some(result);
                }
                found_first = true;
            } else if capturing {
                result.push(ch);
            }
        }
        None
    }

    /// Extract array of string arguments from directive args
    /// For @includeFirst(['view1', 'view2'])
    fn extract_array_string_args(arguments: &str) -> Vec<String> {
        let mut results = Vec::new();
        let mut current = String::new();
        let mut in_string = false;
        let mut quote_char = ' ';

        for ch in arguments.chars() {
            if !in_string {
                if ch == '\'' || ch == '"' {
                    in_string = true;
                    quote_char = ch;
                    current.clear();
                }
            } else if ch == quote_char {
                in_string = false;
                if !current.is_empty() {
                    results.push(current.clone());
                }
            } else {
                current.push(ch);
            }
        }
        results
    }

    /// Create LocationLink for an env reference using Salsa
    async fn create_env_location_from_salsa(
        &self,
        env: &EnvReferenceData,
    ) -> Option<GotoDefinitionResponse> {
        let env_var = self
            .salsa
            .get_parsed_env_var(env.name.clone())
            .await
            .ok()??;
        let target_uri = Url::from_file_path(&env_var.source_file).ok()?;
        let origin_selection_range = Range {
            start: Position {
                line: env.line,
                character: env.column,
            },
            end: Position {
                line: env.line,
                character: env.end_column,
            },
        };
        Some(GotoDefinitionResponse::Link(vec![LocationLink {
            origin_selection_range: Some(origin_selection_range),
            target_uri,
            target_range: Range {
                start: Position {
                    line: env_var.line,
                    character: env_var.column,
                },
                end: Position {
                    line: env_var.line,
                    character: env_var.column + env_var.name.len() as u32,
                },
            },
            target_selection_range: Range {
                start: Position {
                    line: env_var.line,
                    character: env_var.column,
                },
                end: Position {
                    line: env_var.line,
                    character: env_var.column + env_var.name.len() as u32,
                },
            },
        }]))
    }

    /// Create LocationLink for a config reference from Salsa data
    async fn create_config_location_from_salsa(
        &self,
        config_ref: &ConfigReferenceData,
    ) -> Option<GotoDefinitionResponse> {
        let project_config = self.get_cached_config().await?;

        // Parse config key like "app.name" -> file: config/app.php
        let parts: Vec<&str> = config_ref.key.split('.').collect();
        if parts.is_empty() {
            return None;
        }

        let config_file = parts[0];
        let config_path = project_config
            .root
            .join("config")
            .join(format!("{}.php", config_file));

        if self.file_exists_cached(&config_path).await {
            if let Ok(target_uri) = Url::from_file_path(&config_path) {
                let origin_selection_range = Range {
                    start: Position {
                        line: config_ref.line,
                        character: config_ref.column,
                    },
                    end: Position {
                        line: config_ref.line,
                        character: config_ref.end_column,
                    },
                };
                return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                    origin_selection_range: Some(origin_selection_range),
                    target_uri,
                    target_range: Range::default(),
                    target_selection_range: Range::default(),
                }]));
            }
        }
        None
    }

    /// Create LocationLink for a middleware reference
    /// Navigates to the alias declaration (e.g., in bootstrap/app.php)
    /// Uses cache-first lookup (disk cache → Salsa fallback)
    async fn create_middleware_location_from_salsa(
        &self,
        mw: &MiddlewareReferenceData,
    ) -> Option<GotoDefinitionResponse> {
        // Use unified cache-first lookup (same as diagnostics)
        // Returns (class_name, class_file, source_file, source_line) - we navigate to source_file
        let cached = self.get_cached_middleware(&mw.name).await;
        info!(
            "🔍 get_cached_middleware('{}') = {:?}",
            mw.name,
            cached
                .as_ref()
                .map(|(c, cf, sf, sl)| (c, cf.is_some(), sf.is_some(), sl))
        );

        let (_class_name, _class_file, source_file, source_line) = cached?;

        let source_path = match source_file {
            Some(p) => p,
            None => {
                info!("❌ source_file is None for middleware '{}'", mw.name);
                return None;
            }
        };

        if !self.file_exists_cached(&source_path).await {
            info!("❌ source_file does not exist: {:?}", source_path);
            return None;
        }

        let target_uri = Url::from_file_path(&source_path).ok()?;
        // LSP uses 0-based line numbers, but we store 1-based
        let target_line = source_line.unwrap_or(1).saturating_sub(1);

        let origin_selection_range = Range {
            start: Position {
                line: mw.line,
                character: mw.column,
            },
            end: Position {
                line: mw.line,
                character: mw.end_column,
            },
        };

        // Navigate to the specific line where the alias is declared
        let target_range = Range {
            start: Position {
                line: target_line,
                character: 0,
            },
            end: Position {
                line: target_line,
                character: 0,
            },
        };

        Some(GotoDefinitionResponse::Link(vec![LocationLink {
            origin_selection_range: Some(origin_selection_range),
            target_uri,
            target_range,
            target_selection_range: target_range,
        }]))
    }

    /// Create LocationLink for a translation reference from Salsa data
    async fn create_translation_location_from_salsa(
        &self,
        trans: &TranslationReferenceData,
    ) -> Option<GotoDefinitionResponse> {
        let root_guard = self.root_path.read().await;
        let root = root_guard.as_ref()?;

        // Determine if this is a dotted key (PHP file) or text key (JSON file)
        let is_dotted_key = trans.key.contains('.') && !trans.key.contains(' ');

        let translation_path = if is_dotted_key {
            // Dotted key: "validation.required" -> lang/en/validation.php
            let parts: Vec<&str> = trans.key.split('.').collect();
            if parts.is_empty() {
                return None;
            }
            root.join("lang")
                .join("en")
                .join(format!("{}.php", parts[0]))
        } else {
            // Text key: "Welcome to our app" -> lang/en.json
            root.join("lang").join("en.json")
        };

        if self.file_exists_cached(&translation_path).await {
            if let Ok(target_uri) = Url::from_file_path(&translation_path) {
                let origin_selection_range = Range {
                    start: Position {
                        line: trans.line,
                        character: trans.column,
                    },
                    end: Position {
                        line: trans.line,
                        character: trans.end_column,
                    },
                };

                // Find the line number of the key in the file
                let target_range = if !is_dotted_key {
                    // For JSON files, find the line where the key is defined
                    Self::find_json_key_location(&translation_path, &trans.key).unwrap_or_default()
                } else {
                    // For PHP files, default to start (could be enhanced later)
                    Range::default()
                };

                return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                    origin_selection_range: Some(origin_selection_range),
                    target_uri,
                    target_range,
                    target_selection_range: target_range,
                }]));
            }
        }
        None
    }

    /// Find the line and column of a key in a JSON translation file
    fn find_json_key_location(json_path: &Path, key: &str) -> Option<Range> {
        let content = std::fs::read_to_string(json_path).ok()?;

        // Search for the key pattern: "key": or "key" :
        // We look for the key surrounded by quotes at the start of a JSON property
        let search_pattern = format!("\"{}\"", key);

        for (line_num, line) in content.lines().enumerate() {
            if let Some(col) = line.find(&search_pattern) {
                // Found the key, position cursor at the start of the key (after the opening quote)
                let start_col = col + 1; // Skip the opening quote
                let end_col = start_col + key.len();

                return Some(Range {
                    start: Position {
                        line: line_num as u32,
                        character: start_col as u32,
                    },
                    end: Position {
                        line: line_num as u32,
                        character: end_col as u32,
                    },
                });
            }
        }

        None
    }

    /// Create LocationLink for an asset reference from Salsa data
    async fn create_asset_location_from_salsa(
        &self,
        asset: &AssetReferenceData,
    ) -> Option<GotoDefinitionResponse> {
        let root_guard = self.root_path.read().await;
        let root = root_guard.as_ref()?;

        // Determine the base path based on helper type
        use laravel_lsp::salsa_impl::AssetHelperType;
        let base_path = match asset.helper_type {
            AssetHelperType::Asset | AssetHelperType::PublicPath | AssetHelperType::Mix => {
                root.join("public")
            }
            AssetHelperType::BasePath => root.clone(),
            AssetHelperType::AppPath => root.join("app"),
            AssetHelperType::StoragePath => root.join("storage"),
            AssetHelperType::DatabasePath => root.join("database"),
            AssetHelperType::LangPath => root.join("lang"),
            AssetHelperType::ConfigPath => root.join("config"),
            AssetHelperType::ResourcePath | AssetHelperType::ViteAsset => root.join("resources"),
        };

        let asset_path = base_path.join(&asset.path);

        if self.file_exists_cached(&asset_path).await {
            if let Ok(target_uri) = Url::from_file_path(&asset_path) {
                let origin_selection_range = Range {
                    start: Position {
                        line: asset.line,
                        character: asset.column,
                    },
                    end: Position {
                        line: asset.line,
                        character: asset.end_column,
                    },
                };
                return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                    origin_selection_range: Some(origin_selection_range),
                    target_uri,
                    target_range: Range::default(),
                    target_selection_range: Range::default(),
                }]));
            }
        }
        None
    }

    /// Create LocationLink for a binding reference
    /// Navigates to the binding declaration (e.g., in AppServiceProvider.php)
    /// Uses cache-first lookup (disk cache → Salsa fallback)
    async fn create_binding_location_from_salsa(
        &self,
        binding: &BindingReferenceData,
    ) -> Option<GotoDefinitionResponse> {
        let root_guard = self.root_path.read().await;
        let root = root_guard.as_ref()?;

        // If it's a class reference (e.g., User::class), navigate directly to the class file
        if binding.is_class_reference {
            if let Some(path) = resolve_class_to_file(&binding.name, root) {
                if self.file_exists_cached(&path).await {
                    if let Ok(target_uri) = Url::from_file_path(&path) {
                        let origin_selection_range = Range {
                            start: Position {
                                line: binding.line,
                                character: binding.column,
                            },
                            end: Position {
                                line: binding.line,
                                character: binding.end_column,
                            },
                        };
                        return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                            origin_selection_range: Some(origin_selection_range),
                            target_uri,
                            target_range: Range::default(),
                            target_selection_range: Range::default(),
                        }]));
                    }
                }
            }
        }

        // For string bindings, navigate to the binding declaration
        if let Some((_class_name, _class_file, Some(path), source_line)) =
            self.get_cached_binding(&binding.name).await
        {
            if self.file_exists_cached(&path).await {
                if let Ok(target_uri) = Url::from_file_path(&path) {
                    // LSP uses 0-based line numbers, but we store 1-based
                    let target_line = source_line.unwrap_or(1).saturating_sub(1);
                    let origin_selection_range = Range {
                        start: Position {
                            line: binding.line,
                            character: binding.column,
                        },
                        end: Position {
                            line: binding.line,
                            character: binding.end_column,
                        },
                    };
                    let target_range = Range {
                        start: Position {
                            line: target_line,
                            character: 0,
                        },
                        end: Position {
                            line: target_line,
                            character: 0,
                        },
                    };
                    return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                        origin_selection_range: Some(origin_selection_range),
                        target_uri,
                        target_range,
                        target_selection_range: target_range,
                    }]));
                }
            }
        }

        None
    }

    /// Create a goto location for a route('name') call
    /// Navigates to the route definition in routes/*.php files
    async fn create_route_location_from_salsa(
        &self,
        route: &RouteReferenceData,
    ) -> Option<GotoDefinitionResponse> {
        // Look up directly in the pre-built index. The index is populated at
        // init time from `routes/**/*.php`, `vendor/*/routes/**/*.php`,
        // content-matched vendor PHP files (catches macro bodies like Laravel
        // UI's AuthRouteMethods), and app service providers that register
        // routes in their `boot()` methods.
        let index_guard = self.route_index.read().await;
        let index = index_guard.as_ref()?;
        let def = index.get(&route.name)?;

        let target_uri = Url::from_file_path(&def.file).ok()?;
        let origin_selection_range = Range {
            start: Position {
                line: route.line,
                character: route.column,
            },
            end: Position {
                line: route.line,
                character: route.end_column,
            },
        };
        let target_range = Range {
            start: Position {
                line: def.line,
                character: def.column,
            },
            end: Position {
                line: def.line,
                character: def.end_column,
            },
        };
        Some(GotoDefinitionResponse::Link(vec![LocationLink {
            origin_selection_range: Some(origin_selection_range),
            target_uri,
            target_range,
            target_selection_range: target_range,
        }]))
    }

    /// (Re)build the route name index by scanning project + vendor for
    /// `->name('X')` callsites. Cheap enough for cold start (vendor walk runs
    /// once); rescans on app-route changes can call this again.
    async fn rebuild_route_index(&self, root: &Path) {
        let root = root.to_path_buf();
        let index = tokio::task::spawn_blocking(move || {
            let files = discover_route_files(&root);
            let count = files.len();
            let index = build_route_index(&files);
            (count, index)
        })
        .await;

        match index {
            Ok((file_count, index)) => {
                info!(
                    "🛣️  Route index built: {} named routes from {} files",
                    index.len(),
                    file_count
                );
                *self.route_index.write().await = Some(index);
            }
            Err(e) => {
                tracing::warn!("Failed to build route index: {}", e);
            }
        }
    }

    /// Create a goto location for a url('path') call
    /// Navigates to the file in public directory if it exists
    async fn create_url_location_from_salsa(
        &self,
        url: &UrlReferenceData,
    ) -> Option<GotoDefinitionResponse> {
        let root_guard = self.root_path.read().await;
        let root = root_guard.as_ref()?;

        // url() generates URLs relative to public directory
        let path = url.path.trim_start_matches('/');
        let public_path = root.join("public").join(path);

        if self.file_exists_cached(&public_path).await {
            if let Ok(target_uri) = Url::from_file_path(&public_path) {
                let origin_selection_range = Range {
                    start: Position {
                        line: url.line,
                        character: url.column,
                    },
                    end: Position {
                        line: url.line,
                        character: url.end_column,
                    },
                };
                return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                    origin_selection_range: Some(origin_selection_range),
                    target_uri,
                    target_range: Range::default(),
                    target_selection_range: Range::default(),
                }]));
            }
        }

        None
    }

    /// Create a goto location for an action('Controller@method') call
    /// Navigates to the controller file
    async fn create_action_location_from_salsa(
        &self,
        action: &ActionReferenceData,
    ) -> Option<GotoDefinitionResponse> {
        let root_guard = self.root_path.read().await;
        let root = root_guard.as_ref()?;

        // Parse action string: "Controller@method" or "App\Http\Controllers\Controller@method"
        let parts: Vec<&str> = action.action.split('@').collect();
        let controller_class = parts.first()?;

        // Resolve controller to file path
        let path = resolve_class_to_file(controller_class, root)?;

        if self.file_exists_cached(&path).await {
            if let Ok(target_uri) = Url::from_file_path(&path) {
                let origin_selection_range = Range {
                    start: Position {
                        line: action.line,
                        character: action.column,
                    },
                    end: Position {
                        line: action.line,
                        character: action.end_column,
                    },
                };
                return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                    origin_selection_range: Some(origin_selection_range),
                    target_uri,
                    target_range: Range::default(),
                    target_selection_range: Range::default(),
                }]));
            }
        }

        None
    }

    /// Create a goto location for a Feature::active('feature-name') call
    /// Navigates to the feature class file in app/Features/
    async fn create_feature_location_from_salsa(
        &self,
        feature: &FeatureReferenceData,
    ) -> Option<GotoDefinitionResponse> {
        let root_guard = self.root_path.read().await;
        let root = root_guard.as_ref()?;

        // Convert feature name to class path
        let path = if feature.is_class_reference {
            // Class-based: Feature::active(NewApi::class)
            // The feature_name is already the class name like "NewApi" or "App\Features\NewApi"
            resolve_class_to_file(&feature.feature_name, root)?
        } else {
            // String-based: Feature::active('new-api')
            // First check scanned features for custom $name property matches
            let scanned_features = scan_feature_classes(root);
            if let Some(feature_info) = scanned_features
                .iter()
                .find(|f| f.feature_key == feature.feature_name)
            {
                // Found a feature class with matching $name property or derived key
                root.join("app/Features")
                    .join(format!("{}.php", feature_info.class_name))
            } else {
                // Fallback: Convert kebab-case/snake_case to PascalCase
                let class_name = feature_key_to_class_name(&feature.feature_name);
                root.join("app/Features")
                    .join(format!("{}.php", class_name))
            }
        };

        if self.file_exists_cached(&path).await {
            if let Ok(target_uri) = Url::from_file_path(&path) {
                let origin_selection_range = Range {
                    start: Position {
                        line: feature.line,
                        character: feature.column,
                    },
                    end: Position {
                        line: feature.line,
                        character: feature.end_column,
                    },
                };
                return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                    origin_selection_range: Some(origin_selection_range),
                    target_uri,
                    target_range: Range::default(),
                    target_selection_range: Range::default(),
                }]));
            }
        }

        None
    }

    /// Check if Laravel vendor is available and return diagnostic if not
    /// Only returns a diagnostic once per session to avoid spamming
    async fn get_vendor_missing_diagnostic(&self) -> Option<Diagnostic> {
        // Check if we've already shown this diagnostic
        let mut shown = self.vendor_diagnostic_shown.write().await;
        if *shown {
            return None;
        }

        // Get the project root
        let root_guard = self.root_path.read().await;
        let root = root_guard.as_ref()?;

        // Check if vendor/laravel/framework exists
        let vendor_path = root.join("vendor/laravel/framework");
        if vendor_path.exists() {
            return None;
        }

        // Mark as shown so we don't show it again
        *shown = true;

        Some(Diagnostic {
            range: Range {
                start: Position { line: 0, character: 0 },
                end: Position { line: 0, character: 0 },
            },
            severity: Some(DiagnosticSeverity::INFORMATION),
            code: None,
            source: Some("laravel".to_string()),
            message: "Laravel dependencies not installed. Run 'composer install' for full validation autocomplete support.".to_string(),
            related_information: None,
            tags: None,
            code_description: None,
            data: None,
        })
    }

    /// Check if database connection has failed and return an Info diagnostic if so
    /// This diagnostic is shown once per session when exists:/unique: validation rules are used
    /// but the database cannot be connected
    async fn get_database_error_diagnostic(&self) -> Option<Diagnostic> {
        // Check if we've already shown this diagnostic
        let mut shown = self.database_diagnostic_shown.write().await;
        if *shown {
            return None;
        }

        // Check if database schema provider exists and has an error
        let schema_guard = self.database_schema.read().await;
        if let Some(ref provider) = *schema_guard {
            if let Some(error) = provider.get_last_error().await {
                // Mark as shown so we don't show it again
                *shown = true;

                return Some(Diagnostic {
                    range: Range {
                        start: Position { line: 0, character: 0 },
                        end: Position { line: 0, character: 0 },
                    },
                    severity: Some(DiagnosticSeverity::INFORMATION),
                    code: None,
                    source: Some("laravel".to_string()),
                    message: format!(
                        "Database connection failed: {}\nConfigure database settings in .env for exists:/unique: validation autocomplete.",
                        error.message
                    ),
                    related_information: None,
                    tags: None,
                    code_description: None,
                    data: None,
                });
            }
        }

        None
    }

    /// Clone server for spawning async tasks
    fn clone_for_spawn(&self) -> Self {
        LaravelLanguageServer {
            client: self.client.clone(),
            documents: self.documents.clone(),
            root_path: self.root_path.clone(),
            diagnostics: self.diagnostics.clone(),
            pending_diagnostics: self.pending_diagnostics.clone(),
            debounce_delay_ms: self.debounce_delay_ms,
            salsa: self.salsa.clone(),
            cache: self.cache.clone(),
            pending_rescans: self.pending_rescans.clone(),
            rescan_debounce_handle: self.rescan_debounce_handle.clone(),
            file_exists_cache: self.file_exists_cache.clone(),
            cached_config: self.cached_config.clone(),
            cached_livewire: self.cached_livewire.clone(),
            last_goto_request: self.last_goto_request.clone(),
            initialized_root: self.initialized_root.clone(),
            pending_salsa_updates: self.pending_salsa_updates.clone(),
            auto_complete_debounce_ms: self.auto_complete_debounce_ms.clone(),
            directive_spacing: self.directive_spacing.clone(),
            vendor_diagnostic_shown: self.vendor_diagnostic_shown.clone(),
            cached_validation_rule_names: self.cached_validation_rule_names.clone(),
            database_schema: self.database_schema.clone(),
            database_diagnostic_shown: self.database_diagnostic_shown.clone(),
            route_index: self.route_index.clone(),
            vendor_translation_namespaces: self.vendor_translation_namespaces.clone(),
            route_decl_cache: self.route_decl_cache.clone(),
        }
    }

    /// Validate a document (Blade or PHP) and publish diagnostics
    ///
    /// This function uses Salsa-cached patterns for efficient incremental validation:
    /// 1. Gets pre-parsed patterns from Salsa (memoized, only re-parses on content change)
    /// 2. Validates patterns against config, env cache, and service registry
    /// 3. Creates diagnostics for missing files/undefined references
    /// 4. Publishes diagnostics to the editor
    async fn validate_and_publish_diagnostics(&self, uri: &Url, source: &str) {
        info!("🔍 validate_and_publish_diagnostics called for {}", uri);
        let mut diagnostics = Vec::new();

        // Check for vendor missing diagnostic (shows once per session)
        if let Some(vendor_diag) = self.get_vendor_missing_diagnostic().await {
            diagnostics.push(vendor_diag);
        }

        // Check for database connection error diagnostic (shows once per session)
        if let Some(db_diag) = self.get_database_error_diagnostic().await {
            diagnostics.push(db_diag);
        }

        // Get the Laravel config (checks memory cache first, then Salsa)
        let t_config = std::time::Instant::now();
        let config = match self.get_cached_config().await {
            Some(c) => c,
            None => {
                debug!("warn:  Cannot validate: config not set");
                return;
            }
        };
        info!("   ⏱️  get_cached_config: {:?}", t_config.elapsed());

        // Convert URI to file path for Salsa lookup
        let file_path = match uri.to_file_path() {
            Ok(path) => path,
            Err(_) => {
                debug!("warn:  Cannot convert URI to file path");
                return;
            }
        };

        // Determine file type
        let is_blade = uri.path().ends_with(".blade.php");
        let is_php = uri.path().ends_with(".php") && !is_blade;

        // Get patterns from Salsa (cached, incremental)
        let t_patterns = std::time::Instant::now();
        let patterns = match self.salsa.get_patterns(file_path.clone()).await {
            Ok(Some(p)) => p,
            Ok(None) => {
                debug!("warn:  No patterns found in Salsa for {}", uri);
                // Fall back to empty patterns - file might not be in Salsa yet
                // Ensure Salsa has the file before proceeding
                let _ = self
                    .salsa
                    .update_file(file_path.clone(), 0, source.to_string())
                    .await;
                match self.salsa.get_patterns(file_path.clone()).await {
                    Ok(Some(p)) => p,
                    _ => return,
                }
            }
            Err(e) => {
                debug!("warn:  Error getting patterns from Salsa: {}", e);
                return;
            }
        };
        info!("   ⏱️  salsa.get_patterns: {:?}", t_patterns.elapsed());

        // Validate PHP files with view() calls and env() calls
        if is_php {
            // Check view() calls using Salsa patterns
            for view_ref in &patterns.views {
                let possible_paths = config.resolve_view_path(&view_ref.name);
                let exists = possible_paths.iter().any(|p| p.exists());

                if !exists {
                    let expected_path = possible_paths
                        .first()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|| "unknown".to_string());

                    // All view() calls with missing files should be ERROR
                    let severity = DiagnosticSeverity::ERROR;

                    let diagnostic = Diagnostic {
                        range: Range {
                            start: Position {
                                line: view_ref.line,
                                character: view_ref.column,
                            },
                            end: Position {
                                line: view_ref.line,
                                character: view_ref.end_column,
                            },
                        },
                        severity: Some(severity),
                        code: None,
                        source: Some("laravel".to_string()),
                        message: format!(
                            "View file not found: '{}'\nExpected at: {}",
                            view_ref.name, expected_path
                        ),
                        related_information: None,
                        tags: None,
                        code_description: None,
                        data: None,
                    };
                    diagnostics.push(diagnostic);
                }
            }

            // Check env() calls using Salsa patterns - warn if variable not defined
            let root_for_env = self.root_path.read().await;
            for env_ref in &patterns.env_refs {
                let env_exists = self
                    .salsa
                    .get_parsed_env_var(env_ref.name.clone())
                    .await
                    .ok()
                    .flatten()
                    .is_some();

                if !env_exists {
                    // Determine paths based on root
                    let (env_path, env_example_path) = if let Some(root) = root_for_env.as_ref() {
                        let env = root.join(".env");
                        let env_example = root.join(".env.example");
                        (Some(env), Some(env_example))
                    } else {
                        (None, None)
                    };

                    // Check file existence
                    let env_exists = env_path.as_ref().map(|p| p.exists()).unwrap_or(false);
                    let env_example_exists = env_example_path
                        .as_ref()
                        .map(|p| p.exists())
                        .unwrap_or(false);

                    // Build the message with Expected at: and optionally Copy from:
                    // Format varies based on whether .env file exists:
                    // - .env exists: "not found in file" → append to file
                    // - .env doesn't exist + .env.example: "file not found" + "Copy from:" → copy
                    // - .env doesn't exist: "file not found" → create new
                    let (severity, message) = if env_ref.has_fallback {
                        let mut msg = if env_exists {
                            format!(
                                "Environment variable '{}' not found in file (using fallback value)",
                                env_ref.name
                            )
                        } else {
                            format!(
                                "Environment variable '{}' file not found (using fallback value)",
                                env_ref.name
                            )
                        };
                        // Add Expected at: for code action
                        if let Some(ref p) = env_path {
                            msg.push_str(&format!("\nExpected at: {}", p.display()));
                        }
                        // If .env doesn't exist but .env.example does, add Copy from:
                        if !env_exists && env_example_exists {
                            if let Some(ref p) = env_example_path {
                                msg.push_str(&format!("\nCopy from: {}", p.display()));
                            }
                        }
                        (DiagnosticSeverity::INFORMATION, msg)
                    } else {
                        let mut msg = if env_exists {
                            format!(
                                "Environment variable '{}' not found in file and has no fallback",
                                env_ref.name
                            )
                        } else {
                            format!(
                                "Environment variable '{}' file not found and has no fallback",
                                env_ref.name
                            )
                        };
                        // Add Expected at: for code action
                        if let Some(ref p) = env_path {
                            msg.push_str(&format!("\nExpected at: {}", p.display()));
                        }
                        // If .env doesn't exist but .env.example does, add Copy from:
                        if !env_exists && env_example_exists {
                            if let Some(ref p) = env_example_path {
                                msg.push_str(&format!("\nCopy from: {}", p.display()));
                            }
                        }
                        (DiagnosticSeverity::WARNING, msg)
                    };

                    let diagnostic = Diagnostic {
                        range: Range {
                            start: Position {
                                line: env_ref.line,
                                character: env_ref.column,
                            },
                            end: Position {
                                line: env_ref.line,
                                character: env_ref.end_column,
                            },
                        },
                        severity: Some(severity),
                        code: None,
                        source: Some("laravel".to_string()),
                        message,
                        related_information: None,
                        tags: None,
                        code_description: None,
                        data: None,
                    };
                    diagnostics.push(diagnostic);
                }
            }
            drop(root_for_env);

            // Warn about env() usage outside config files (configuration caching issue)
            // Per Laravel docs: "you should ensure you are only calling the env function
            // from within your application's configuration (config) files"
            // https://laravel.com/docs/12.x/configuration#configuration-caching
            let is_config_file = file_path.to_string_lossy().contains("/config/");
            if !is_config_file && !patterns.env_refs.is_empty() {
                for env_ref in &patterns.env_refs {
                    let diagnostic = Diagnostic {
                        range: Range {
                            start: Position {
                                line: env_ref.line,
                                character: env_ref.column,
                            },
                            end: Position {
                                line: env_ref.line,
                                character: env_ref.end_column,
                            },
                        },
                        severity: Some(DiagnosticSeverity::WARNING),
                        code: None,
                        source: Some("laravel".to_string()),
                        message: format!(
                            "Avoid using env() outside of config files.\n\n\
                            When config is cached (`php artisan config:cache`), the .env file \
                            is not loaded and env() will return null.\n\n\
                            Instead, use config() to access this value:\n\
                            config('your_config.{}')",
                            env_ref.name.to_lowercase()
                        ),
                        related_information: None,
                        tags: None,
                        code_description: None,
                        data: None,
                    };
                    diagnostics.push(diagnostic);
                }
            }

            // Check middleware calls using Salsa patterns - warn about undefined middleware or missing class files
            let root_guard = self.root_path.read().await;
            if let Some(root) = root_guard.as_ref() {
                for mw_ref in &patterns.middleware_refs {
                    let middleware_name = &mw_ref.name;

                    // Check if middleware exists in cache or Salsa registry
                    debug!(
                        "Checking middleware '{}' in cache/registry",
                        middleware_name
                    );
                    if let Some((class_name, class_file, _source_file, _source_line)) =
                        self.get_cached_middleware(middleware_name).await
                    {
                        debug!(
                            "Middleware '{}' found, class: {}",
                            middleware_name, class_name
                        );
                        // Middleware is in registry - check if class file exists
                        if let Some(ref mw_class_path) = class_file {
                            debug!(
                                "Checking class file: {:?}, exists: {}",
                                mw_class_path,
                                mw_class_path.exists()
                            );
                            if !mw_class_path.exists() {
                                // ERROR - middleware defined but class file missing (will crash at runtime)
                                debug!("Creating ERROR diagnostic for missing middleware class file: {}", middleware_name);
                                let diagnostic = Diagnostic {
                                    range: Range {
                                        start: Position {
                                            line: mw_ref.line,
                                            character: mw_ref.column,
                                        },
                                        end: Position {
                                            line: mw_ref.line,
                                            character: mw_ref.end_column,
                                        },
                                    },
                                    severity: Some(DiagnosticSeverity::ERROR),
                                    code: None,
                                    source: Some("laravel".to_string()),
                                    message: format!(
                                        "Middleware '{}' not found\nClass: {}\nExpected at: {}\n\nThe middleware alias is registered but the class file doesn't exist.\n💡 Click to view where the alias is defined.",
                                        middleware_name,
                                        class_name,
                                        mw_class_path.to_string_lossy()
                                    ),
                                    related_information: None,
                                    tags: None,
                                    code_description: None,
                                    data: None,
                                };
                                diagnostics.push(diagnostic);
                            } else {
                                debug!(
                                    "Middleware '{}' class file exists at {:?}",
                                    middleware_name, mw_class_path
                                );
                            }
                        } else {
                            debug!("Middleware '{}' in registry but no class_file resolved - skipping diagnostic", middleware_name);
                            // Skip diagnostic - can't verify file existence without a path
                            // This handles some framework middleware
                        }
                    } else {
                        // Middleware not in registry - try to resolve it by convention
                        info!("Laravel LSP: Middleware '{}' NOT found in registry, attempting resolution by convention", middleware_name);

                        // Strip parameters (e.g., 'auth:sanctum' -> 'auth') before converting,
                        // otherwise PascalCase produces invalid class names like 'Auth:Sanctum'.
                        let base_alias = middleware_base_alias(middleware_name);

                        // Convert kebab-case to PascalCase (e.g., 'undefined-middleware' -> 'UndefinedMiddleware')
                        let class_name = Self::kebab_to_pascal_case(base_alias);
                        let app_class = format!("App\\Http\\Middleware\\{}", class_name);

                        // Try to resolve as App\Http\Middleware\{ClassName}
                        if let Some(mw_file_path) = resolve_class_to_file(&app_class, root) {
                            info!("Laravel LSP: Attempting to resolve middleware '{}' as class '{}' at {:?}", middleware_name, app_class, mw_file_path);

                            if !mw_file_path.exists() {
                                // ERROR - middleware not in config and class file doesn't exist
                                info!("Laravel LSP: Creating ERROR diagnostic for unresolved middleware: {}", middleware_name);
                                let diagnostic = Diagnostic {
                                    range: Range {
                                        start: Position {
                                            line: mw_ref.line,
                                            character: mw_ref.column,
                                        },
                                        end: Position {
                                            line: mw_ref.line,
                                            character: mw_ref.end_column,
                                        },
                                    },
                                    severity: Some(DiagnosticSeverity::ERROR),
                                    code: None,
                                    source: Some("laravel".to_string()),
                                    message: format!(
                                        "Middleware '{}' not found\nExpected at: {}\n\nCreate the middleware or add an alias in bootstrap/app.php",
                                        middleware_name,
                                        mw_file_path.to_string_lossy()
                                    ),
                                    related_information: None,
                                    tags: None,
                                    code_description: None,
                                    data: None,
                                };
                                diagnostics.push(diagnostic);
                            } else {
                                info!("Laravel LSP: Middleware '{}' resolved by convention, file exists at {:?}", middleware_name, mw_file_path);
                            }
                        } else {
                            // Can't resolve - show INFO as we don't know where to check
                            info!("Laravel LSP: Middleware '{}' NOT found in registry and can't resolve file path, creating INFO diagnostic", middleware_name);
                            let diagnostic = Diagnostic {
                                range: Range {
                                    start: Position {
                                        line: mw_ref.line,
                                        character: mw_ref.column,
                                    },
                                    end: Position {
                                        line: mw_ref.line,
                                        character: mw_ref.end_column,
                                    },
                                },
                                severity: Some(DiagnosticSeverity::INFORMATION),
                                code: None,
                                source: Some("laravel".to_string()),
                                message: format!(
                                    "Middleware '{}' not found\n\nIf this middleware exists, add an alias in bootstrap/app.php",
                                    middleware_name
                                ),
                                related_information: None,
                                tags: None,
                                code_description: None,
                                data: None,
                            };
                            diagnostics.push(diagnostic);
                        }
                    }
                }
            }
            drop(root_guard);

            // Check translation calls using Salsa patterns - warn about missing translation files
            let root_guard = self.root_path.read().await;
            if let Some(root) = root_guard.as_ref() {
                for trans_ref in &patterns.translation_refs {
                    let check = Self::check_translation_file(root, &trans_ref.key);
                    if !check.exists {
                        diagnostics.push(Self::create_translation_diagnostic(
                            &trans_ref.key,
                            &check,
                            trans_ref.line,
                            trans_ref.column,
                            trans_ref.end_column,
                            DiagnosticSeverity::ERROR, // ERROR for dotted keys in PHP
                        ));
                    }
                }
            }
            drop(root_guard);

            // Check config calls using Salsa patterns - warn about missing config files
            let root_guard = self.root_path.read().await;
            if let Some(root) = root_guard.as_ref() {
                for config_ref in &patterns.config_refs {
                    let check = Self::check_config_file(root, &config_ref.key);
                    if !check.exists {
                        diagnostics.push(Self::create_config_diagnostic(
                            &config_ref.key,
                            &check,
                            config_ref.line,
                            config_ref.column,
                            config_ref.end_column,
                        ));
                    }
                }
            }
            drop(root_guard);

            // Check container binding calls using Salsa patterns - error for undefined bindings or missing class files
            let root_guard = self.root_path.read().await;
            if let Some(root) = root_guard.as_ref() {
                for binding_ref in &patterns.binding_refs {
                    // Only validate string bindings (not Class::class references)
                    // Class::class references might be auto-resolved by Laravel
                    if !binding_ref.is_class_reference {
                        let binding_name = &binding_ref.name;

                        // Check if binding exists in Salsa registry
                        if let Ok(Some(binding_data)) =
                            self.salsa.get_parsed_binding(binding_name.clone()).await
                        {
                            // Binding exists - check if the concrete class file exists
                            if let Some(ref bind_file_path) = binding_data.file_path {
                                if !bind_file_path.exists() {
                                    // ERROR - binding exists but class file is missing
                                    info!("Laravel LSP: Creating ERROR diagnostic for binding with missing class: {}", binding_name);

                                    // Build the diagnostic message with registration location
                                    let mut message = format!(
                                        "Binding '{}' registered but class file not found\nExpected class at: {}",
                                        binding_name,
                                        bind_file_path.to_string_lossy()
                                    );

                                    // Add registration location
                                    let registered_in = binding_data
                                        .source_file
                                        .file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or("service provider");
                                    message.push_str(&format!(
                                        "\n\nBound in: {}:{}",
                                        registered_in,
                                        binding_data.source_line + 1
                                    ));
                                    message.push_str(&format!(
                                        "\nConcrete class: {}",
                                        binding_data.concrete_class
                                    ));

                                    let diagnostic = Diagnostic {
                                        range: Range {
                                            start: Position {
                                                line: binding_ref.line,
                                                character: binding_ref.column,
                                            },
                                            end: Position {
                                                line: binding_ref.line,
                                                character: binding_ref.end_column,
                                            },
                                        },
                                        severity: Some(DiagnosticSeverity::ERROR),
                                        code: None,
                                        source: Some("laravel".to_string()),
                                        message,
                                        related_information: None,
                                        tags: None,
                                        code_description: None,
                                        data: None,
                                    };
                                    diagnostics.push(diagnostic);
                                }
                            }
                        } else {
                            // Binding not found - check if it's a known framework binding
                            let framework_bindings = [
                                "app",
                                "auth",
                                "auth.driver",
                                "blade.compiler",
                                "cache",
                                "cache.store",
                                "config",
                                "cookie",
                                "db",
                                "db.connection",
                                "encrypter",
                                "events",
                                "files",
                                "filesystem",
                                "filesystem.disk",
                                "hash",
                                "log",
                                "mailer",
                                "queue",
                                "queue.connection",
                                "redirect",
                                "redis",
                                "request",
                                "router",
                                "session",
                                "session.store",
                                "url",
                                "validator",
                                "view",
                            ];

                            if !framework_bindings.contains(&binding_name.as_str()) {
                                // Check if we can resolve the class by convention
                                if let Some(class_path) = resolve_class_to_file(binding_name, root)
                                {
                                    if class_path.exists() {
                                        // Class exists via convention - skip diagnostic
                                        continue;
                                    }
                                }

                                // ERROR - binding not found and not a known framework binding
                                info!("Laravel LSP: Creating ERROR diagnostic for undefined binding: {}", binding_name);
                                let diagnostic = Diagnostic {
                                    range: Range {
                                        start: Position {
                                            line: binding_ref.line,
                                            character: binding_ref.column,
                                        },
                                        end: Position {
                                            line: binding_ref.line,
                                            character: binding_ref.end_column,
                                        },
                                    },
                                    severity: Some(DiagnosticSeverity::ERROR),
                                    code: None,
                                    source: Some("laravel".to_string()),
                                    message: format!(
                                        "Container binding '{}' not found\n\nDefine this binding in a service provider's register() method",
                                        binding_name
                                    ),
                                    related_information: None,
                                    tags: None,
                                    code_description: None,
                                    data: None,
                                };
                                diagnostics.push(diagnostic);
                            }
                        }
                    }
                }
            }
            drop(root_guard);

            // Check asset() and related helper calls - error if file not found
            let root_guard = self.root_path.read().await;
            if let Some(root) = root_guard.as_ref() {
                for asset_ref in &patterns.asset_refs {
                    use laravel_lsp::salsa_impl::AssetHelperType;

                    // Determine base path based on helper type
                    let (base_path, helper_name) = match asset_ref.helper_type {
                        AssetHelperType::Asset => (root.join("public"), "asset"),
                        AssetHelperType::PublicPath => (root.join("public"), "public_path"),
                        AssetHelperType::Mix => (root.join("public"), "mix"),
                        AssetHelperType::BasePath => (root.clone(), "base_path"),
                        AssetHelperType::AppPath => (root.join("app"), "app_path"),
                        AssetHelperType::StoragePath => (root.join("storage"), "storage_path"),
                        AssetHelperType::DatabasePath => (root.join("database"), "database_path"),
                        AssetHelperType::LangPath => (root.join("lang"), "lang_path"),
                        AssetHelperType::ConfigPath => (root.join("config"), "config_path"),
                        AssetHelperType::ResourcePath => (root.join("resources"), "resource_path"),
                        AssetHelperType::ViteAsset => (root.join("resources"), "@vite"),
                    };

                    let asset_path = base_path.join(&asset_ref.path);

                    if !asset_path.exists() {
                        let diagnostic = Diagnostic {
                            range: Range {
                                start: Position {
                                    line: asset_ref.line,
                                    character: asset_ref.column,
                                },
                                end: Position {
                                    line: asset_ref.line,
                                    character: asset_ref.end_column,
                                },
                            },
                            severity: Some(DiagnosticSeverity::ERROR),
                            code: None,
                            source: Some("laravel".to_string()),
                            message: format!(
                                "Asset file not found: '{}'\nExpected at: {}\nHelper: {}()",
                                asset_ref.path,
                                asset_path.to_string_lossy(),
                                helper_name
                            ),
                            related_information: None,
                            tags: None,
                            code_description: None,
                            data: None,
                        };
                        diagnostics.push(diagnostic);
                    }
                }
            }
            drop(root_guard);

            // Check Laravel Pennant feature calls - error if feature class not found
            let root_guard = self.root_path.read().await;
            if let Some(root) = root_guard.as_ref() {
                // Get all existing feature classes for comparison
                let existing_features: std::collections::HashSet<String> =
                    scan_feature_classes(root)
                        .into_iter()
                        .map(|f| f.feature_key)
                        .collect();

                for feature_ref in &patterns.feature_refs {
                    // Skip class references (they're resolved differently)
                    if feature_ref.is_class_reference {
                        // For class references, check if the class file exists
                        if let Some(class_path) =
                            resolve_class_to_file(&feature_ref.feature_name, root)
                        {
                            if !class_path.exists() {
                                let diagnostic = Diagnostic {
                                    range: Range {
                                        start: Position {
                                            line: feature_ref.line,
                                            character: feature_ref.column,
                                        },
                                        end: Position {
                                            line: feature_ref.line,
                                            character: feature_ref.end_column,
                                        },
                                    },
                                    severity: Some(DiagnosticSeverity::ERROR),
                                    code: None,
                                    source: Some("laravel".to_string()),
                                    message: format!(
                                        "Feature class not found: '{}'\nExpected at: {}",
                                        feature_ref.feature_name,
                                        class_path.to_string_lossy()
                                    ),
                                    related_information: None,
                                    tags: None,
                                    code_description: None,
                                    data: None,
                                };
                                diagnostics.push(diagnostic);
                            }
                        }
                    } else {
                        // For string references, check if the feature key exists
                        if !existing_features.contains(&feature_ref.feature_name) {
                            let expected_class =
                                feature_key_to_class_name(&feature_ref.feature_name);
                            let expected_path = root
                                .join("app/Features")
                                .join(format!("{}.php", expected_class));

                            let diagnostic = Diagnostic {
                                range: Range {
                                    start: Position {
                                        line: feature_ref.line,
                                        character: feature_ref.column,
                                    },
                                    end: Position {
                                        line: feature_ref.line,
                                        character: feature_ref.end_column,
                                    },
                                },
                                severity: Some(DiagnosticSeverity::ERROR),
                                code: None,
                                source: Some("laravel".to_string()),
                                message: format!(
                                    "Feature not found: '{}'\nExpected at: {}",
                                    feature_ref.feature_name,
                                    expected_path.to_string_lossy()
                                ),
                                related_information: None,
                                tags: None,
                                code_description: None,
                                data: None,
                            };
                            diagnostics.push(diagnostic);
                        }
                    }
                }
            }
            drop(root_guard);

            // Validate validation rules in PHP files
            let validation_diagnostics = self.validate_validation_rules(source).await;
            diagnostics.extend(validation_diagnostics);

            // Store and publish diagnostics for PHP files
            self.diagnostics
                .write()
                .await
                .insert(uri.clone(), diagnostics.clone());
            self.client
                .publish_diagnostics(uri.clone(), diagnostics, None)
                .await;
            return;
        }

        // =====================================================================
        // Blade file validation - uses Salsa patterns (already parsed above)
        // =====================================================================
        if !is_blade {
            return;
        }

        // Translation calls are already extracted by Salsa (patterns.translation_refs)
        // Check translation calls in Blade files (includes {{ __() }} syntax)
        let root_guard = self.root_path.read().await;
        if let Some(root) = root_guard.as_ref() {
            for trans_ref in &patterns.translation_refs {
                let check = Self::check_translation_file(root, &trans_ref.key);
                if !check.exists {
                    diagnostics.push(Self::create_translation_diagnostic(
                        &trans_ref.key,
                        &check,
                        trans_ref.line,
                        trans_ref.column,
                        trans_ref.end_column,
                        DiagnosticSeverity::ERROR, // ERROR for dotted keys in Blade __()
                    ));
                }
            }
        }
        drop(root_guard);

        // Check @extends and @include directives using Salsa patterns
        for dir_ref in &patterns.directives {
            // Only validate @extends and @include
            if dir_ref.name == "extends" || dir_ref.name == "include" {
                if let Some(ref args) = dir_ref.arguments {
                    if let Some(view_name) = Self::extract_view_from_directive_args(args) {
                        let possible_paths = config.resolve_view_path(&view_name);

                        // Check if ANY of the possible paths exist
                        let exists = possible_paths.iter().any(|p| p.exists());

                        if !exists {
                            // Use the first path for the diagnostic message
                            let expected_path = possible_paths
                                .first()
                                .map(|p| p.to_string_lossy().to_string())
                                .unwrap_or_else(|| "unknown".to_string());

                            let diagnostic = Diagnostic {
                                range: Range {
                                    start: Position {
                                        line: dir_ref.line,
                                        character: dir_ref.column,
                                    },
                                    end: Position {
                                        line: dir_ref.line,
                                        character: dir_ref.end_column,
                                    },
                                },
                                severity: Some(DiagnosticSeverity::ERROR),
                                code: None,
                                source: Some("laravel".to_string()),
                                message: format!(
                                    "View file not found: '{}'\nExpected at: {}",
                                    view_name, expected_path
                                ),
                                related_information: None,
                                tags: None,
                                code_description: None,
                                data: None,
                            };
                            diagnostics.push(diagnostic);
                        }
                    }
                }
            }
        }

        // Check Blade components (<x-button>) using Salsa patterns. A
        // component is considered to exist if EITHER a conventional view
        // file (`resources/views/components/{name}.blade.php`) OR a class
        // file (`app/View/Components/{Pascal}.php`) is on disk. The class-
        // only case covers components like `<x-app-layout>` whose
        // `render()` method returns a view at a non-conventional path
        // (`view('layouts.app')`) — common in Laravel Breeze/Jetstream
        // starter kits.
        let root_for_components = self.root_path.read().await;
        for comp_ref in &patterns.components {
            let possible_paths = config.resolve_component_path(&comp_ref.name);
            let view_exists = possible_paths.iter().any(|p| p.exists());

            let class_path =
                laravel_lsp::component_declaration_locator::conventional_class_file_path(
                    &comp_ref.name,
                    &config,
                );
            let class_exists = class_path.is_file();

            if !view_exists && !class_exists {
                // Neither view nor class exists — surface as "not found"
                // so the user gets a Create Missing View / Create Missing
                // Component code action.
                let expected_path = possible_paths
                    .first()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| "unknown".to_string());

                let diagnostic = Diagnostic {
                    range: Range {
                        start: Position {
                            line: comp_ref.line,
                            character: comp_ref.column,
                        },
                        end: Position {
                            line: comp_ref.line,
                            character: comp_ref.end_column,
                        },
                    },
                    severity: Some(DiagnosticSeverity::ERROR),
                    code: None,
                    source: Some("laravel".to_string()),
                    message: format!(
                        "Blade component not found: '{}'\nExpected at: {}",
                        comp_ref.name, expected_path
                    ),
                    related_information: None,
                    tags: None,
                    code_description: None,
                    data: None,
                };
                diagnostics.push(diagnostic);
            }
        }
        drop(root_for_components);

        // Check Livewire components using Salsa patterns. The resolver
        // routes through three layers in order:
        //   1. The new Livewire resolver (v4 SFC/MFC, V3 Class, Volt).
        //   2. Fallback to view-path resolution (matches the goto
        //      directive handler — finds `<livewire:...>` and `@livewire(...)`
        //      components whose view lives at a non-conventional path,
        //      typically because the class is vendor-registered like
        //      Jetstream's published components at `resources/views/api/`).
        //   3. Only after BOTH miss do we publish the "not found" diagnostic.
        for lw_ref in &patterns.livewire_refs {
            let has_livewire_kind = self
                .resolve_livewire_component(&lw_ref.name)
                .await
                .is_some();
            let view_exists = if has_livewire_kind {
                true
            } else {
                config
                    .resolve_view_path(&lw_ref.name)
                    .iter()
                    .any(|p| p.exists())
            };
            if !view_exists {
                let diagnostic = Diagnostic {
                    range: Range {
                        start: Position {
                            line: lw_ref.line,
                            character: lw_ref.column,
                        },
                        end: Position {
                            line: lw_ref.line,
                            character: lw_ref.end_column,
                        },
                    },
                    severity: Some(DiagnosticSeverity::ERROR),
                    code: None,
                    source: Some("laravel".to_string()),
                    message: format!("Livewire component not found: '{}'", lw_ref.name),
                    related_information: None,
                    tags: None,
                    code_description: None,
                    data: None,
                };
                diagnostics.push(diagnostic);
            }
        }

        // Check @lang directives for translation files using Salsa patterns
        let root_guard = self.root_path.read().await;
        if let Some(root) = root_guard.as_ref() {
            for dir_ref in &patterns.directives {
                // Only validate @lang directives
                if dir_ref.name == "lang" {
                    if let Some(ref args) = dir_ref.arguments {
                        if let Some(translation_key) = Self::extract_view_from_directive_args(args)
                        {
                            let check = Self::check_translation_file(root, &translation_key);
                            if !check.exists {
                                diagnostics.push(Self::create_translation_diagnostic(
                                    &translation_key,
                                    &check,
                                    dir_ref.line,
                                    dir_ref.column,
                                    dir_ref.end_column,
                                    DiagnosticSeverity::WARNING, // WARNING for dotted keys in @lang
                                ));
                            }
                        }
                    }
                }
            }

            // Check @feature directives for Laravel Pennant feature classes
            // Build a map of feature keys to their actual file paths (supports custom $name properties)
            let feature_map: std::collections::HashMap<String, PathBuf> =
                scan_feature_classes(root)
                    .into_iter()
                    .map(|f| {
                        let file_path = root
                            .join("app/Features")
                            .join(format!("{}.php", f.class_name));
                        (f.feature_key, file_path)
                    })
                    .collect();

            for dir_ref in &patterns.directives {
                if dir_ref.name == "feature" {
                    if let Some(ref args) = dir_ref.arguments {
                        if let Some(feature_name) = Self::extract_view_from_directive_args(args) {
                            // Check if feature key exists in scanned features (includes custom $name)
                            if !feature_map.contains_key(&feature_name) {
                                // Feature not found - show expected path based on derived class name
                                let class_name = feature_key_to_class_name(&feature_name);
                                let feature_path =
                                    root.join(format!("app/Features/{}.php", class_name));

                                // Use pre-calculated string_column/string_end_column from Salsa
                                // These point to the content INSIDE the quotes (the feature name)
                                let diagnostic = Diagnostic {
                                    range: Range {
                                        start: Position {
                                            line: dir_ref.line,
                                            character: dir_ref.string_column,
                                        },
                                        end: Position {
                                            line: dir_ref.line,
                                            character: dir_ref.string_end_column,
                                        },
                                    },
                                    severity: Some(DiagnosticSeverity::ERROR),
                                    code: None,
                                    source: Some("laravel".to_string()),
                                    message: format!(
                                        "Feature not found: '{}'\nExpected at: {}",
                                        feature_name,
                                        feature_path.to_string_lossy()
                                    ),
                                    related_information: None,
                                    tags: None,
                                    code_description: None,
                                    data: None,
                                };
                                diagnostics.push(diagnostic);
                            }
                        }
                    }
                }
            }
        }
        drop(root_guard);

        // Check @vite and asset() calls in Blade files - error if file not found
        let root_guard = self.root_path.read().await;
        if let Some(root) = root_guard.as_ref() {
            for asset_ref in &patterns.asset_refs {
                use laravel_lsp::salsa_impl::AssetHelperType;

                // Determine base path based on helper type
                let (base_path, helper_name) = match asset_ref.helper_type {
                    AssetHelperType::Asset => (root.join("public"), "asset"),
                    AssetHelperType::PublicPath => (root.join("public"), "public_path"),
                    AssetHelperType::Mix => (root.join("public"), "mix"),
                    AssetHelperType::BasePath => (root.clone(), "base_path"),
                    AssetHelperType::AppPath => (root.join("app"), "app_path"),
                    AssetHelperType::StoragePath => (root.join("storage"), "storage_path"),
                    AssetHelperType::DatabasePath => (root.join("database"), "database_path"),
                    AssetHelperType::LangPath => (root.join("lang"), "lang_path"),
                    AssetHelperType::ConfigPath => (root.join("config"), "config_path"),
                    AssetHelperType::ResourcePath => (root.join("resources"), "resource_path"),
                    AssetHelperType::ViteAsset => (root.join("resources"), "@vite"),
                };

                let asset_path = base_path.join(&asset_ref.path);

                if !asset_path.exists() {
                    let diagnostic = Diagnostic {
                        range: Range {
                            start: Position {
                                line: asset_ref.line,
                                character: asset_ref.column,
                            },
                            end: Position {
                                line: asset_ref.line,
                                character: asset_ref.end_column,
                            },
                        },
                        severity: Some(DiagnosticSeverity::WARNING),
                        code: None,
                        source: Some("laravel".to_string()),
                        message: format!(
                            "Asset file not found: '{}'\nExpected at: {}\nHelper: {}()",
                            asset_ref.path,
                            asset_path.to_string_lossy(),
                            helper_name
                        ),
                        related_information: None,
                        tags: None,
                        code_description: None,
                        data: None,
                    };
                    diagnostics.push(diagnostic);
                }
            }
        }
        drop(root_guard);

        // Check for unresolved variable property accesses in Blade files
        // This warns about variables like $user-> where the type cannot be determined
        let variable_accesses = Self::extract_blade_variable_accesses(source);
        let mut seen_variables: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        for access in variable_accesses {
            // Only report once per variable name to avoid spam
            if seen_variables.contains(&access.variable_name) {
                continue;
            }

            // Try to resolve the variable type
            let resolved_type = self
                .resolve_blade_variable_type(uri, &access.variable_name)
                .await;

            if resolved_type.is_none() {
                seen_variables.insert(access.variable_name.clone());

                let diagnostic = Diagnostic {
                    range: Range {
                        start: Position {
                            line: access.line,
                            character: access.column,
                        },
                        end: Position {
                            line: access.line,
                            character: access.end_column,
                        },
                    },
                    severity: Some(DiagnosticSeverity::HINT),
                    code: None,
                    source: Some("laravel".to_string()),
                    message: format!(
                        "Cannot resolve type for '{}'\n\nTo enable autocomplete, ensure the variable is passed from:\n- Controller with return view('...', compact('{}'))\n- Livewire component with public property or #[Computed] method\n- View component with constructor parameter\n- @props directive with type hint\n- @foreach loop where the iterable's type carries a generic element (e.g. `@return Collection<int, Audit>`)",
                        access.variable_name,
                        access.variable_name.trim_start_matches('$')
                    ),
                    related_information: None,
                    tags: None,
                    code_description: None,
                    data: None,
                };
                diagnostics.push(diagnostic);
            }
        }

        // Store diagnostics for hover filtering
        self.diagnostics
            .write()
            .await
            .insert(uri.clone(), diagnostics.clone());

        // Publish diagnostics
        info!(
            "   📤 Publishing {} diagnostics to client",
            diagnostics.len()
        );
        self.client
            .publish_diagnostics(uri.clone(), diagnostics, None)
            .await;
        info!("   ✅ Diagnostics published successfully");
    }

    /// Resolve a [`HoverTarget`] into the markdown body that should appear in
    /// the editor's hover tooltip. Each pattern branch performs its own data
    /// resolution (file existence, cached config values, env vars, route
    /// metadata, etc.) and then hands the resolved data to a pure formatter
    /// in [`laravel_lsp::hover`].
    ///
    /// Returns `None` when neither the cursor's pattern nor the variable
    /// fallback produced something worth displaying.
    async fn build_hover_markdown(
        &self,
        target: laravel_lsp::hover::HoverTarget,
        uri: &Url,
    ) -> Option<String> {
        use laravel_lsp::hover;
        use laravel_lsp::salsa_impl::PatternAtPosition;

        let root = self.root_path.read().await.clone();

        let pattern = match target {
            hover::HoverTarget::Pattern(p) => p,
            hover::HoverTarget::BladeVariable { var_name, property } => {
                return self
                    .build_blade_variable_hover_content(uri, &var_name, property)
                    .await;
            }
        };

        // Each arm builds a HoverContent for the unified template. Per-pattern
        // work is purely data fetching — the rendering is identical across
        // all patterns and lives in `hover::render`.
        let rendered = match pattern {
            PatternAtPosition::View(view) => self.hover_for_view(&view.name).await,
            // Pass `comp.name` (bare, without `x-` prefix) — `tag_name`
            // includes the prefix which would break path resolution.
            PatternAtPosition::Component(comp) => self.hover_for_component(&comp.name).await,
            PatternAtPosition::Livewire(lw) => self.hover_for_livewire(&lw.name).await,
            PatternAtPosition::Route(route) => self.hover_for_route(&route.name).await,
            PatternAtPosition::ConfigRef(cfg) => {
                self.hover_for_config(&cfg.key, root.as_deref()).await
            }
            PatternAtPosition::EnvRef(env) => self.hover_for_env(&env.name).await,
            PatternAtPosition::Translation(trans) => {
                self.hover_for_translation(&trans.key, root.as_deref())
                    .await
            }
            PatternAtPosition::Middleware(mw) => self.hover_for_middleware(&mw.name).await,
            PatternAtPosition::Binding(binding) => self.hover_for_binding(&binding.name).await,
            PatternAtPosition::Asset(asset) => self.hover_for_asset(&asset).await,
            PatternAtPosition::Url(url) => self.hover_for_url(&url.path).await,
            // Patterns we don't yet surface in hover — silently drop.
            PatternAtPosition::Directive(_)
            | PatternAtPosition::Action(_)
            | PatternAtPosition::Feature(_) => return None,
        };

        if rendered.is_empty() {
            None
        } else {
            Some(rendered)
        }
    }

    /// View — `@props([...])` snippet + resolved file link.
    async fn hover_for_view(&self, name: &str) -> String {
        use laravel_lsp::hover;
        let path = self.resolve_view_file(name).await;
        let link = match &path {
            Some(p) => Some(self.source_link(p, None).await),
            None => None,
        };
        let snippet = path
            .as_deref()
            .and_then(laravel_lsp::blade_props::extract_props_directive);
        let trailer = if link.is_none() {
            Some("*(file not found)*")
        } else {
            None
        };
        hover::render(&hover::HoverContent {
            code: snippet.as_deref().map(|s| hover::CodeBlock {
                language: hover::CodeLanguage::Php,
                content: s,
            }),
            source_link: link.as_deref(),
            trailer,
            ..Default::default()
        })
    }

    /// Blade component — distinguishes class-backed components (Laravel
    /// renders these via an `app/View/Components/<Pascal>.php` class) from
    /// anonymous Blade components (just a `.blade.php` template).
    ///
    /// - **Class-backed** → class FQN as header + `class Foo extends
    ///   Component` signature snippet + link to the class file.
    /// - **Anonymous** → no header + `@props([...])` snippet from the
    ///   Blade file + link to that file.
    ///
    /// Class detection: look for `app/View/Components/<PascalCase(name)>.php`
    /// on disk. Dots in the component name become path separators, kebab
    /// segments get pascal-cased — `<x-forms.input-text>` →
    /// `app/View/Components/Forms/InputText.php`.
    async fn hover_for_component(&self, name: &str) -> String {
        use laravel_lsp::hover;
        let class_path = self.resolve_component_class_file(name).await;
        let blade_path = self.resolve_component_file(name).await;

        // Class-backed wins when its file exists — Laravel resolves
        // class-backed components first at runtime too.
        if let Some(ref class_file) = class_path {
            let class_fqn = laravel_lsp::php_class::extract_class_fqn(class_file);
            let snippet = laravel_lsp::php_class::extract_class_signature(class_file);
            let link = self.source_link(class_file, None).await;
            return hover::render(&hover::HoverContent {
                header: class_fqn.as_deref(),
                code: snippet.as_deref().map(|s| hover::CodeBlock {
                    language: hover::CodeLanguage::Php,
                    content: s,
                }),
                source_link: Some(&link),
                ..Default::default()
            });
        }

        // Fall through to anonymous-component shape.
        let link = match &blade_path {
            Some(p) => Some(self.source_link(p, None).await),
            None => None,
        };
        let snippet = blade_path
            .as_deref()
            .and_then(laravel_lsp::blade_props::extract_props_directive);
        let trailer = if link.is_none() {
            Some("*(file not found)*")
        } else {
            None
        };
        hover::render(&hover::HoverContent {
            code: snippet.as_deref().map(|s| hover::CodeBlock {
                language: hover::CodeLanguage::Php,
                content: s,
            }),
            source_link: link.as_deref(),
            trailer,
            ..Default::default()
        })
    }

    /// Look up the conventional class-backed component file for a tag name.
    /// Returns `app/View/Components/<PascalCase(name)>.php` if it exists on
    /// disk, otherwise `None`. Handles dotted names (`forms.input` →
    /// `Forms/Input.php`) and kebab segments (`alert-box` → `AlertBox.php`).
    async fn resolve_component_class_file(&self, name: &str) -> Option<PathBuf> {
        let root = self.root_path.read().await.clone()?;
        let class_path = name
            .split('.')
            .map(laravel_lsp::config::kebab_to_pascal_case)
            .collect::<Vec<_>>()
            .join("/");
        let candidate = root
            .join("app/View/Components")
            .join(format!("{}.php", class_path));
        if self.file_exists_cached(&candidate).await {
            Some(candidate)
        } else {
            None
        }
    }

    /// Livewire — class FQN header, class signature snippet, file link.
    async fn hover_for_livewire(&self, name: &str) -> String {
        use laravel_lsp::hover;
        let path = self.resolve_livewire_file(name).await;
        let link = match &path {
            Some(p) => Some(self.source_link(p, None).await),
            None => None,
        };
        let snippet = path
            .as_deref()
            .and_then(laravel_lsp::php_class::extract_class_signature);
        let class_fqn = path
            .as_deref()
            .and_then(laravel_lsp::php_class::extract_class_fqn);
        let trailer = if link.is_none() {
            Some("*(file not found)*")
        } else {
            None
        };
        hover::render(&hover::HoverContent {
            header: class_fqn.as_deref(),
            code: snippet.as_deref().map(|s| hover::CodeBlock {
                language: hover::CodeLanguage::Php,
                content: s,
            }),
            source_link: link.as_deref(),
            trailer,
            ..Default::default()
        })
    }

    /// Route — detail line (verb + URI + action) + the route registration
    /// line from source + link to the `->name(` callsite.
    async fn hover_for_route(&self, name: &str) -> String {
        use laravel_lsp::hover;
        let idx_guard = self.route_index.read().await;
        let def = idx_guard.as_ref().and_then(|idx| idx.get(name));
        let Some(def) = def else {
            return hover::render(&hover::HoverContent {
                trailer: Some("*(route not found in index)*"),
                ..Default::default()
            });
        };
        let method = def
            .method
            .as_deref()
            .map(|m| m.to_uppercase())
            .unwrap_or_else(|| "?".to_string());
        let uri = def.uri.as_deref().unwrap_or("?");
        let action = def.action.as_deref().unwrap_or("?");
        let detail = format!("`{} {}` → `{}`", method, uri, action);
        let snippet = laravel_lsp::php_class::read_line_from_file(&def.file, def.line);
        let link = self.source_link(&def.file, Some(def.line + 1)).await;
        // Drop the lock on route_index before awaiting other things in render.
        drop(idx_guard);
        hover::render(&hover::HoverContent {
            detail: Some(&detail),
            code: snippet.as_deref().map(|s| hover::CodeBlock {
                language: hover::CodeLanguage::Php,
                content: s,
            }),
            source_link: Some(&link),
            ..Default::default()
        })
    }

    /// Config — resolved value as a `php` code block, link to the
    /// `config/<group>.php` file.
    async fn hover_for_config(&self, key: &str, root: Option<&Path>) -> String {
        use laravel_lsp::hover;
        let value = root.and_then(|r| laravel_lsp::config_lookup::resolve_value(r, key));
        let link = match (root, key.split('.').next()) {
            (Some(r), Some(group)) => {
                let path = r.join("config").join(format!("{}.php", group));
                Some(self.source_link(&path, None).await)
            }
            _ => None,
        };
        let truncated = value
            .as_deref()
            .map(|v| laravel_lsp::hover::truncate_for_display(v, 200));
        let trailer = if value.is_none() {
            Some("*(value not found)*")
        } else {
            None
        };
        hover::render(&hover::HoverContent {
            code: truncated.as_deref().map(|s| hover::CodeBlock {
                language: hover::CodeLanguage::Php,
                content: s,
            }),
            source_link: link.as_deref(),
            trailer,
            ..Default::default()
        })
    }

    /// Env — value as a plain code block, link to the `.env` file it was
    /// read from. Commented-out entries render as a detail note.
    async fn hover_for_env(&self, name: &str) -> String {
        use laravel_lsp::hover;
        let var = self
            .salsa
            .get_parsed_env_var(name.to_string())
            .await
            .ok()
            .flatten();
        let Some(var) = var else {
            return hover::render(&hover::HoverContent {
                trailer: Some("*(not defined in .env)*"),
                ..Default::default()
            });
        };
        let link = self.source_link(&var.source_file, None).await;
        if var.is_commented {
            hover::render(&hover::HoverContent {
                detail: Some("*(commented out)*"),
                source_link: Some(&link),
                ..Default::default()
            })
        } else {
            hover::render(&hover::HoverContent {
                code: Some(hover::CodeBlock {
                    language: hover::CodeLanguage::Plain,
                    content: &var.value,
                }),
                source_link: Some(&link),
                ..Default::default()
            })
        }
    }

    /// Translation — resolved value (with outer quotes stripped) as a plain
    /// code block, link to the lang file.
    async fn hover_for_translation(&self, key: &str, root: Option<&Path>) -> String {
        use laravel_lsp::hover;
        let resolution = match root {
            Some(r) => {
                let vendor_map = self.vendor_translation_namespaces_for(r).await;
                let map_ref = vendor_map.as_ref().map(|m| m.as_ref());
                laravel_lsp::translation_lookup::resolve_translation_detailed(r, key, "en", map_ref)
            }
            None => None,
        };
        let link = match &resolution {
            Some(res) => Some(self.source_link(&res.source_file, None).await),
            None => None,
        };
        // Translation values are PHP literals (`'foo'`) — strip the outer
        // quotes for nicer in-block display.
        let unquoted = resolution.as_ref().map(|r| {
            let v = r.value.trim();
            v.strip_prefix('\'')
                .and_then(|s| s.strip_suffix('\''))
                .or_else(|| v.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
                .unwrap_or(v)
                .to_string()
        });
        let truncated = unquoted
            .as_deref()
            .map(|s| laravel_lsp::hover::truncate_for_display(s, 200));
        let trailer = if resolution.is_none() {
            Some("*(translation not found for default locale)*")
        } else {
            None
        };
        hover::render(&hover::HoverContent {
            code: truncated.as_deref().map(|s| hover::CodeBlock {
                language: hover::CodeLanguage::Plain,
                content: s,
            }),
            source_link: link.as_deref(),
            trailer,
            ..Default::default()
        })
    }

    /// Middleware — header is the alias's class FQN (the new info beyond
    /// the cursor's `'auth'` string).
    async fn hover_for_middleware(&self, name: &str) -> String {
        use laravel_lsp::hover;
        let cached = self.get_cached_middleware(name).await;
        let class_fqn = cached.as_ref().map(|(class, _, _, _)| class.as_str());
        let source_path = cached
            .as_ref()
            .and_then(|(_, _, source_file, _)| source_file.as_ref());
        let link = match source_path {
            Some(p) => Some(self.source_link(p, None).await),
            None => None,
        };
        let trailer = if class_fqn.is_none() {
            Some("*(alias not registered)*")
        } else {
            None
        };
        hover::render(&hover::HoverContent {
            header: class_fqn,
            source_link: link.as_deref(),
            trailer,
            ..Default::default()
        })
    }

    /// Container binding — header is the concrete class FQN bound to the
    /// alias.
    async fn hover_for_binding(&self, name: &str) -> String {
        use laravel_lsp::hover;
        let cached = self.get_cached_binding(name).await;
        let class_fqn = cached.as_ref().map(|(class, _, _, _)| class.as_str());
        let source_path = cached
            .as_ref()
            .and_then(|(_, _, source_file, _)| source_file.as_ref());
        let link = match source_path {
            Some(p) => Some(self.source_link(p, None).await),
            None => None,
        };
        let trailer = if class_fqn.is_none() {
            Some("*(binding not registered)*")
        } else {
            None
        };
        hover::render(&hover::HoverContent {
            header: class_fqn,
            source_link: link.as_deref(),
            trailer,
            ..Default::default()
        })
    }

    /// Asset (`asset()`, `Vite::asset()`, `mix()`, `public_path()`, etc.) —
    /// just the resolved file link.
    async fn hover_for_asset(&self, asset: &laravel_lsp::salsa_impl::AssetReferenceData) -> String {
        use laravel_lsp::hover;
        let resolved = self.resolve_display_path_for_asset(asset).await;
        let trailer = if resolved.is_none() {
            Some("*(file not found)*")
        } else {
            None
        };
        hover::render(&hover::HoverContent {
            source_link: resolved.as_deref(),
            trailer,
            ..Default::default()
        })
    }

    /// `url('/path')` — same shape as asset.
    async fn hover_for_url(&self, url_path: &str) -> String {
        use laravel_lsp::hover;
        let resolved = self.resolve_display_path_for_url(url_path).await;
        let trailer = if resolved.is_none() {
            Some("*(file not found)*")
        } else {
            None
        };
        hover::render(&hover::HoverContent {
            source_link: resolved.as_deref(),
            trailer,
            ..Default::default()
        })
    }

    /// Map a Salsa `AssetHelperType` to the function-call form a developer
    /// would have literally written. No current caller (asset hovers don't
    /// surface the helper label anymore now that the unified template
    /// dropped the call-form header) but kept available for future use —
    /// e.g. a completion item that wants to render the helper name.
    #[allow(dead_code)]
    fn asset_helper_label(t: laravel_lsp::salsa_impl::AssetHelperType) -> &'static str {
        use laravel_lsp::salsa_impl::AssetHelperType;
        match t {
            AssetHelperType::Asset => "asset",
            AssetHelperType::PublicPath => "public_path",
            AssetHelperType::Mix => "mix",
            AssetHelperType::BasePath => "base_path",
            AssetHelperType::AppPath => "app_path",
            AssetHelperType::StoragePath => "storage_path",
            AssetHelperType::DatabasePath => "database_path",
            AssetHelperType::LangPath => "lang_path",
            AssetHelperType::ConfigPath => "config_path",
            AssetHelperType::ResourcePath => "resource_path",
            AssetHelperType::ViteAsset => "Vite::asset",
        }
    }

    /// Resolve an asset reference to its on-disk file path. Mirrors the
    /// directory mapping used by `create_asset_location_from_salsa` for
    /// goto-definition — `asset()`/`Vite::asset()` look under `public/` or
    /// `resources/` depending on which helper produced the reference.
    async fn resolve_display_path_for_asset(
        &self,
        asset: &laravel_lsp::salsa_impl::AssetReferenceData,
    ) -> Option<String> {
        use laravel_lsp::salsa_impl::AssetHelperType;
        let root = self.root_path.read().await.clone()?;
        let base = match asset.helper_type {
            AssetHelperType::Asset | AssetHelperType::PublicPath | AssetHelperType::Mix => {
                root.join("public")
            }
            AssetHelperType::BasePath => root.clone(),
            AssetHelperType::AppPath => root.join("app"),
            AssetHelperType::StoragePath => root.join("storage"),
            AssetHelperType::DatabasePath => root.join("database"),
            AssetHelperType::LangPath => root.join("lang"),
            AssetHelperType::ConfigPath => root.join("config"),
            AssetHelperType::ResourcePath | AssetHelperType::ViteAsset => root.join("resources"),
        };
        let asset_path = base.join(&asset.path);
        if self.file_exists_cached(&asset_path).await {
            Some(self.source_link(&asset_path, None).await)
        } else {
            None
        }
    }

    /// Resolve a `url('/path')` reference to a click-to-open link for the
    /// matching file under `public/`. Returns `None` when nothing matches
    /// (the URL might point to a dynamic route, not a static asset).
    async fn resolve_display_path_for_url(&self, url_path: &str) -> Option<String> {
        let root = self.root_path.read().await.clone()?;
        let path = url_path.trim_start_matches('/');
        let candidate = root.join("public").join(path);
        if self.file_exists_cached(&candidate).await {
            Some(self.source_link(&candidate, None).await)
        } else {
            None
        }
    }

    /// Resolve a Blade variable hover into rendered markdown. Builds a
    /// [`hover::HoverContent`] from:
    ///
    /// - **Variable type resolution** (existing `resolve_blade_variable_type`)
    ///   — class FQN, primitive, or `None`.
    /// - **Class file lookup** — `class_locator::find_php_class_file`.
    /// - **Property declaration + PHPDoc** — pulled from the class source so
    ///   the hover shows `public string $email;` plus any leading docblock.
    ///
    /// The bold header is set ONLY when we have a class-like FQN to qualify
    /// the variable / property with (avoids echoing `$user` or
    /// `$user->email` as a redundant header).
    async fn build_blade_variable_hover_content(
        &self,
        uri: &Url,
        var_name: &str,
        property: Option<String>,
    ) -> Option<String> {
        use laravel_lsp::hover;
        let var_dollar = format!("${}", var_name);
        let resolved_var_type = self.resolve_blade_variable_type(uri, &var_dollar).await;

        // `"mixed"` is a sentinel from the loop-variable resolver — calling
        // `find_php_class_file("mixed")` always misses, throwing away any
        // useful Blade-file fallback we could surface.
        let class_fqn_for_lookup = resolved_var_type
            .as_deref()
            .filter(|t| hover::is_class_like_type(t));

        let (class_source, class_path) = match class_fqn_for_lookup {
            Some(class_fqn) => {
                let root = self.root_path.read().await.clone();
                let path = root
                    .as_ref()
                    .and_then(|r| laravel_lsp::class_locator::find_php_class_file(class_fqn, r));
                let source = path.as_ref().and_then(|p| std::fs::read_to_string(p).ok());
                (source, path)
            }
            None => (None, None),
        };

        let declaration = match (property.as_deref(), class_source.as_deref()) {
            (Some(prop), Some(source)) => {
                laravel_lsp::php_class::extract_property_declaration(source, prop)
            }
            _ => None,
        };

        // Source-link priority:
        //   1. Class file + property line — best jump target
        //   2. Class file alone — class found but no explicit property line
        //      (Eloquent columns not in $casts/$fillable)
        //   3. Blade-file origin (`@foreach`/`@props` line) — when type
        //      didn't resolve to a class
        let link = match (class_path.as_ref(), declaration.as_ref()) {
            (Some(path), Some(decl)) => Some(self.source_link(path, Some(decl.line + 1)).await),
            (Some(path), None) => Some(self.source_link(path, None).await),
            _ => self.find_blade_variable_origin(uri, var_name).await,
        };

        // Bold header — only when we have a class FQN. Otherwise no header
        // (echoing `$var` or `$var->prop` as bold adds no info beyond the
        // cursor).
        let header_owned = match (resolved_var_type.as_deref(), property.as_deref()) {
            (Some(class), Some(prop)) if hover::is_class_like_type(class) => {
                Some(format!("{}::${}", class, prop))
            }
            (Some(class), None) if hover::is_class_like_type(class) => {
                Some(format!("${} : `{}`", var_name, class))
            }
            _ => None,
        };

        let description = declaration.as_ref().and_then(|d| d.description.clone());
        let tags: Vec<String> = declaration
            .as_ref()
            .map(|d| d.phpdoc_tags.clone())
            .unwrap_or_default();

        let rendered = hover::render(&hover::HoverContent {
            header: header_owned.as_deref(),
            description: description.as_deref(),
            code: declaration.as_ref().map(|d| hover::CodeBlock {
                language: hover::CodeLanguage::Php,
                content: &d.declaration_text,
            }),
            tags: &tags,
            source_link: link.as_deref(),
            ..Default::default()
        });
        if rendered.is_empty() {
            None
        } else {
            Some(rendered)
        }
    }

    /// Scan the current Blade file for where `var_name` was introduced —
    /// either by a loop directive (`@foreach (... as $name)`) or by a
    /// `@props([..., 'name'])` declaration. Returns a `path:line` string
    /// suitable for the hover's bottom-line source reference.
    ///
    /// Used as a fallback when the variable's *type* doesn't resolve to a
    /// findable class — e.g. loop variables with unresolved element types
    /// (the `"mixed"` sentinel). Telling the user "this variable was
    /// introduced by the `@foreach` on line 7" is more useful than an empty
    /// hover.
    async fn find_blade_variable_origin(&self, uri: &Url, var_name: &str) -> Option<String> {
        let file_path = uri.to_file_path().ok()?;

        // 1. Loop blocks (cheapest — Salsa already cached them for the resolver).
        if let Ok(Some(blocks)) = self.salsa.get_loop_blocks(file_path.clone()).await {
            for block in blocks.iter() {
                if block.variables.iter().any(|(n, _)| n == var_name) {
                    return Some(
                        self.source_link(&file_path, Some(block.start_line as u32 + 1))
                            .await,
                    );
                }
            }
        }

        // 2. `@props([..., 'name' ...])` declaration in the Blade source.
        let content = match self.documents.read().await.get(uri).cloned() {
            Some((c, _)) => c,
            None => std::fs::read_to_string(&file_path).unwrap_or_default(),
        };
        if let Some(line) = find_props_declaration_line(&content, var_name) {
            return Some(self.source_link(&file_path, Some(line + 1)).await);
        }

        None
    }

    /// Resolve a view name to its on-disk file path. Returns `None` when no
    /// candidate file exists on disk. The hover dispatch then builds the
    /// `source_link` markdown AND the `@props(...)` source snippet from the
    /// same path.
    async fn resolve_view_file(&self, name: &str) -> Option<PathBuf> {
        let config = self.get_cached_config().await?;
        for path in config.resolve_view_path(name) {
            if self.file_exists_cached(&path).await {
                return Some(path);
            }
        }
        None
    }

    /// Same shape as [`Self::resolve_view_file`] but for Blade components.
    async fn resolve_component_file(&self, name: &str) -> Option<PathBuf> {
        let config = self.get_cached_config().await?;
        for path in config.resolve_component_path(name) {
            if self.file_exists_cached(&path).await {
                return Some(path);
            }
        }
        None
    }

    /// Same shape as [`Self::resolve_view_file`] but for Livewire components.
    /// Routed through the new resolver so all four shapes (V4 SFC, V4 MFC,
    /// V3 Class, Volt) are discovered, not just the legacy class path.
    async fn resolve_livewire_file(&self, name: &str) -> Option<PathBuf> {
        self.resolve_livewire_primary_path(name).await
    }

    /// Render a path as a string relative to the project root when possible.
    /// Falls back to the absolute path string when no root is set or when the
    /// path lies outside the root (vendor/, package directories).
    async fn relative_display_path(&self, path: &Path) -> String {
        if let Some(root) = self.root_path.read().await.as_ref() {
            if let Ok(rel) = path.strip_prefix(root) {
                return rel.to_string_lossy().to_string();
            }
        }
        path.to_string_lossy().to_string()
    }

    /// Build the bottom-of-hover `at <link>` markdown string for a resolved
    /// file path and optional 1-based line. The link is a `file://` URL so
    /// Zed (and other LSP clients with markdown link support) treat it as
    /// click-to-open. Falls back to an unlinked monospace path when the
    /// path can't be converted to a URL (extremely rare — only relative or
    /// invalid UTF-8 paths fail).
    async fn source_link(&self, abs_path: &Path, line: Option<u32>) -> String {
        let display = self.relative_display_path(abs_path).await;
        match Url::from_file_path(abs_path) {
            Ok(url) => laravel_lsp::hover::source_link(&display, url.as_str(), line),
            Err(_) => match line {
                Some(l) => format!("`{}:{}`", display, l),
                None => format!("`{}`", display),
            },
        }
    }

    /// Return the cached vendor translation-namespace map, building it on
    /// first call. The scan walks `vendor/` for service providers calling
    /// `loadTranslationsFrom(...)` — see [`laravel_lsp::vendor_translations`].
    /// Subsequent hover calls reuse the cached Arc without re-scanning.
    async fn vendor_translation_namespaces_for(
        &self,
        root: &Path,
    ) -> Option<Arc<HashMap<String, PathBuf>>> {
        {
            let guard = self.vendor_translation_namespaces.read().await;
            if let Some(ref existing) = *guard {
                return Some(existing.clone());
            }
        }
        // Cache miss — scan and store. Done under spawn_blocking so the
        // walkdir traversal doesn't block the LSP event loop.
        let root_clone = root.to_path_buf();
        let scanned = tokio::task::spawn_blocking(move || {
            laravel_lsp::vendor_translations::scan_vendor_translation_namespaces(&root_clone)
        })
        .await
        .ok()?;
        let arc = Arc::new(scanned);
        *self.vendor_translation_namespaces.write().await = Some(arc.clone());
        Some(arc)
    }
}

/// Classify the symbol under the cursor, with a declaration-site fallback
/// for files the parser doesn't fully cover.
///
/// The tree-sitter `php.scm` query captures every call-site shape for routes
/// (`route('home')`, `URL::route('home')`, `signed_route('home')`, etc.) but
/// has no rule for `->name('home')` declarations. When the cursor sits on a
/// declaration, the primary classifier returns `None`, which would make
/// references / rename silently no-op — confusing because the user is
/// pointing right at the symbol's definition.
///
/// The fallback walks the file with `route_name_locator` (the same locator
/// rename already uses) and checks whether the cursor falls inside any
/// `->name(...)` argument range. If so, return a `Route` symbol carrying
/// the locator's full route name.
async fn classify_with_decl_fallback(
    server: &LaravelLanguageServer,
    file_path: &Path,
    patterns: &laravel_lsp::salsa_impl::ParsedPatternsData,
    position: Position,
) -> Option<laravel_lsp::references::SymbolRef> {
    // Primary path: the parser already classified something here.
    if let Some(sym) = laravel_lsp::references::classify_pattern_at_cursor(
        patterns,
        position.line,
        position.character,
    ) {
        return Some(sym);
    }

    // Fallback path: route-name declarations live in `routes/*.php` and
    // aren't tagged by php.scm. Use the mtime-cached decl walker so
    // subsequent invocations don't re-parse the file.
    if !is_in_routes_dir(file_path) {
        return None;
    }
    let decls = server.cached_route_decls(file_path).await?;
    for decl in decls.iter() {
        if decl.line == position.line
            && position.character >= decl.start_column
            && position.character <= decl.end_column
        {
            return Some(laravel_lsp::references::SymbolRef::Route(
                decl.full_name.clone(),
            ));
        }
    }
    None
}

/// Look up the source-text range a `prepare_rename` should return when the
/// cursor sat on a declaration-fallback site (parser saw nothing, but the
/// locator did). Used for prepare_rename's editor-highlight range.
async fn decl_range_at(
    server: &LaravelLanguageServer,
    file_path: &Path,
    position: Position,
    symbol: &laravel_lsp::references::SymbolRef,
) -> Option<Range> {
    // Only route declarations are currently locator-discoverable; configs
    // and translations are reachable only via call-site classification.
    let laravel_lsp::references::SymbolRef::Route(name) = symbol else {
        return None;
    };
    let decls = server.cached_route_decls(file_path).await?;
    for d in decls.iter().filter(|d| d.full_name == *name) {
        if d.line == position.line
            && position.character >= d.start_column
            && position.character <= d.end_column
        {
            return Some(Range {
                start: Position {
                    line: d.line,
                    character: d.start_column,
                },
                end: Position {
                    line: d.line,
                    character: d.end_column,
                },
            });
        }
    }
    None
}

/// Heuristic: is this file under a `routes/` subdirectory anywhere in its
/// path? Used to gate the declaration-fallback walk so we don't pay the
/// parse cost on every PHP file.
fn is_in_routes_dir(path: &Path) -> bool {
    path.components()
        .any(|c| c.as_os_str() == std::ffi::OsStr::new("routes"))
}

/// Return the column range of the classified pattern under the cursor.
/// Used by `prepare_rename` so the editor highlights the right span.
fn pattern_range_at(
    patterns: &laravel_lsp::salsa_impl::ParsedPatternsData,
    line: u32,
    column: u32,
) -> Option<Range> {
    let pat = patterns.find_at_position(line, column)?;
    let (l, start, end) = match pat {
        laravel_lsp::salsa_impl::PatternAtPosition::View(v) => (v.line, v.column, v.end_column),
        laravel_lsp::salsa_impl::PatternAtPosition::Route(r) => (r.line, r.column, r.end_column),
        laravel_lsp::salsa_impl::PatternAtPosition::ConfigRef(c) => {
            (c.line, c.column, c.end_column)
        }
        laravel_lsp::salsa_impl::PatternAtPosition::Translation(t) => {
            (t.line, t.column, t.end_column)
        }
        laravel_lsp::salsa_impl::PatternAtPosition::EnvRef(e) => (e.line, e.column, e.end_column),
        laravel_lsp::salsa_impl::PatternAtPosition::Component(c) => {
            // Range covers the full `x-name` tag including the prefix.
            // Keeping the prefix visible in the rename input gives the
            // user a clear picture of what they're editing — matches
            // what they see in source. The rename handler strips `x-`
            // for file-path computation and re-prefixes text edits so
            // tags stay valid regardless of whether the user types the
            // prefix back in.
            (c.line, c.column, c.end_column)
        }
        laravel_lsp::salsa_impl::PatternAtPosition::Livewire(l) => (l.line, l.column, l.end_column),
        laravel_lsp::salsa_impl::PatternAtPosition::Middleware(m) => {
            (m.line, m.column, m.end_column)
        }
        laravel_lsp::salsa_impl::PatternAtPosition::Binding(b) => (b.line, b.column, b.end_column),
        laravel_lsp::salsa_impl::PatternAtPosition::Directive(d) => {
            (d.line, d.string_column, d.string_end_column)
        }
        // Other patterns aren't renameable; range still useful for highlights.
        laravel_lsp::salsa_impl::PatternAtPosition::Asset(a) => (a.line, a.column, a.end_column),
        laravel_lsp::salsa_impl::PatternAtPosition::Url(u) => (u.line, u.column, u.end_column),
        laravel_lsp::salsa_impl::PatternAtPosition::Action(a) => (a.line, a.column, a.end_column),
        laravel_lsp::salsa_impl::PatternAtPosition::Feature(f) => (f.line, f.column, f.end_column),
    };
    Some(Range {
        start: Position {
            line: l,
            character: start,
        },
        end: Position {
            line: l,
            character: end,
        },
    })
}

/// Resolve the source position of a dotted config key's declaration in
/// `config/<file>.php` and return an edit target. Wraps
/// [`laravel_lsp::config_key_locator::locate_key`] so the rename handler
/// stays terse.
fn collect_config_declaration_target(
    root: &Path,
    old_key: &str,
    new_key: &str,
) -> Option<laravel_lsp::rename::EditTarget> {
    let pos = laravel_lsp::config_key_locator::locate_key(root, old_key)?;
    let file_stem = old_key.split('.').next()?;
    let file_path = root.join("config").join(format!("{file_stem}.php"));
    // Decl text = leaf segment of the new dotted form. The file portion
    // stays — it IS the config filename, which renames don't move.
    let new_leaf = new_key.rsplit('.').next().unwrap_or(new_key).to_string();
    Some(laravel_lsp::rename::EditTarget {
        file_path,
        line: pos.line,
        start_column: pos.start_column,
        end_column: pos.end_column,
        new_text: new_leaf,
    })
}

/// Find every declaration-site `Location` for a classified symbol — the
/// references-side companion to the rename helpers below. Walks the same
/// tree-sitter locators but returns `Location` values directly. Kinds whose
/// declaration is invisible to the parser (everything outside route/config/
/// translation right now) contribute nothing.
async fn collect_declaration_locations(
    server: &LaravelLanguageServer,
    root: &Path,
    symbol: &laravel_lsp::references::SymbolRef,
) -> Vec<Location> {
    use laravel_lsp::references::SymbolRef;
    let mut out = Vec::new();
    match symbol {
        SymbolRef::Route(name) => {
            let routes_dir = root.join("routes");
            if !routes_dir.exists() {
                return out;
            }
            for entry in WalkDir::new(&routes_dir)
                .max_depth(6)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                let path = entry.path();
                if !path.is_file() || path.extension().is_none_or(|ext| ext != "php") {
                    continue;
                }
                let Some(all_decls) = server.cached_route_decls(path).await else {
                    continue;
                };
                for d in all_decls.iter().filter(|d| d.full_name == *name) {
                    if let Ok(uri) = Url::from_file_path(path) {
                        out.push(Location {
                            uri,
                            range: Range {
                                start: Position {
                                    line: d.line,
                                    character: d.start_column,
                                },
                                end: Position {
                                    line: d.line,
                                    character: d.end_column,
                                },
                            },
                        });
                    }
                }
            }
        }
        SymbolRef::Config(key) => {
            if let Some(pos) = laravel_lsp::config_key_locator::locate_key(root, key) {
                if let Some(file_stem) = key.split('.').next() {
                    let path = root.join("config").join(format!("{file_stem}.php"));
                    if let Ok(uri) = Url::from_file_path(&path) {
                        out.push(Location {
                            uri,
                            range: Range {
                                start: Position {
                                    line: pos.line,
                                    character: pos.start_column,
                                },
                                end: Position {
                                    line: pos.line,
                                    character: pos.end_column,
                                },
                            },
                        });
                    }
                }
            }
        }
        SymbolRef::Translation(key) => {
            let locs = laravel_lsp::translation_key_locator::locate_keys_across_locales(root, key);
            for loc in locs {
                if let Ok(uri) = Url::from_file_path(&loc.file_path) {
                    out.push(Location {
                        uri,
                        range: Range {
                            start: Position {
                                line: loc.position.line,
                                character: loc.position.start_column,
                            },
                            end: Position {
                                line: loc.position.line,
                                character: loc.position.end_column,
                            },
                        },
                    });
                }
            }
        }
        SymbolRef::Env(key) => {
            // Env declarations live in every `.env*` file at the project
            // root that has the matching `KEY=…` line. Surfacing them in
            // find-references is the same shape as translations — one
            // Location per matching file.
            let locs = laravel_lsp::env_key_locator::locate_keys_across_env_files(root, key);
            for loc in locs {
                if let Ok(uri) = Url::from_file_path(&loc.file_path) {
                    out.push(Location {
                        uri,
                        range: Range {
                            start: Position {
                                line: loc.position.line,
                                character: loc.position.start_column,
                            },
                            end: Position {
                                line: loc.position.line,
                                character: loc.position.end_column,
                            },
                        },
                    });
                }
            }
        }
        // View, component, livewire, middleware, binding: their declarations
        // either don't have a single canonical point or they involve a PHP
        // class (deferred to Phase 3). No additional locations to surface.
        _ => {}
    }
    out
}

/// Resolve translation-key declaration positions across every locale's lang
/// file. The LEAF segment of the new dotted form is written at each
/// declaration; the file portion is the lang filename and can't change
/// without moving the file.
fn collect_translation_declaration_targets(
    root: &Path,
    old_key: &str,
    new_key: &str,
) -> Vec<laravel_lsp::rename::EditTarget> {
    let locations = laravel_lsp::translation_key_locator::locate_keys_across_locales(root, old_key);
    let new_leaf = new_key.rsplit('.').next().unwrap_or(new_key).to_string();
    locations
        .into_iter()
        .map(|loc| laravel_lsp::rename::EditTarget {
            file_path: loc.file_path,
            line: loc.position.line,
            start_column: loc.position.start_column,
            end_column: loc.position.end_column,
            new_text: new_leaf.clone(),
        })
        .collect()
}

/// Resolve env-key declaration positions across every `.env*` file at the
/// project root. Unlike config and translation, env keys aren't dotted —
/// the new name is written verbatim at every declaration site, matching
/// what gets written at call sites in `env('NEW_NAME')`.
fn collect_env_declaration_targets(
    root: &Path,
    old_key: &str,
    new_key: &str,
) -> Vec<laravel_lsp::rename::EditTarget> {
    laravel_lsp::env_key_locator::locate_keys_across_env_files(root, old_key)
        .into_iter()
        .map(|loc| laravel_lsp::rename::EditTarget {
            file_path: loc.file_path,
            line: loc.position.line,
            start_column: loc.position.start_column,
            end_column: loc.position.end_column,
            new_text: new_key.to_string(),
        })
        .collect()
}

/// Walk every `routes/*.php` under the project root and surface
/// `->name(...)` declaration sites whose full name matches `target`. Each
/// emitted target writes `new_name` verbatim at the captured position. (For
/// group-prefixed routes, the locator already emits one target per segment
/// so the prefix composition stays intact.)
async fn collect_route_declaration_targets(
    server: &LaravelLanguageServer,
    root: &Path,
    target: &str,
    new_name: &str,
) -> Vec<laravel_lsp::rename::EditTarget> {
    let mut targets = Vec::new();
    let routes_dir = root.join("routes");
    if !routes_dir.exists() {
        return targets;
    }
    for entry in WalkDir::new(&routes_dir)
        .max_depth(6)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() || path.extension().is_none_or(|ext| ext != "php") {
            continue;
        }
        // Mtime-cached: first invocation per file mtime parses the file
        // with route_name_locator; subsequent invocations are a HashMap hit
        // until the file changes on disk.
        let Some(all_decls) = server.cached_route_decls(path).await else {
            continue;
        };
        for d in all_decls.iter().filter(|d| d.full_name == target) {
            targets.push(laravel_lsp::rename::EditTarget {
                file_path: path.to_path_buf(),
                line: d.line,
                start_column: d.start_column,
                end_column: d.end_column,
                new_text: new_name.to_string(),
            });
        }
    }
    targets
}

/// Convert a Salsa `ReferenceLocationData` into an LSP `Location`. Positions
/// are 0-based throughout (matches `tree-sitter` and `lsp-types`). Returns
/// `None` when the file path can't be expressed as a `file://` URL.
fn reference_location_to_lsp(loc: &ReferenceLocationData) -> Option<Location> {
    let uri = Url::from_file_path(&loc.file_path).ok()?;
    Some(Location {
        uri,
        range: Range {
            start: Position {
                line: loc.line,
                character: loc.column,
            },
            end: Position {
                line: loc.line,
                character: loc.end_column,
            },
        },
    })
}

/// Convert a `document_symbols::SymbolEntry` (LSP-agnostic) into a tower-lsp
/// `DocumentSymbol`. Recurses into children. `selection_range` is set to the
/// same range as `range` — clients use selection_range for the click target.
#[allow(deprecated)]
fn symbol_entry_to_lsp(entry: &laravel_lsp::document_symbols::SymbolEntry) -> DocumentSymbol {
    let range = Range {
        start: Position {
            line: entry.start_line,
            character: entry.start_column,
        },
        end: Position {
            line: entry.end_line,
            character: entry.end_column,
        },
    };

    DocumentSymbol {
        name: entry.name.clone(),
        detail: entry.detail.clone(),
        kind: symbol_entry_kind_to_lsp(entry.kind),
        tags: None,
        // `deprecated` is deprecated in the LSP spec in favour of `tags`,
        // but the field is still required by tower-lsp's struct definition.
        deprecated: None,
        range,
        selection_range: range,
        children: if entry.children.is_empty() {
            None
        } else {
            Some(entry.children.iter().map(symbol_entry_to_lsp).collect())
        },
    }
}

fn symbol_entry_kind_to_lsp(kind: laravel_lsp::document_symbols::SymbolEntryKind) -> SymbolKind {
    use laravel_lsp::document_symbols::SymbolEntryKind;
    match kind {
        SymbolEntryKind::Class => SymbolKind::CLASS,
        SymbolEntryKind::Interface => SymbolKind::INTERFACE,
        SymbolEntryKind::Trait => SymbolKind::STRUCT,
        SymbolEntryKind::Enum => SymbolKind::ENUM,
        SymbolEntryKind::Method => SymbolKind::METHOD,
        SymbolEntryKind::Property => SymbolKind::PROPERTY,
        SymbolEntryKind::Field => SymbolKind::FIELD,
        SymbolEntryKind::Function => SymbolKind::FUNCTION,
        SymbolEntryKind::Namespace => SymbolKind::NAMESPACE,
        SymbolEntryKind::Variable => SymbolKind::VARIABLE,
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for LaravelLanguageServer {
    async fn initialize(&self, params: InitializeParams) -> jsonrpc::Result<InitializeResult> {
        let init_start = std::time::Instant::now();
        info!("Laravel LSP: INITIALIZE");

        // Read initial settings from initialization_options (if provided)
        // These can be overridden at runtime via did_change_configuration
        if let Some(init_options) = params.initialization_options {
            match serde_json::from_value::<LspSettings>(init_options) {
                Ok(settings) => {
                    info!("⚙️  Initial settings: autoCompleteDebounce={}ms, blade.directiveSpacing={}",
                        settings.auto_complete_debounce, settings.blade.directive_spacing);
                    self.update_settings(&settings).await;
                }
                Err(e) => {
                    debug!("Could not parse initialization_options: {}", e);
                }
            }
        }

        // Store the root path - lightweight operation
        if let Some(root_uri) = params.root_uri {
            if let Ok(path) = root_uri.to_file_path() {
                *self.root_path.write().await = Some(path.clone());
                info!("✅ Laravel LSP: Root path set to {:?}", path);

                // Load ALL cached data (config, middleware, bindings, env) using batch registration (fast)
                // This uses 2 round-trips instead of N round-trips for N entries
                let t_cache = std::time::Instant::now();
                info!("📦 Loading cached data...");
                let needs_rescans = self.load_cache_data(&path).await;
                info!("⏱️  load_cache_data: {:?}", t_cache.elapsed());

                // Store needs_rescans for initialized() to pick up
                self.pending_rescans.write().await.extend(needs_rescans);
            }
        }
        info!("⏱️  INITIALIZE TOTAL: {:?}", init_start.elapsed());

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                // We support go-to-definition
                definition_provider: Some(OneOf::Left(true)),

                // We need to sync document content and receive save notifications
                text_document_sync: Some(TextDocumentSyncCapability::Options(
                    TextDocumentSyncOptions {
                        open_close: Some(true),
                        change: Some(TextDocumentSyncKind::FULL),
                        will_save: None,
                        will_save_wait_until: None,
                        save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
                            include_text: Some(false), // We get text from did_change
                        })),
                    },
                )),

                // ✅ Hover provider — shows resolved type information for Blade
                // variables and their property accesses. For Livewire-component
                // properties, displays both the declared type and (when not
                // statically determinable) a "value: undetermined" indicator
                // per Mike's spec.
                hover_provider: Some(HoverProviderCapability::Simple(true)),

                // ✅ Completion provider for autocomplete features
                // Triggers on various characters depending on context:
                // - ' and " for env(), config(), route(), etc.
                // - { for ${...} in .env files
                // - | for pipe-delimited validation rules
                // - : for validation rule parameters (exists:, after:, etc.)
                // - @ for Blade directives (@if, @foreach, etc.)
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![
                        "'".to_string(),  // env(', config(', route(', etc.
                        "\"".to_string(), // env(", config(", route(", etc.
                        "{".to_string(),  // {{ for echo, {!! for unescaped, {{-- for comment
                        "!".to_string(),  // {!! for unescaped echo
                        "-".to_string(),  // {{-- for Blade comment
                        "|".to_string(),  // validation rules: 'required|'
                        ":".to_string(),  // validation rule params: 'exists:'
                        ".".to_string(),  // connection.table in exists:/unique:
                        ",".to_string(),  // table,column in exists:/unique:
                        "@".to_string(),  // Blade directives: @if, @foreach, etc.
                        "$".to_string(), // Blade variables: $name, $form, etc. (shows full list on bare $)
                        ">".to_string(), // Property access: $form-> triggers property completion
                    ]),
                    ..Default::default()
                }),

                // ✅ Code actions for quick fixes (create missing views, etc.)
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),

                // ✅ Document symbol provider — populates outline panels
                // (Zed outline, Helix symbol picker, Neovim aerial, etc.) with
                // Laravel-aware structure: route definitions, Blade section
                // hierarchy, Livewire component members, Eloquent relationships
                // and scopes.
                document_symbol_provider: Some(OneOf::Left(true)),

                // ✅ References provider — `textDocument/references`. Finds
                // every parser-classified call site for a Laravel pattern
                // (view, route, config, translation, env, blade component,
                // livewire, middleware, binding). Never matches raw string
                // shape — only positions the parser tagged as the matching
                // pattern kind are returned.
                references_provider: Some(OneOf::Left(true)),

                // ✅ Rename provider — `textDocument/rename` /
                // `textDocument/prepareRename`. Phase 2: route names, config
                // keys, translation keys. Anything that resolves to (or may
                // resolve to) a PHP class is held back for Phase 3.
                rename_provider: Some(OneOf::Right(RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: WorkDoneProgressOptions {
                        work_done_progress: None,
                    },
                })),

                // On-type formatting (currently unused, bracket expansion uses completions)
                document_on_type_formatting_provider: Some(DocumentOnTypeFormattingOptions {
                    first_trigger_character: "{".to_string(),
                    more_trigger_character: Some(vec!["!".to_string(), "-".to_string()]),
                }),

                // ✅ Semantic tokens for dynamic Blade directive highlighting
                // Provides real-time highlighting that updates on every keystroke,
                // overriding tree-sitter's incremental parsing which can leave stale highlights
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            legend: SemanticTokensLegend {
                                token_types: vec![
                                    SemanticTokenType::FUNCTION, // index 0 - @directive
                                ],
                                token_modifiers: vec![],
                            },
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                            range: None,
                            ..Default::default()
                        },
                    ),
                ),

                // ✅ `workspace/willRenameFiles` — Phase 3d. When the user
                // renames a `.blade.php` in Zed's file tree, we get a
                // chance to rewrite every call site referencing the old
                // name before the rename happens. Pattern is intentionally
                // narrow: only `.blade.php` files in any depth. Class-file
                // (.php) renames could be added later for class-based
                // components and Livewire classes.
                workspace: Some(WorkspaceServerCapabilities {
                    workspace_folders: None,
                    file_operations: Some(WorkspaceFileOperationsServerCapabilities {
                        will_rename: Some(FileOperationRegistrationOptions {
                            filters: vec![FileOperationFilter {
                                scheme: Some("file".to_string()),
                                pattern: FileOperationPattern {
                                    glob: "**/*.blade.php".to_string(),
                                    matches: Some(FileOperationPatternKind::File),
                                    options: None,
                                },
                            }],
                        }),
                        did_create: None,
                        will_create: None,
                        did_rename: None,
                        did_delete: None,
                        will_delete: None,
                    }),
                }),

                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        info!("========================================");
        info!("🚀 Laravel LSP: INITIALIZED - spawning background work");
        info!("========================================");

        // Get root path
        let root = match self.root_path.read().await.clone() {
            Some(r) => r,
            None => {
                info!("No root path set, skipping background initialization");
                return;
            }
        };

        // Spawn background task for heavy initialization work
        // This doesn't block the LSP - Zed can start sending requests immediately
        // Note: If cache exists, config/middleware/env are already loaded in initialize()
        let server = self.clone_for_spawn();
        let client = self.client.clone();
        tokio::spawn(async move {
            // Open a single LSP work-done progress token that spans the
            // full first-load indexing flow (config → files → warming).
            // Title is just "Laravel" so the user can see at a glance
            // which extension owns the status-bar entry — Zed's status
            // bar can host multiple LSP progress entries simultaneously
            // and an unbranded one would be ambiguous. The descriptive
            // detail ("Indexing 12,345 of 40,589 files") lives in the
            // `message` so the title stays short.
            //
            // If the client doesn't support work-done-progress, `begin`
            // returns None and the rest of the flow just skips reports.
            let mut progress = laravel_lsp::indexing_progress::IndexingProgress::begin(
                client,
                "Laravel",
                "Starting indexer…",
            )
            .await;

            // Register config if not loaded from cache
            if server.get_cached_config().await.is_none() {
                info!("📋 No cached config, registering from files...");
                if let Some(p) = progress.as_mut() {
                    p.report("Loading project configuration…", Some(0), true)
                        .await;
                }
                server.register_config_with_salsa(&root).await;
            }

            // Register project files with Salsa for reference finding (if config available).
            // The progress handle is MOVED into register_project_files_with_salsa,
            // which forwards it into the spawned warming task — that task is
            // responsible for ending the progress when warming completes.
            if let Some(config) = server.get_cached_config().await {
                info!(
                    "Laravel config available: {} view paths",
                    config.view_paths.len()
                );

                // Tell the client which files to watch for live cache
                // invalidation. This MUST happen before warming starts,
                // not after, so that any external changes during the
                // 7-second cold parse are captured rather than missed.
                // We use dynamic registration (not declared in
                // `initialize`) because the view paths and livewire
                // path depend on project config we only just loaded.
                let registration = laravel_lsp::file_watcher::build_registration(
                    &root,
                    &config.view_paths,
                    config.livewire_path.as_deref(),
                );
                match server.client.register_capability(vec![registration]).await {
                    Ok(_) => info!(
                        "🛡️  Registered file watcher: {} view paths, livewire={}",
                        config.view_paths.len(),
                        config.livewire_path.is_some()
                    ),
                    Err(e) => debug!(
                        "File watcher registration failed (client may not support it): {}",
                        e
                    ),
                }

                server
                    .register_project_files_with_salsa(&root, progress.take())
                    .await;
            } else {
                info!("Config not available for project file registration");
                if let Some(p) = progress.take() {
                    p.end("No project config found.").await;
                }
            }

            // Register env files with Salsa (if not loaded from cache)
            server.register_env_files_with_salsa(&root).await;

            // Initialize database schema provider for exists:/unique: validation autocomplete
            server.init_database_schema_provider(&root).await;

            // Queue and execute initial vendor/app rescans
            // This registers middleware and bindings from Laravel framework and service providers
            info!("🔍 Queueing initial vendor and app rescans...");
            server
                .pending_rescans
                .write()
                .await
                .insert(RescanType::Vendor);
            server.pending_rescans.write().await.insert(RescanType::App);

            // Execute pending rescans (vendor, app, node_modules)
            server.execute_pending_rescans().await;

            info!("✅ Background Salsa registration complete");
        });
    }

    async fn shutdown(&self) -> jsonrpc::Result<()> {
        info!("Laravel LSP: Shutting down - cleaning up resources");

        // Cancel all pending diagnostic tasks
        {
            let mut pending = self.pending_diagnostics.write().await;
            for (uri, handle) in pending.drain() {
                debug!("Cancelling pending diagnostics for: {}", uri);
                handle.abort();
            }
        }

        // Clear document cache
        self.documents.write().await.clear();

        // Clear diagnostics cache
        self.diagnostics.write().await.clear();

        // Shutdown Salsa actor
        if let Err(e) = self.salsa.shutdown().await {
            debug!("Salsa actor shutdown: {}", e);
        }

        info!("Laravel LSP: Shutdown complete");
        Ok(())
    }

    async fn on_type_formatting(
        &self,
        params: DocumentOnTypeFormattingParams,
    ) -> jsonrpc::Result<Option<Vec<TextEdit>>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;

        // Only handle Blade files
        if !uri.path().ends_with(".blade.php") {
            return Ok(None);
        }

        // Get document content
        let documents = self.documents.read().await;
        let content = match documents.get(uri) {
            Some((text, _)) => text.clone(),
            None => return Ok(None),
        };
        drop(documents);

        // Get the current line
        let lines: Vec<&str> = content.lines().collect();
        let line_text = match lines.get(position.line as usize) {
            Some(line) => *line,
            None => return Ok(None),
        };

        // Blade bracket expansion is handled via snippet completions, not on_type_formatting
        let _ = (line_text, position); // Silence unused warnings
        Ok(None)
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let total_start = std::time::Instant::now();
        let uri = params.text_document.uri;
        let text = params.text_document.text;
        let version = params.text_document.version;

        info!(
            "📂 did_open: {}",
            uri.path().split('/').next_back().unwrap_or("")
        );
        self.documents
            .write()
            .await
            .insert(uri.clone(), (text.clone(), version));

        // Try to discover Laravel config from this file if we don't have one yet
        if let Ok(file_path) = uri.to_file_path() {
            let t1 = std::time::Instant::now();
            self.try_discover_from_file(&file_path).await;
            info!("   ⏱️  try_discover_from_file: {:?}", t1.elapsed());

            // Update Salsa database with new file content
            let t2 = std::time::Instant::now();
            if let Err(e) = self
                .salsa
                .update_file(file_path.clone(), version, text.clone())
                .await
            {
                debug!("Failed to update Salsa database: {}", e);
            }
            info!("   ⏱️  salsa.update_file: {:?}", t2.elapsed());
        }

        // Validate and publish diagnostics for Blade files
        let t3 = std::time::Instant::now();
        self.validate_and_publish_diagnostics(&uri, &text).await;
        info!(
            "   ⏱️  validate_and_publish_diagnostics: {:?}",
            t3.elapsed()
        );
        info!("   ✅ did_open total: {:?}", total_start.elapsed());
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let version = params.text_document.version;

        if let Some(change) = params.content_changes.into_iter().next() {
            debug!(
                "Laravel LSP: Document changed: {} (version: {})",
                uri, version
            );

            // Store in documents buffer immediately (for goto_definition during debounce)
            self.documents
                .write()
                .await
                .insert(uri.clone(), (change.text.clone(), version));

            // Queue debounced Salsa update (250ms)
            // This handles all file types: SourceFile, ConfigFile, EnvFile, ServiceProviderFile
            // After debounce, execute_salsa_update will:
            // 1. Determine file type and update appropriate Salsa input
            // 2. Re-run diagnostics for this file
            self.queue_salsa_update(uri, change.text, version).await;
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri;
        info!("🔔 Laravel LSP: did_save called for {}", uri);

        // Check for lock file changes that trigger rescans
        if let Ok(path) = uri.to_file_path() {
            let file_name = path.file_name().and_then(|n| n.to_str());
            let path_str = path.to_string_lossy();

            // Invalidate config cache if config-related files change
            let is_config_file = matches!(file_name, Some("composer.json"))
                || path_str.contains("/config/")
                || matches!(file_name, Some("view.php" | "livewire.php"));

            if is_config_file {
                info!("📦 Config file changed, invalidating config cache");
                self.invalidate_config_cache().await;
            }

            match file_name {
                Some("composer.lock") => {
                    info!("📦 composer.lock changed, queuing vendor rescan");
                    self.queue_background_rescan(RescanType::Vendor).await;
                }
                Some("package-lock.json") | Some("yarn.lock") | Some("pnpm-lock.yaml") => {
                    info!("📦 Package lock changed, queuing node_modules rescan");
                    self.queue_background_rescan(RescanType::NodeModules).await;
                }
                Some(name) if name.ends_with(".php") && path_str.contains("app/Providers/") => {
                    info!("📦 App provider changed, queuing app rescan");
                    self.queue_background_rescan(RescanType::App).await;
                }
                Some("app.php") if path.parent().is_some_and(|p| p.ends_with("bootstrap")) => {
                    info!("📦 bootstrap/app.php changed, queuing app rescan");
                    self.queue_background_rescan(RescanType::App).await;
                }
                _ => {}
            }
        }

        // Cancel any pending debounced diagnostics for this file
        // We'll run diagnostics immediately on save instead
        if let Some(handle) = self.pending_diagnostics.write().await.remove(&uri) {
            handle.abort();
            info!("   ✅ Cancelled pending diagnostic task");
        }

        // Run cache update AND diagnostics on save
        if let Some((text, _version)) = self.documents.read().await.get(&uri).cloned() {
            info!("   ✅ Found document in cache, updating cache and running diagnostics...");
            let is_blade = uri.path().ends_with(".blade.php");
            let is_php = uri.path().ends_with(".php");

            if is_blade || is_php {
                // Removed: parse_and_cache_patterns - performance_cache handles this automatically
            }

            // Run diagnostics immediately on save
            info!("   📊 Running diagnostics immediately on save for {}", uri);
            self.validate_and_publish_diagnostics(&uri, &text).await;
            info!("   ✅ Diagnostics published for {}", uri);
        } else {
            debug!("warn:  Document not found in cache for {}", uri);
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        debug!("Laravel LSP: Document closed: {}", uri);

        // Cancel any pending debounced diagnostics
        if let Some(handle) = self.pending_diagnostics.write().await.remove(&uri) {
            handle.abort();
        }

        self.documents.write().await.remove(&uri);

        // Clear diagnostics from our cache
        self.diagnostics.write().await.remove(&uri);

        // Remove from Salsa database
        if let Ok(file_path) = uri.to_file_path() {
            if let Err(e) = self.salsa.remove_file(file_path).await {
                debug!("Failed to remove from Salsa database: {}", e);
            }
        }

        // Publish empty diagnostics to clear them from the client
        self.client.publish_diagnostics(uri, vec![], None).await;
    }

    /// Client-reported file change for a path matching one of our
    /// registered globs (set up via `client.register_capability` in
    /// `initialized`). Catches changes made outside Zed — `git pull`,
    /// external formatters, second-editor saves — that would otherwise
    /// leave the in-memory pattern cache stale.
    ///
    /// Behaviour:
    ///
    /// - **Created / Changed**: read the file (if reachable), push the
    ///   new content into Salsa via `update_file`. That call also
    ///   removes the path from `pattern_cache`, so the next reference
    ///   query parses fresh. We do NOT re-parse eagerly — spreading
    ///   the work across user-driven queries is cheaper than burning
    ///   CPU on a `git checkout` burst.
    ///
    /// - **Deleted**: tell Salsa to drop the file entirely. This
    ///   removes its `SourceFile` input AND every per-file cache
    ///   keyed by path (pattern_cache, loop_blocks_cache, etc.).
    ///
    /// - **Open documents**: events for currently-open files are
    ///   skipped. The editor buffer is authoritative for those — its
    ///   content has already been (or will be) pushed via
    ///   `didChange`. Honouring a disk-side change would clobber
    ///   unsaved buffer content.
    ///
    /// All work is idempotent: duplicate events for the same path
    /// collapse safely, atomic-write event storms (Created+Deleted+
    /// Renamed for tmp files) are filtered by the glob layer.
    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        if params.changes.is_empty() {
            return;
        }
        debug!(
            "did_change_watched_files: {} change(s) from client",
            params.changes.len()
        );

        // Snapshot open-doc URIs once so we don't hammer the documents
        // lock per event in a burst.
        let open_docs: std::collections::HashSet<Url> = {
            let docs = self.documents.read().await;
            docs.keys().cloned().collect()
        };

        let mut created_or_changed = 0usize;
        let mut deleted = 0usize;
        let mut skipped_open = 0usize;

        for change in params.changes {
            if open_docs.contains(&change.uri) {
                skipped_open += 1;
                continue;
            }
            let Ok(path) = change.uri.to_file_path() else {
                continue;
            };
            match change.typ {
                FileChangeType::CREATED => {
                    // Best-effort read. The file may already be gone
                    // (delete-after-change race) — skip silently.
                    let Ok(text) = std::fs::read_to_string(&path) else {
                        continue;
                    };
                    // Register the new path in its category list FIRST
                    // so that subsequent find-references walks visit
                    // it. Then push content into Salsa via update_file
                    // (which also invalidates pattern_cache for the
                    // path). Order matters: if update_file ran first,
                    // a concurrent reference query that started
                    // between the two awaits would parse the file but
                    // never iterate its path. Adding to the category
                    // list first closes that race.
                    let _ = self
                        .salsa
                        .update_project_file_list(
                            path.clone(),
                            laravel_lsp::salsa_impl::FileListOp::Add,
                        )
                        .await;
                    // Version 0 is fine here: `handle_update_file`
                    // bumps the Salsa input version internally on
                    // every set, which is what invalidates downstream
                    // tracked queries. The numeric version field is
                    // for the editor's textDocument/didChange path
                    // and isn't load-bearing on this code path.
                    if let Err(e) = self.salsa.update_file(path, 0, text).await {
                        debug!("watched-files: update_file failed: {}", e);
                    } else {
                        created_or_changed += 1;
                    }
                }
                FileChangeType::CHANGED => {
                    // No category-list update needed — a CHANGED event
                    // is for a file that was already in our index.
                    // Just invalidate cache + push fresh content.
                    let Ok(text) = std::fs::read_to_string(&path) else {
                        continue;
                    };
                    if let Err(e) = self.salsa.update_file(path, 0, text).await {
                        debug!("watched-files: update_file failed: {}", e);
                    } else {
                        created_or_changed += 1;
                    }
                }
                FileChangeType::DELETED => {
                    // Tear down both the Salsa input and the
                    // category-list entry. Order is the reverse of
                    // CREATED for the same reason: drop from the list
                    // FIRST so concurrent find-references don't visit
                    // a path whose Salsa state we're about to remove.
                    let _ = self
                        .salsa
                        .update_project_file_list(
                            path.clone(),
                            laravel_lsp::salsa_impl::FileListOp::Remove,
                        )
                        .await;
                    if let Err(e) = self.salsa.remove_file(path).await {
                        debug!("watched-files: remove_file failed: {}", e);
                    } else {
                        deleted += 1;
                    }
                }
                _ => {}
            }
        }

        // One summary log per batch, not per file — bursts of 1000
        // events shouldn't produce 1000 log lines.
        if created_or_changed + deleted + skipped_open > 0 {
            debug!(
                "🛡️  watched files: {} updated, {} deleted, {} skipped (open in editor)",
                created_or_changed, deleted, skipped_open
            );
        }
    }

    async fn did_change_configuration(&self, params: DidChangeConfigurationParams) {
        // Handle runtime configuration changes without requiring LSP restart
        // Settings are configured via: { "lsp": { "laravel-lsp": { "settings": { "laravel": { ... } } } } }
        debug!("🔧 Configuration changed: {:?}", params.settings);

        match serde_json::from_value::<LspSettings>(params.settings) {
            Ok(settings) => {
                info!("⚙️  Configuration updated: autoCompleteDebounce={}ms, blade.directiveSpacing={}",
                    settings.auto_complete_debounce, settings.blade.directive_spacing);
                self.update_settings(&settings).await;
            }
            Err(e) => {
                debug!("Could not parse configuration settings: {}", e);
            }
        }
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> jsonrpc::Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        info!(
            "🎯 goto_definition called: {}:{}:{}",
            uri, position.line, position.character
        );

        // Coalescing window: skip duplicate requests within ~16ms (~60fps)
        const COALESCE_MS: u64 = 16;

        // Early return: only process PHP files
        let is_php = uri.path().ends_with(".php");
        if !is_php {
            return Ok(None);
        }

        // Request coalescing: skip rapid duplicate requests at same position
        // This handles the case where the editor rapidly fires requests while moving cursor
        {
            let last_requests = self.last_goto_request.read().await;
            if let Some((last_pos, last_time)) = last_requests.get(&uri) {
                if *last_pos == position && last_time.elapsed() < Duration::from_millis(COALESCE_MS)
                {
                    // Same position, very recent request - skip to avoid redundant work
                    return Ok(None);
                }
            }
        }

        // Update last request tracking
        self.last_goto_request
            .write()
            .await
            .insert(uri.clone(), (position, Instant::now()));

        // Early return: check if document exists in our cache
        // This avoids expensive Salsa lookups for files we haven't seen
        if !self.documents.read().await.contains_key(&uri) {
            return Ok(None);
        }

        // Convert URI to file path
        let file_path = match uri.to_file_path() {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };

        // Get patterns from Salsa (cached, O(1) lookup)
        let patterns = match self.salsa.get_patterns(file_path).await {
            Ok(Some(p)) => p,
            Ok(None) => {
                debug!("Laravel LSP: No patterns cached for file");
                return Ok(None);
            }
            Err(e) => {
                debug!("Laravel LSP: Error getting patterns: {:?}", e);
                return Ok(None);
            }
        };

        // Find pattern at cursor position
        let pattern = match patterns.find_at_position(position.line, position.character) {
            Some(p) => p,
            None => {
                // Slot fallback: <x-slot:name> tags aren't components and aren't
                // indexed in the position map. Check whether the cursor sits on
                // one before declaring nothing-to-do.
                if let Some(slot_location) = self.create_slot_location(&uri, position).await {
                    return Ok(Some(slot_location));
                }

                // Blade-variable fallback: when the cursor sits on a `$var` or
                // `$var->prop` reference in a `.blade.php` file, jump to the
                // declaration site instead of returning nothing.
                if uri.path().ends_with(".blade.php") {
                    if let Some(loc) = self.blade_variable_goto_definition(&uri, position).await {
                        return Ok(Some(loc));
                    }
                }

                // Debug: show what middleware patterns exist on this line
                let mw_on_line: Vec<_> = patterns
                    .middleware_refs
                    .iter()
                    .filter(|m| m.line == position.line)
                    .map(|m| format!("'{}' col {}-{}", m.name, m.column, m.end_column))
                    .collect();
                info!(
                    "🔍 No pattern at line {} col {} (middleware on line: {:?})",
                    position.line, position.character, mw_on_line
                );
                return Ok(None);
            }
        };

        // Create location based on pattern type
        let location = match pattern {
            PatternAtPosition::View(view) => {
                debug!("Laravel LSP: Found view: {}", view.name);
                self.create_view_location_from_salsa(&view).await
            }
            PatternAtPosition::Component(comp) => {
                debug!("Laravel LSP: Found component: {}", comp.name);
                self.create_component_location_from_salsa(&comp).await
            }
            PatternAtPosition::Livewire(lw) => {
                debug!("Laravel LSP: Found livewire: {}", lw.name);
                self.create_livewire_location_from_salsa(&lw).await
            }
            PatternAtPosition::Directive(dir) => {
                info!(
                    "🎯 Laravel LSP: Found directive: {} with args {:?} at {}:{}-{}",
                    dir.name, dir.arguments, dir.line, dir.column, dir.end_column
                );
                self.create_directive_location_from_salsa(&dir).await
            }
            PatternAtPosition::EnvRef(env) => {
                debug!("Laravel LSP: Found env: {}", env.name);
                self.create_env_location_from_salsa(&env).await
            }
            PatternAtPosition::ConfigRef(config) => {
                debug!("Laravel LSP: Found config: {}", config.key);
                self.create_config_location_from_salsa(&config).await
            }
            PatternAtPosition::Middleware(mw) => {
                info!(
                    "🎯 Found middleware pattern: '{}' at {}:{}-{}",
                    mw.name, mw.line, mw.column, mw.end_column
                );
                let result = self.create_middleware_location_from_salsa(&mw).await;
                if result.is_none() {
                    info!(
                        "❌ Middleware location lookup returned None for '{}'",
                        mw.name
                    );
                }
                result
            }
            PatternAtPosition::Translation(trans) => {
                info!(
                    "🎯 Laravel LSP: Found translation pattern: '{}' at {}:{}-{}",
                    trans.key, trans.line, trans.column, trans.end_column
                );
                self.create_translation_location_from_salsa(&trans).await
            }
            PatternAtPosition::Asset(asset) => {
                debug!("Laravel LSP: Found asset: {}", asset.path);
                self.create_asset_location_from_salsa(&asset).await
            }
            PatternAtPosition::Binding(binding) => {
                debug!("Laravel LSP: Found binding: {}", binding.name);
                self.create_binding_location_from_salsa(&binding).await
            }
            PatternAtPosition::Route(route) => {
                debug!("Laravel LSP: Found route: {}", route.name);
                self.create_route_location_from_salsa(&route).await
            }
            PatternAtPosition::Url(url) => {
                debug!("Laravel LSP: Found url: {}", url.path);
                self.create_url_location_from_salsa(&url).await
            }
            PatternAtPosition::Action(action) => {
                debug!("Laravel LSP: Found action: {}", action.action);
                self.create_action_location_from_salsa(&action).await
            }
            PatternAtPosition::Feature(feature) => {
                debug!("Laravel LSP: Found feature: {}", feature.feature_name);
                self.create_feature_location_from_salsa(&feature).await
            }
        };

        if location.is_none() {
            debug!("Laravel LSP: Could not resolve location for pattern");
        }

        Ok(location)
    }

    /// Hover provider — shows resolved type information for Blade variables and
    /// their property accesses. Per Mike's spec, three states are surfaced:
    ///   - Known type, known value (rare — currently only `@php` literal assignments)
    ///   - Known type, undetermined value (typical: `public ContactForm $form;`)
    ///   - Undetermined type, undetermined value (cursor on a variable with no resolvable source)
    async fn hover(&self, params: HoverParams) -> jsonrpc::Result<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let file_path = match uri.to_file_path() {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };
        let path = match file_path.to_str() {
            Some(s) => s,
            None => return Ok(None),
        };
        let is_blade = path.ends_with(".blade.php");
        let is_php = path.ends_with(".php");
        if !is_blade && !is_php {
            return Ok(None);
        }

        // Pull the current document for the line-text needed by the Blade
        // variable fallback (also gives us the file as the LSP saw it last).
        let content = match self.documents.read().await.get(uri).cloned() {
            Some((c, _version)) => c,
            None => std::fs::read_to_string(path).unwrap_or_default(),
        };
        let line_text = content
            .lines()
            .nth(position.line as usize)
            .unwrap_or("")
            .to_string();

        // Salsa pattern lookup. The target dispatch in `find_hover_target`
        // tries patterns first and only falls back to Blade variables when
        // nothing matched at the cursor.
        let patterns = match self.salsa.get_patterns(file_path).await {
            Ok(Some(p)) => p,
            _ => {
                // No cached patterns — Blade-variable hover can still fire on
                // a .blade.php file when the cursor sits on a `$var` token.
                if !is_blade {
                    return Ok(None);
                }
                let Some((var_name, property)) =
                    extract_blade_variable_at_cursor(&line_text, position.character)
                else {
                    return Ok(None);
                };
                let value = self
                    .build_blade_variable_hover_content(uri, &var_name, property)
                    .await;
                return Ok(value.map(|v| Hover {
                    contents: HoverContents::Markup(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: v,
                    }),
                    range: None,
                }));
            }
        };

        let target = match laravel_lsp::hover::find_hover_target(
            &patterns,
            &line_text,
            position.line,
            position.character,
            is_blade,
        ) {
            Some(t) => t,
            None => return Ok(None),
        };

        let value = match self.build_hover_markdown(target, uri).await {
            Some(v) => v,
            None => return Ok(None),
        };

        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value,
            }),
            range: None,
        }))
    }

    // NOTE: completion handler removed - capability not advertised in ServerCapabilities

    // NOTE: code_lens handler removed - Zed doesn't support custom LSP commands

    /// Document-symbol request — returns Laravel-aware structure for outline
    /// panels. Delegates the parsing to the Salsa actor which memoizes per file
    /// version. Unknown / unsupported file kinds return an empty list rather
    /// than `None` so clients render an empty outline instead of falling back
    /// to no-LSP behaviour.
    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> jsonrpc::Result<Option<DocumentSymbolResponse>> {
        let uri = &params.text_document.uri;
        info!("📚 document_symbol: requested for {}", uri);

        let file_path = match uri.to_file_path() {
            Ok(p) => p,
            Err(_) => {
                info!("📚 document_symbol: URI did not convert to file path; returning None");
                return Ok(None);
            }
        };

        let entries = match self.salsa.get_document_symbols(file_path).await {
            Ok(Some(arc)) => arc,
            Ok(None) => {
                info!("📚 document_symbol: Salsa returned None (file not registered?); empty list");
                return Ok(Some(DocumentSymbolResponse::Nested(Vec::new())));
            }
            Err(e) => {
                info!("📚 document_symbol: Salsa error '{}'; empty list", e);
                return Ok(Some(DocumentSymbolResponse::Nested(Vec::new())));
            }
        };

        info!("📚 document_symbol: {} symbols for {}", entries.len(), uri);

        let symbols: Vec<DocumentSymbol> = entries.iter().map(symbol_entry_to_lsp).collect();
        Ok(Some(DocumentSymbolResponse::Nested(symbols)))
    }

    /// `textDocument/references` — find every parser-classified call site for
    /// the Laravel pattern under the cursor.
    ///
    /// The handler dispatches to [`laravel_lsp::references::
    /// classify_pattern_at_cursor`] to identify which pattern kind + name the
    /// cursor is on. We never raw-shape-match: positions the parser hasn't
    /// classified as a Laravel pattern are not considered. The Salsa actor
    /// then walks every registered project file and returns positions tagged
    /// with the matching kind + name.
    async fn references(&self, params: ReferenceParams) -> jsonrpc::Result<Option<Vec<Location>>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;

        let file_path = match uri.to_file_path() {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };

        let path_str = match file_path.to_str() {
            Some(s) => s,
            None => return Ok(None),
        };
        if !path_str.ends_with(".php") && !path_str.ends_with(".blade.php") {
            return Ok(None);
        }

        let patterns = match self.salsa.get_patterns(file_path.clone()).await {
            Ok(Some(p)) => p,
            _ => return Ok(None),
        };

        let symbol = match classify_with_decl_fallback(self, &file_path, &patterns, position).await
        {
            Some(s) => s,
            None => return Ok(None),
        };

        let include_declaration = params.context.include_declaration;

        // Call sites come from Salsa — every parser-classified position.
        let call_sites = match self
            .salsa
            .find_references(symbol.to_data(), include_declaration)
            .await
        {
            Ok(locs) => locs,
            Err(e) => {
                debug!("references: Salsa error {:?}", e);
                return Ok(None);
            }
        };

        let mut lsp_locations: Vec<Location> = call_sites
            .into_iter()
            .filter_map(|loc| reference_location_to_lsp(&loc))
            .collect();

        // Declaration sites come from per-kind tree-sitter walkers (same
        // walkers rename uses). The parser's php.scm captures call sites
        // only — declaration sites like `->name('home')` in route files,
        // array keys in `config/<file>.php`, and array keys in
        // `lang/<locale>/<file>.php` aren't tagged as `route_refs` /
        // `config_refs` / `translation_refs`, so they have to be found
        // separately. Per the LSP spec (and Zed's default), we include them
        // when `include_declaration` is set.
        if include_declaration {
            let root_path = self.root_path.read().await.clone();
            if let Some(root) = root_path.as_ref() {
                lsp_locations.extend(collect_declaration_locations(self, root, &symbol).await);
            }
        }

        Ok(Some(lsp_locations))
    }

    /// `textDocument/prepareRename` — decide whether the symbol under the
    /// cursor is renameable, and if so, return the range the editor should
    /// highlight + use as the initial input. Returning `None` makes the
    /// editor refuse the rename.
    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> jsonrpc::Result<Option<PrepareRenameResponse>> {
        let uri = &params.text_document.uri;
        let position = params.position;

        let file_path = match uri.to_file_path() {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };
        let path_str = match file_path.to_str() {
            Some(s) => s,
            None => return Ok(None),
        };
        if !path_str.ends_with(".php") && !path_str.ends_with(".blade.php") {
            return Ok(None);
        }

        let patterns = match self.salsa.get_patterns(file_path.clone()).await {
            Ok(Some(p)) => p,
            _ => return Ok(None),
        };

        let symbol = match classify_with_decl_fallback(self, &file_path, &patterns, position).await
        {
            Some(s) => {
                debug!(
                    "prepare_rename: classified as {:?} at {}:{}",
                    s, position.line, position.character
                );
                s
            }
            None => {
                debug!(
                    "prepare_rename: no classifier match at {}:{}",
                    position.line, position.character
                );
                return Ok(None);
            }
        };

        if !laravel_lsp::rename::can_rename(&symbol) {
            // Surface as an LSP error rather than `Ok(None)` so Zed shows
            // the user a status-bar message instead of silently dropping
            // the F2. `Ok(None)` is reserved for "cursor isn't on any
            // classified Laravel pattern" — silent is correct UX there.
            return Err(laravel_lsp::rename::unsupported_rename_error(&symbol));
        }

        // For declaration-cursor cases, the pattern range from
        // `pattern_range_at` is None (the parser didn't classify the
        // position). Fall back to the locator-reported decl range.
        let pattern_range = pattern_range_at(&patterns, position.line, position.character);
        let range = match pattern_range {
            Some(r) => Some(r),
            None => decl_range_at(self, &file_path, position, &symbol).await,
        };
        match &range {
            Some(r) => debug!(
                "prepare_rename: returning Range {}:{}..{}:{} for {:?}",
                r.start.line, r.start.character, r.end.line, r.end.character, symbol
            ),
            None => debug!("prepare_rename: returning None for {:?}", symbol),
        }
        Ok(range.map(PrepareRenameResponse::Range))
    }

    /// `textDocument/rename` — produce a `WorkspaceEdit` rewriting every
    /// parser-classified call site for the symbol under the cursor PLUS the
    /// declaration site for that symbol. The "instance chain" rule applies:
    /// edits target only positions the tree-sitter parser already tagged.
    async fn rename(&self, params: RenameParams) -> jsonrpc::Result<Option<WorkspaceEdit>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let new_name = params.new_name;
        debug!(
            "rename request: {} at {}:{} → new_name='{}'",
            uri, position.line, position.character, new_name
        );

        let file_path = match uri.to_file_path() {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };
        let path_str = match file_path.to_str() {
            Some(s) => s,
            None => return Ok(None),
        };
        if !path_str.ends_with(".php") && !path_str.ends_with(".blade.php") {
            return Ok(None);
        }

        let patterns = match self.salsa.get_patterns(file_path.clone()).await {
            Ok(Some(p)) => p,
            _ => return Ok(None),
        };

        let symbol = match classify_with_decl_fallback(self, &file_path, &patterns, position).await
        {
            Some(s) => {
                debug!("rename: classified as {:?}", s);
                s
            }
            None => {
                debug!(
                    "rename: classify returned None at {}:{}",
                    position.line, position.character
                );
                return Ok(None);
            }
        };

        if !laravel_lsp::rename::can_rename(&symbol) {
            // Defensive: `prepare_rename` should already have errored, but
            // a client that skipped the prepare round-trip would land here
            // — match the same user-facing error rather than no-op.
            debug!("rename: can_rename rejected {:?}", symbol);
            return Err(laravel_lsp::rename::unsupported_rename_error(&symbol));
        }

        // Call-site references via Salsa. These are always parser-classified.
        let call_sites = match self.salsa.find_references(symbol.to_data(), true).await {
            Ok(refs) => {
                debug!("rename: found {} call site(s) for {:?}", refs.len(), symbol);
                refs
            }
            Err(e) => {
                debug!("rename: find_references error {:?}", e);
                return Ok(None);
            }
        };

        // Call-site rewrite text: the full `new_name` as the user typed it.
        // Decl-site rewrite text is computed per-kind below — it may differ.
        let mut targets: Vec<laravel_lsp::rename::EditTarget> = call_sites
            .into_iter()
            .map(|r| laravel_lsp::rename::EditTarget {
                file_path: r.file_path,
                line: r.line,
                start_column: r.column,
                end_column: r.end_column,
                new_text: new_name.clone(),
            })
            .collect();

        // For Blade components, call-site ranges cover the full `x-name`
        // tag-name node — rewriting with a bare name would produce `<foo>`
        // and break the tag. Override each call-site `new_text` with the
        // `x-`-prefixed form. The user's typed prefix (if any) is
        // normalized in the Component arm below; here we re-prefix once
        // for every call site so source tags stay valid.
        if matches!(&symbol, laravel_lsp::references::SymbolRef::Component(_)) {
            let prefixed = format!("x-{}", new_name.trim().trim_start_matches("x-"));
            for target in &mut targets {
                target.new_text = prefixed.clone();
            }
        }

        // For Livewire, call sites come in two flavors with different
        // ranges: tag `<livewire:counter>` (range covers the full
        // `livewire:counter`) and directive `@livewire('counter')`
        // (range covers just `counter`). Differentiate by span length —
        // if the original span is longer than the OLD symbol name, the
        // prefix is included in the range and we re-prefix on rewrite.
        if let laravel_lsp::references::SymbolRef::Livewire(old_name) = &symbol {
            let bare_new = new_name.trim().trim_start_matches("livewire:").to_string();
            let tag_new_text = format!("livewire:{}", bare_new);
            let old_chars = old_name.chars().count() as u32;
            for target in &mut targets {
                let span = target.end_column.saturating_sub(target.start_column);
                if span > old_chars {
                    target.new_text = tag_new_text.clone();
                } else {
                    target.new_text = bare_new.clone();
                }
            }
        }

        // Declaration sites — per-kind walkers, tree-sitter based.
        // `can_rename` above is the gate: a symbol only reaches here when its
        // declaration walker is implemented. New kinds extend both `can_rename`
        // and this match arm together.
        //
        // Phase 3+: kinds whose "declaration" is a file (not a source
        // position) push into `file_renames` instead of `targets`. Both
        // travel together in the final WorkspaceEdit.
        let root_path = self.root_path.read().await.clone();
        let mut file_renames: Vec<laravel_lsp::rename::FileRename> = Vec::new();
        match &symbol {
            laravel_lsp::references::SymbolRef::Route(name) => {
                // Route names are written verbatim at every declaration site
                // (the locator emits one target per `->name(...)` segment;
                // group prefixes get their own segments).
                if let Some(root) = root_path.as_ref() {
                    targets.extend(
                        collect_route_declaration_targets(self, root, name, &new_name).await,
                    );
                }
            }
            laravel_lsp::references::SymbolRef::Config(key) => {
                // Config decl writes only the LEAF segment of the new dotted
                // form (e.g. "app.name" → "app.label" writes "label" at the
                // key position in config/app.php). The file portion can't
                // change without moving the config file.
                if let Some(root) = root_path.as_ref() {
                    if let Some(t) = collect_config_declaration_target(root, key, &new_name) {
                        targets.push(t);
                    }
                }
            }
            laravel_lsp::references::SymbolRef::Translation(key) => {
                // Same shape as config but applied across every locale's lang
                // file under lang/<locale>/<file>.php.
                if let Some(root) = root_path.as_ref() {
                    targets.extend(collect_translation_declaration_targets(
                        root, key, &new_name,
                    ));
                }
            }
            laravel_lsp::references::SymbolRef::Env(key) => {
                // Env keys aren't dotted — the new name is written verbatim
                // at every declaration AND every call site. Touches every
                // `.env*` file at the project root that has the key.
                if let Some(root) = root_path.as_ref() {
                    targets.extend(collect_env_declaration_targets(root, key, &new_name));
                }
            }
            laravel_lsp::references::SymbolRef::Component(name) => {
                // Blade `<x-...>` components. Two shapes:
                //   - Anonymous: just a .blade.php file.
                //   - Class-based: .blade.php + a PHP class file under
                //     app/View/Components/ with `class X extends Component`
                //     and a `namespace App\View\Components[\Sub];` line.
                //
                // For class-based, rename moves both files AND rewrites the
                // class name (always) and the namespace declaration (only
                // when the move crosses directories, changing the
                // conventional namespace).
                // Defensive: if the user types the `x-` Blade-tag prefix
                // out of habit (the rename input itself excludes it, but
                // they might paste from memory), strip it before
                // resolving paths. Without this, `x-foo` would resolve
                // to `components/x-foo.blade.php` and brick the component.
                let trimmed_raw = new_name.trim();
                let trimmed_new = trimmed_raw.strip_prefix("x-").unwrap_or(trimmed_raw);
                if let Err(e) =
                    laravel_lsp::component_declaration_locator::validate_component_name(trimmed_new)
                {
                    return Err(laravel_lsp::rename::rename_error(format!(
                        "invalid component name: {}",
                        e.message()
                    )));
                }
                if name == trimmed_new {
                    return Err(laravel_lsp::rename::rename_error(
                        "new component name must differ from the current name.",
                    ));
                }
                let Some(config) = self.get_cached_config().await else {
                    return Err(laravel_lsp::rename::rename_error(
                        "Laravel config not available; cannot resolve component paths.",
                    ));
                };
                let Some(current) =
                    laravel_lsp::component_declaration_locator::locate_component(name, &config)
                else {
                    return Err(laravel_lsp::rename::rename_error(format!(
                        "no Blade or class file found for component '{}'.",
                        name
                    )));
                };

                // Vendor refusal — both file paths must live in the project.
                if let Some(root) = root_path.as_ref() {
                    let vendor_blade = current.blade_file.as_ref().is_some_and(|p| {
                        laravel_lsp::component_declaration_locator::is_under_vendor(p, root)
                    });
                    let vendor_class = current.class_file.as_ref().is_some_and(|p| {
                        laravel_lsp::component_declaration_locator::is_under_vendor(p, root)
                    });
                    if vendor_blade || vendor_class {
                        return Err(laravel_lsp::rename::rename_error(
                            "cannot rename components located under vendor/.",
                        ));
                    }
                }

                // Target blade path — only when a blade file exists.
                if let Some(current_blade) = &current.blade_file {
                    let Some(target_blade) =
                        laravel_lsp::component_declaration_locator::compute_blade_target_path(
                            name,
                            trimmed_new,
                            current_blade,
                            &config,
                        )
                    else {
                        return Err(laravel_lsp::rename::rename_error(format!(
                            "could not compute target path for component blade file '{}'.",
                            trimmed_new
                        )));
                    };
                    file_renames.push(laravel_lsp::rename::FileRename {
                        old_path: current_blade.clone(),
                        new_path: target_blade,
                    });
                }

                // Target class file + declaration rewrites — only when a
                // class file exists.
                if let Some(current_class) = &current.class_file {
                    let target_class =
                        laravel_lsp::component_declaration_locator::conventional_class_file_path(
                            trimmed_new,
                            &config,
                        );
                    if &target_class != current_class {
                        file_renames.push(laravel_lsp::rename::FileRename {
                            old_path: current_class.clone(),
                            new_path: target_class.clone(),
                        });
                    }

                    // Class name rewrite — fires whenever the leaf Pascal-
                    // form differs (almost always, since rename = name
                    // change).
                    let new_class_name =
                        laravel_lsp::component_declaration_locator::class_name_for(trimmed_new);
                    if let Some(span) = &current.class_declaration {
                        if span.current_text != new_class_name {
                            targets.push(laravel_lsp::rename::EditTarget {
                                file_path: span.file_path.clone(),
                                line: span.line,
                                start_column: span.start_column,
                                end_column: span.end_column,
                                new_text: new_class_name,
                            });
                        }
                    }

                    // Namespace rewrite — only when the file moved into a
                    // different conventional namespace.
                    if let Some(root) = root_path.as_ref() {
                        let new_namespace =
                            laravel_lsp::component_declaration_locator::conventional_namespace_for(
                                &target_class,
                                root,
                            );
                        if let Some(span) = &current.namespace_declaration {
                            if !new_namespace.is_empty() && span.current_text != new_namespace {
                                targets.push(laravel_lsp::rename::EditTarget {
                                    file_path: span.file_path.clone(),
                                    line: span.line,
                                    start_column: span.start_column,
                                    end_column: span.end_column,
                                    new_text: new_namespace,
                                });
                            }
                        }
                    }
                }
            }
            laravel_lsp::references::SymbolRef::Livewire(name) => {
                debug!("rename Livewire: old='{}', new='{}'", name, new_name);
                // Livewire dispatches by kind:
                //   - V4 SFC: rename single .blade.php
                //   - V4 MFC: rename each child file (empty dir left behind)
                //   - V3 Class: rename class file + view file + class decl
                //               + namespace decl (if cross-dir)
                //   - Volt: rename single .blade.php
                let trimmed_raw = new_name.trim();
                let trimmed_new = trimmed_raw.strip_prefix("livewire:").unwrap_or(trimmed_raw);
                if let Err(e) =
                    laravel_lsp::livewire_declaration_locator::validate_livewire_name(trimmed_new)
                {
                    return Err(laravel_lsp::rename::rename_error(format!(
                        "invalid Livewire component name: {}",
                        e.message()
                    )));
                }
                if name == trimmed_new {
                    return Err(laravel_lsp::rename::rename_error(
                        "new Livewire component name must differ from the current name.",
                    ));
                }
                let Some(root) = root_path.as_ref() else {
                    return Err(laravel_lsp::rename::rename_error(
                        "project root not yet known; cannot resolve Livewire paths.",
                    ));
                };
                let (livewire_config, livewire_version) = match self.get_cached_livewire().await {
                    Some(v) => v,
                    None => {
                        return Err(laravel_lsp::rename::rename_error(
                            "could not load Livewire configuration.",
                        ));
                    }
                };
                info!(
                    "   livewire version: {:?}, class_path: {}, view_path: {}, class_namespace: {}",
                    livewire_version,
                    livewire_config.class_path.display(),
                    livewire_config.view_path.display(),
                    livewire_config.class_namespace
                );
                let current = match laravel_lsp::livewire_declaration_locator::locate(
                    name,
                    &livewire_config,
                    livewire_version,
                ) {
                    Some(c) => c,
                    None => {
                        // Conventional Livewire paths missed. Try the
                        // generic Laravel view-path resolver — this
                        // covers two distinct cases:
                        //   1. View-only Livewire components (no class
                        //      file; component is registered explicitly
                        //      via `Livewire::component(...)`).
                        //   2. Vendor-registered components whose views
                        //      were published to a non-conventional path
                        //      (e.g., Jetstream → `resources/views/api/`).
                        //
                        // We can rename case 1 (view + call sites). Case 2
                        // we refuse because moving the published view
                        // breaks the package's registered name. The two
                        // are distinguished by whether the matching view
                        // is under `vendor/`.
                        let Some(laravel_cfg) = self.get_cached_config().await else {
                            return Err(laravel_lsp::rename::rename_error(
                                "Laravel config not available for view-path fallback.",
                            ));
                        };
                        let Some(view_path) = laravel_cfg
                            .resolve_view_path(name)
                            .into_iter()
                            .find(|p| p.exists())
                        else {
                            info!(
                                "   locate('{}') returned None and view-path fallback also missed",
                                name
                            );
                            return Err(laravel_lsp::rename::rename_error(format!(
                                "Cannot find Livewire component '{}' anywhere — neither \
                                 the conventional Livewire paths nor the view-path \
                                 resolver matched.",
                                name
                            )));
                        };
                        info!(
                            "   locate('{}') returned None; view-only fallback found view at {}",
                            name,
                            view_path.display()
                        );

                        // Vendor refusal — published views from a vendor
                        // package should not be renamed locally (the
                        // package's registration still points at the old
                        // name and would break).
                        if let Some(root) = root_path.as_ref() {
                            if laravel_lsp::view_declaration_locator::is_under_vendor(
                                &view_path, root,
                            ) {
                                return Err(laravel_lsp::rename::rename_error(format!(
                                    "Cannot rename Livewire component '{}': the only \
                                     matching view lives under vendor/.",
                                    name
                                )));
                            }
                        }

                        // Compute the target view path. Reuse the View
                        // rename's logic — it preserves the same
                        // view_paths root the source file sits under.
                        let Some(target_view) =
                            laravel_lsp::view_declaration_locator::compute_target_path(
                                name,
                                trimmed_new,
                                &view_path,
                                &laravel_cfg,
                            )
                        else {
                            return Err(laravel_lsp::rename::rename_error(format!(
                                "could not compute target view path for view-only \
                                 Livewire rename to '{}'.",
                                trimmed_new
                            )));
                        };

                        file_renames.push(laravel_lsp::rename::FileRename {
                            old_path: view_path,
                            new_path: target_view,
                        });

                        // Short-circuit out of the Livewire arm — no
                        // class file or declaration to rewrite. The
                        // call-site text edits in `targets` (already
                        // built above) carry the new name; the
                        // file_renames vec carries the file move.
                        return Ok(laravel_lsp::rename::build_rename_workspace_edit(
                            &targets,
                            &file_renames,
                        ));
                    }
                };
                info!(
                    "   located kind={:?}, paths={:?}",
                    current.kind, current.paths
                );

                // Vendor refusal — checks the primary file (paths[0]).
                if let Some(primary) = current.paths.first() {
                    if laravel_lsp::livewire_declaration_locator::is_under_vendor(primary, root) {
                        return Err(laravel_lsp::rename::rename_error(
                            "cannot rename Livewire components located under vendor/.",
                        ));
                    }
                }

                let Some(target) = laravel_lsp::livewire_declaration_locator::compute_target_paths(
                    name,
                    trimmed_new,
                    &current,
                    &livewire_config,
                ) else {
                    debug!("compute_target_paths returned None");
                    return Err(laravel_lsp::rename::rename_error(format!(
                        "could not compute target paths for Livewire rename to '{}'.",
                        trimmed_new
                    )));
                };
                debug!("target paths: {:?}", target.paths);

                // Emit FileRenames. For V4 MFC, skip the directory entry
                // at paths[0] — emitting it as a RenameFile would race
                // with the child renames (sequencing tangles). Children
                // move individually; the now-empty old directory is left
                // behind as a known small-cleanup item.
                let pair_start = if current.kind
                    == laravel_lsp::livewire_resolver::LivewireComponentKind::V4Mfc
                {
                    1
                } else {
                    0
                };
                for (old, new) in current.paths[pair_start..].iter().zip(target.paths.iter()) {
                    if old == new {
                        continue;
                    }
                    file_renames.push(laravel_lsp::rename::FileRename {
                        old_path: old.clone(),
                        new_path: new.clone(),
                    });
                }

                // V3 Class: rewrite class declaration + namespace
                // declaration when they change.
                if current.kind == laravel_lsp::livewire_resolver::LivewireComponentKind::V3Class {
                    let new_class_name =
                        laravel_lsp::component_declaration_locator::class_name_for(trimmed_new);
                    if let Some(span) = &current.class_declaration {
                        if span.current_text != new_class_name {
                            targets.push(laravel_lsp::rename::EditTarget {
                                file_path: span.file_path.clone(),
                                line: span.line,
                                start_column: span.start_column,
                                end_column: span.end_column,
                                new_text: new_class_name,
                            });
                        }
                    }
                    // Namespace tracks the new class file's parent dir
                    // under the Laravel `app/` autoload root.
                    if let Some(new_class_path) = target.paths.first() {
                        let new_namespace =
                            laravel_lsp::component_declaration_locator::conventional_namespace_for(
                                new_class_path,
                                root,
                            );
                        if let Some(span) = &current.namespace_declaration {
                            if !new_namespace.is_empty() && span.current_text != new_namespace {
                                targets.push(laravel_lsp::rename::EditTarget {
                                    file_path: span.file_path.clone(),
                                    line: span.line,
                                    start_column: span.start_column,
                                    end_column: span.end_column,
                                    new_text: new_namespace,
                                });
                            }
                        }
                    }
                }
            }
            laravel_lsp::references::SymbolRef::View(name) => {
                // Views don't have a textual declaration — the file IS the
                // declaration. Rename = move the .blade.php + rewrite every
                // `view('old')` call site (call sites already collected
                // above into `targets`). New name validation, vendor-path
                // refusal, and target-path computation surface as toasts
                // via short-circuit errors rather than silent no-ops.
                let trimmed_new = new_name.trim();
                if let Err(e) =
                    laravel_lsp::view_declaration_locator::validate_view_name(trimmed_new)
                {
                    return Err(laravel_lsp::rename::rename_error(format!(
                        "invalid view name: {}",
                        e.message()
                    )));
                }
                if name == trimmed_new {
                    return Err(laravel_lsp::rename::rename_error(
                        "new view name must differ from the current name.",
                    ));
                }
                let Some(config) = self.get_cached_config().await else {
                    return Err(laravel_lsp::rename::rename_error(
                        "Laravel config not available; cannot resolve view paths.",
                    ));
                };
                let Some(current_path) =
                    laravel_lsp::view_declaration_locator::locate_view_file(name, &config)
                else {
                    return Err(laravel_lsp::rename::rename_error(format!(
                        "view file for '{}' not found on disk.",
                        name
                    )));
                };
                if let Some(root) = root_path.as_ref() {
                    if laravel_lsp::view_declaration_locator::is_under_vendor(&current_path, root) {
                        return Err(laravel_lsp::rename::rename_error(
                            "cannot rename views located under vendor/.",
                        ));
                    }
                }
                let Some(target_path) = laravel_lsp::view_declaration_locator::compute_target_path(
                    name,
                    trimmed_new,
                    &current_path,
                    &config,
                ) else {
                    return Err(laravel_lsp::rename::rename_error(format!(
                        "could not compute target path for renamed view '{}'.",
                        trimmed_new
                    )));
                };
                file_renames.push(laravel_lsp::rename::FileRename {
                    old_path: current_path,
                    new_path: target_path,
                });
            }
            laravel_lsp::references::SymbolRef::Middleware(alias) => {
                // Middleware aliases register as a quoted string at a single
                // line in Kernel.php / bootstrap/app.php (Laravel 11+) or in
                // a custom service-provider `register()`. The lookup gives
                // us source_file + source_line; we then scan that line for
                // the quoted alias to get the exact column span. Call sites
                // already collected via Salsa; only the registration site
                // is added here.
                //
                // Parameter forms like `auth:sanctum` aren't renameable —
                // the cursor is on a use site that includes guard params,
                // not the alias itself. Refuse with a clear toast rather
                // than partially-rewriting and losing the `:sanctum` tail.
                if alias.contains(':') {
                    return Err(laravel_lsp::rename::rename_error(
                        "cannot rename a parameterized middleware reference \
                         (e.g. 'auth:sanctum'); rename the bare alias \
                         instead.",
                    ));
                }
                let trimmed_new = new_name.trim();
                if let Err(e) =
                    laravel_lsp::middleware_binding_locator::validate_alias_name(trimmed_new)
                {
                    return Err(laravel_lsp::rename::rename_error(format!(
                        "invalid middleware alias: {}",
                        e.message()
                    )));
                }
                if alias == trimmed_new {
                    return Err(laravel_lsp::rename::rename_error(
                        "new middleware alias must differ from the current name.",
                    ));
                }
                let Some((_class, _class_file, source_file, source_line)) =
                    self.get_cached_middleware(alias).await
                else {
                    return Err(laravel_lsp::rename::rename_error(format!(
                        "middleware alias '{}' not found in registry.",
                        alias
                    )));
                };
                let Some(source_path) = source_file else {
                    return Err(laravel_lsp::rename::rename_error(format!(
                        "registration site for middleware alias '{}' is unknown.",
                        alias
                    )));
                };
                if let Some(root) = root_path.as_ref() {
                    if laravel_lsp::view_declaration_locator::is_under_vendor(&source_path, root) {
                        return Err(laravel_lsp::rename::rename_error(
                            "cannot rename middleware aliases registered under vendor/.",
                        ));
                    }
                }
                let Some(line_1based) = source_line else {
                    return Err(laravel_lsp::rename::rename_error(format!(
                        "registration line for middleware alias '{}' is unknown.",
                        alias
                    )));
                };
                let Some(span) = laravel_lsp::middleware_binding_locator::locate_alias_on_line(
                    &source_path,
                    line_1based,
                    alias,
                ) else {
                    return Err(laravel_lsp::rename::rename_error(format!(
                        "could not locate quoted alias '{}' on line {} of {}.",
                        alias,
                        line_1based,
                        source_path.display()
                    )));
                };
                targets.push(laravel_lsp::rename::EditTarget {
                    file_path: source_path,
                    line: span.line,
                    start_column: span.start_column,
                    end_column: span.end_column,
                    new_text: trimmed_new.to_string(),
                });
            }
            laravel_lsp::references::SymbolRef::Binding(name) => {
                // Container bindings (`$this->app->bind('cache', ...)`,
                // `app()->singleton(...)`, etc.) follow the same shape as
                // middleware aliases: a quoted name on a single source
                // line. Same vendor refusal, same locator. Kept as a
                // separate arm because the error messages need to name
                // the right symbol kind for the user-facing toast.
                let trimmed_new = new_name.trim();
                if let Err(e) =
                    laravel_lsp::middleware_binding_locator::validate_alias_name(trimmed_new)
                {
                    return Err(laravel_lsp::rename::rename_error(format!(
                        "invalid binding name: {}",
                        e.message()
                    )));
                }
                if name == trimmed_new {
                    return Err(laravel_lsp::rename::rename_error(
                        "new binding name must differ from the current name.",
                    ));
                }
                let Some((_class, _class_file, source_file, source_line)) =
                    self.get_cached_binding(name).await
                else {
                    return Err(laravel_lsp::rename::rename_error(format!(
                        "binding '{}' not found in registry.",
                        name
                    )));
                };
                let Some(source_path) = source_file else {
                    return Err(laravel_lsp::rename::rename_error(format!(
                        "registration site for binding '{}' is unknown.",
                        name
                    )));
                };
                if let Some(root) = root_path.as_ref() {
                    if laravel_lsp::view_declaration_locator::is_under_vendor(&source_path, root) {
                        return Err(laravel_lsp::rename::rename_error(
                            "cannot rename bindings registered under vendor/.",
                        ));
                    }
                }
                let Some(line_1based) = source_line else {
                    return Err(laravel_lsp::rename::rename_error(format!(
                        "registration line for binding '{}' is unknown.",
                        name
                    )));
                };
                let Some(span) = laravel_lsp::middleware_binding_locator::locate_alias_on_line(
                    &source_path,
                    line_1based,
                    name,
                ) else {
                    return Err(laravel_lsp::rename::rename_error(format!(
                        "could not locate quoted binding name '{}' on line {} of {}.",
                        name,
                        line_1based,
                        source_path.display()
                    )));
                };
                targets.push(laravel_lsp::rename::EditTarget {
                    file_path: source_path,
                    line: span.line,
                    start_column: span.start_column,
                    end_column: span.end_column,
                    new_text: trimmed_new.to_string(),
                });
            }
        }

        let edit = laravel_lsp::rename::build_rename_workspace_edit(&targets, &file_renames);
        debug!(
            "rename: built WorkspaceEdit with {} text edit(s) + {} file rename(s) → {}",
            targets.len(),
            file_renames.len(),
            if edit.is_some() {
                "Some(WorkspaceEdit)"
            } else {
                "None"
            },
        );
        Ok(edit)
    }

    /// `workspace/willRenameFiles` — Phase 3d. Fires when the user
    /// renames a file in Zed's file tree (right-click → rename). We
    /// return text edits to rewrite every call site that referenced
    /// the old name, and the client applies them atomically with the
    /// file rename.
    ///
    /// Today: handles `.blade.php` files classified as either a view
    /// (`view('users.index')`) or a Blade component (`<x-button>`).
    /// PHP class file renames (component classes, Livewire classes)
    /// could be added later — the capability filter would need to
    /// include `**/*.php` first.
    async fn will_rename_files(
        &self,
        params: RenameFilesParams,
    ) -> jsonrpc::Result<Option<WorkspaceEdit>> {
        debug!(
            "will_rename_files: {} file(s) — {:?}",
            params.files.len(),
            params
                .files
                .iter()
                .map(|f| format!("{} → {}", f.old_uri, f.new_uri))
                .collect::<Vec<_>>()
        );

        let Some(config) = self.get_cached_config().await else {
            debug!("warn:  will_rename_files: no Laravel config cached, skipping");
            return Ok(None);
        };

        let mut targets: Vec<laravel_lsp::rename::EditTarget> = Vec::new();

        for file_rename in &params.files {
            let Some(targets_for_file) =
                self.collect_will_rename_targets(file_rename, &config).await
            else {
                debug!(
                    "will_rename_files: {} → {} not classified (skipping)",
                    file_rename.old_uri, file_rename.new_uri
                );
                continue;
            };
            targets.extend(targets_for_file);
        }

        // No FileRename ops here — Zed is doing the file move itself.
        // We only contribute the text edits that keep references valid.
        let edit = laravel_lsp::rename::build_rename_workspace_edit(&targets, &[]);
        debug!(
            "will_rename_files: returning {} ({} total text edits)",
            if edit.is_some() {
                "WorkspaceEdit"
            } else {
                "None"
            },
            targets.len()
        );
        Ok(edit)
    }

    /// Handle code action requests (quick fixes like "Create missing view")
    async fn code_action(
        &self,
        params: CodeActionParams,
    ) -> jsonrpc::Result<Option<CodeActionResponse>> {
        let uri = &params.text_document.uri;
        let context = &params.context;

        // Early return if no diagnostics in context
        if context.diagnostics.is_empty() {
            return Ok(None);
        }

        info!(
            "🔧 code_action called for {} with {} diagnostics",
            uri,
            context.diagnostics.len()
        );

        let mut actions = Vec::new();

        // Get root path for Livewire (needs to calculate view path)
        let root_guard = self.root_path.read().await;
        let root = root_guard.as_ref().map(|p| p.as_path());

        // Process each diagnostic to see if we can offer a fix
        for diagnostic in &context.diagnostics {
            // Check if this is our diagnostic (source: laravel)
            if diagnostic.source.as_deref() != Some("laravel") {
                continue;
            }

            // Parse diagnostic into FileAction(s) - may return multiple options
            let file_actions = FileAction::from_diagnostic(&diagnostic.message);
            for file_action in file_actions {
                let template = self.get_stub_content(&file_action).await;

                if let Some(code_action) = file_action.build_code_action(template, diagnostic, root)
                {
                    actions.push(code_action);
                }
            }
        }
        drop(root_guard);

        if actions.is_empty() {
            Ok(None)
        } else {
            info!("🔧 Returning {} code actions", actions.len());
            Ok(Some(actions))
        }
    }

    async fn completion(
        &self,
        params: CompletionParams,
    ) -> jsonrpc::Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;

        info!(
            "📝 completion called for {}:{}:{}",
            uri, position.line, position.character
        );

        // Get document content
        let documents = self.documents.read().await;
        let (content, _version) = match documents.get(uri) {
            Some(doc) => doc.clone(),
            None => {
                debug!("   Document not found in cache");
                return Ok(None);
            }
        };
        drop(documents);

        // Get the current line
        let lines: Vec<&str> = content.lines().collect();
        let line_text = match lines.get(position.line as usize) {
            Some(line) => *line,
            None => return Ok(None),
        };

        // Determine context based on file type
        let path = uri.path();
        let is_env_file = path.contains(".env");
        let is_phpunit_file = path.ends_with("phpunit.xml")
            || path.ends_with("phpunit.xml.dist")
            || path.ends_with("phpunit.dist.xml");
        let is_php_or_blade = path.ends_with(".php") || path.ends_with(".blade.php");

        // In PHP/Blade files, check for various contexts
        if is_php_or_blade {
            // Eloquent / DB query builder chain completion. No line-local
            // pre-filter: chains can span multiple lines (the `->` may live
            // on a different line than the cursor), and the helper itself
            // short-circuits fast (single Arc::clone + emptiness check) when
            // the file has no chains. Files with chains pay an O(chains)
            // walk which is in the tens.
            if let Some(items) = self
                .try_query_chain_completion(&content, position, uri)
                .await
            {
                if !items.is_empty() {
                    return Ok(Some(CompletionResponse::List(CompletionList {
                        is_incomplete: false,
                        items,
                    })));
                }
            }

            // Check for variable name context in Blade files (typing $user, $u, etc.)
            // This must come BEFORE model property context to avoid conflicts
            if uri.path().ends_with(".blade.php") {
                if let Some(var_prefix) =
                    Self::get_variable_name_context(line_text, position.character)
                {
                    debug!(
                        "   Variable name context in Blade, prefix: '{}'",
                        var_prefix
                    );

                    // Get all available variables for this Blade file (with loop scope awareness)
                    let variables = self.get_blade_available_variables(
                        uri,
                        Some(&content),
                        Some(position.line),
                    );

                    // Filter by prefix and build completion items
                    let prefix_lower = var_prefix.to_lowercase();
                    let items: Vec<CompletionItem> = variables
                        .into_iter()
                        .filter(|v| v.name.to_lowercase().starts_with(&prefix_lower))
                        .map(|v| {
                            CompletionItem {
                                label: format!("${}", v.name),
                                kind: Some(CompletionItemKind::VARIABLE),
                                detail: Some(format!("{} ({})", v.php_type, v.source)),
                                insert_text: Some(v.name.clone()), // Insert without $ since user already typed it
                                documentation: None,
                                ..Default::default()
                            }
                        })
                        .collect();

                    debug!("   Returning {} variable completion items", items.len());

                    if !items.is_empty() {
                        return Ok(Some(CompletionResponse::List(CompletionList {
                            is_incomplete: false,
                            items,
                        })));
                    }
                }

                // Check for Blade directive context (typing @if, @foreach, etc.)
                if let Some(directive_prefix) =
                    Self::get_blade_directive_context(line_text, position.character)
                {
                    debug!("   Blade directive context, prefix: '{}'", directive_prefix);

                    // Get directive spacing preference
                    let use_spacing = *self.directive_spacing.read().await;
                    let paren = if use_spacing { " (" } else { "(" };

                    // Get discovered directives (from framework + app + packages)
                    let directives = {
                        let root_guard = self.root_path.read().await;
                        match root_guard.as_ref() {
                            Some(root) => get_all_blade_directives(root),
                            None => get_fallback_blade_directives(),
                        }
                    };

                    let prefix_lower = directive_prefix.to_lowercase();
                    let items: Vec<CompletionItem> = directives
                        .iter()
                        .filter(|d| d.name.to_lowercase().starts_with(&prefix_lower))
                        .map(|d| {
                            // Build snippet based on params and closing directive
                            // Use configured spacing: @if($1) or @if ($1)
                            let insert_text = match (d.has_params, &d.closing) {
                                // Block directive with params: @if($1)\n\t$0\n@endif
                                (true, Some(end)) => {
                                    format!("{}{}$1)\n\t$0\n@{}", d.name, paren, end)
                                }
                                // Block directive without params: @php\n\t$0\n@endphp
                                (false, Some(end)) => format!("{}\n\t$0\n@{}", d.name, end),
                                // Inline directive with params: @include($1)$0
                                (true, None) => format!("{}{}$1)$0", d.name, paren),
                                // Inline directive without params: @csrf
                                (false, None) => d.name.clone(),
                            };

                            let label = if let Some(ref end) = d.closing {
                                format!("@{}...@{}", d.name, end)
                            } else {
                                format!("@{}", d.name)
                            };

                            CompletionItem {
                                label,
                                kind: Some(CompletionItemKind::KEYWORD),
                                detail: Some(format!("{} ({})", d.description, d.source)),
                                insert_text: Some(insert_text),
                                insert_text_format: Some(InsertTextFormat::SNIPPET),
                                documentation: None,
                                ..Default::default()
                            }
                        })
                        .collect();

                    debug!("   Returning {} directive completion items", items.len());

                    if !items.is_empty() {
                        return Ok(Some(CompletionResponse::List(CompletionList {
                            is_incomplete: false,
                            items,
                        })));
                    }
                }

                // Check for Blade bracket context - show all options after first {
                let cursor_col = position.character as usize;
                let text_before = if cursor_col <= line_text.len() {
                    &line_text[..cursor_col]
                } else {
                    line_text
                };

                // Check if we're in a Blade bracket context (starts with {)
                // Find what the user has typed so far
                let (trigger_start, typed_prefix) = if text_before.ends_with("{{--") {
                    (4, "{{--")
                } else if text_before.ends_with("{{-") {
                    (3, "{{-")
                } else if text_before.ends_with("{!!") {
                    (3, "{!!")
                } else if text_before.ends_with("{!") {
                    (2, "{!")
                } else if text_before.ends_with("{{") {
                    (2, "{{")
                } else if text_before.ends_with("{") && !text_before.ends_with("${") {
                    (1, "{")
                } else {
                    (0, "")
                };

                if trigger_start > 0 {
                    let start_col = position.character.saturating_sub(trigger_start as u32);

                    // Check for closing characters after cursor that should be replaced
                    // (e.g., auto-closed `}`, `}}`, `--}}`, `!!}` from typing `{`, `{{`, etc.)
                    let text_after = if cursor_col < line_text.len() {
                        &line_text[cursor_col..]
                    } else {
                        ""
                    };

                    // Count how many trailing characters to replace after the cursor
                    let trailing_len = if text_after.starts_with("--}}") {
                        4
                    } else if text_after.starts_with("!!}") {
                        3
                    } else if text_after.starts_with("}}") {
                        2
                    } else if text_after.starts_with("}") {
                        1
                    } else {
                        0
                    };

                    let end_col = position.character + trailing_len as u32;

                    // Define all bracket snippets
                    let all_brackets = [
                        ("{{", "{{ $0 }}", "Echo (escaped)"),
                        ("{!!", "{!! $0 !!}", "Echo (unescaped)"),
                        ("{{--", "{{-- $0 --}}", "Blade comment"),
                    ];

                    // Filter to those matching what user has typed
                    let items: Vec<CompletionItem> = all_brackets
                        .iter()
                        .filter(|(trigger, _, _)| trigger.starts_with(typed_prefix))
                        .map(|(trigger, snippet, description)| {
                            CompletionItem {
                                label: snippet.replace(" $0 ", " ... "),
                                kind: Some(CompletionItemKind::SNIPPET),
                                detail: Some(description.to_string()),
                                insert_text_format: Some(InsertTextFormat::SNIPPET),
                                text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                                    range: Range {
                                        start: Position {
                                            line: position.line,
                                            character: start_col,
                                        },
                                        end: Position {
                                            line: position.line,
                                            character: end_col,
                                        },
                                    },
                                    new_text: snippet.to_string(),
                                })),
                                // Preselect {{ as the most common
                                preselect: Some(*trigger == "{{"),
                                sort_text: Some(match *trigger {
                                    "{{" => "0".to_string(),
                                    "{!!" => "1".to_string(),
                                    "{{--" => "2".to_string(),
                                    _ => "9".to_string(),
                                }),
                                ..Default::default()
                            }
                        })
                        .collect();

                    if !items.is_empty() {
                        debug!("   Returning {} Blade bracket snippets", items.len());
                        return Ok(Some(CompletionResponse::List(CompletionList {
                            is_incomplete: false,
                            items,
                        })));
                    }
                }
            }

            // Check for model property context ($user-> or User::find()->)
            if let Some((class_hint, typed_prefix)) =
                Self::get_model_property_context(line_text, position.character)
            {
                debug!(
                    "   Model property context, class hint: '{}', typed prefix: '{}'",
                    class_hint, typed_prefix
                );

                // Resolve the model class from the hint
                let model_class = if class_hint.starts_with('$') {
                    // Variable - try explicit type hints/PHPDoc in current file first
                    match Self::resolve_variable_type(&content, &class_hint) {
                        Some(t) => Some(t),
                        // For Blade files, try to resolve from the source that provides this variable
                        // (controller, Livewire component, view component, or view composer).
                        // Cannot use `.or_else()` here because the fallback is async.
                        None => self.resolve_blade_variable_type(uri, &class_hint).await,
                    }
                } else {
                    // Direct class name from static chain
                    Some(class_hint)
                };

                if let Some(class_name) = model_class {
                    debug!("   Resolved model class: {}", class_name);

                    // Get model properties
                    let properties = self.get_class_properties(&class_name).await;

                    // Build completion items, filtering by prefix
                    let prefix_lower = typed_prefix.to_lowercase();
                    let items: Vec<CompletionItem> = properties
                        .into_iter()
                        .filter(|p| p.name.to_lowercase().starts_with(&prefix_lower))
                        .map(|p| {
                            let kind = match p.source.as_str() {
                                "relationship" => CompletionItemKind::METHOD,
                                "accessor" => CompletionItemKind::PROPERTY,
                                _ => CompletionItemKind::FIELD,
                            };

                            // Hide the source label for generic class-property scans —
                            // it's redundant when the property completion is already
                            // attached to a class context. Eloquent-derived sources
                            // (database/cast/accessor/relationship) keep the label
                            // because the distinction is meaningful for users.
                            let detail = if p.source == "class" {
                                p.php_type.clone()
                            } else {
                                format!("{} ({})", p.php_type, p.source)
                            };

                            CompletionItem {
                                label: p.name.clone(),
                                kind: Some(kind),
                                detail: Some(detail),
                                documentation: None,
                                ..Default::default()
                            }
                        })
                        .collect();

                    debug!(
                        "   Returning {} model property completion items",
                        items.len()
                    );

                    if !items.is_empty() {
                        return Ok(Some(CompletionResponse::List(CompletionList {
                            is_incomplete: false,
                            items,
                        })));
                    }
                }
            }

            if let Some(config_ctx) = Self::get_config_call_context(line_text, position.character) {
                debug!("   Config context, filter prefix: '{}'", config_ctx.prefix);

                // Get all config keys
                let config_keys = self.get_all_config_keys().await;

                // Build completion items, filtering by prefix (case-sensitive)
                let items: Vec<CompletionItem> = config_keys
                    .into_iter()
                    .filter(|c| c.key.starts_with(&config_ctx.prefix))
                    .map(|c| {
                        let detail = if c.value.is_empty() {
                            format!("({})", c.source)
                        } else {
                            format!("{} ({})", c.value, c.source)
                        };

                        CompletionItem {
                            label: c.key.clone(),
                            kind: Some(CompletionItemKind::CONSTANT),
                            detail: Some(detail),
                            documentation: None,
                            text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                                range: Range {
                                    start: Position {
                                        line: position.line,
                                        character: config_ctx.start_col,
                                    },
                                    end: Position {
                                        line: position.line,
                                        character: config_ctx.end_col,
                                    },
                                },
                                new_text: c.key.clone(),
                            })),
                            ..Default::default()
                        }
                    })
                    .collect();

                debug!("   Returning {} config completion items", items.len());

                return if items.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(CompletionResponse::List(CompletionList {
                        is_incomplete: false,
                        items,
                    })))
                };
            }

            // Check for view context
            if let Some(view_ctx) = Self::get_view_call_context(line_text, position.character) {
                debug!("   View context, filter prefix: '{}'", view_ctx.prefix);

                // Get all view names
                let view_names = self.get_all_view_names().await;

                // Build completion items, filtering by prefix (case-insensitive)
                let prefix_lower = view_ctx.prefix.to_lowercase();
                let items: Vec<CompletionItem> = view_names
                    .into_iter()
                    .filter(|v| v.name.to_lowercase().starts_with(&prefix_lower))
                    .map(|v| CompletionItem {
                        label: v.name.clone(),
                        kind: Some(CompletionItemKind::FILE),
                        detail: Some(v.path),
                        documentation: None,
                        text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                            range: Range {
                                start: Position {
                                    line: position.line,
                                    character: view_ctx.start_col,
                                },
                                end: Position {
                                    line: position.line,
                                    character: view_ctx.end_col,
                                },
                            },
                            new_text: v.name.clone(),
                        })),
                        ..Default::default()
                    })
                    .collect();

                debug!("   Returning {} view completion items", items.len());

                return if items.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(CompletionResponse::List(CompletionList {
                        is_incomplete: false,
                        items,
                    })))
                };
            }

            // Check for Blade component context (<x-...)
            if let Some(component_ctx) =
                Self::get_blade_component_context(line_text, position.character)
            {
                debug!(
                    "   Blade component context, filter prefix: '{}'",
                    component_ctx.prefix
                );

                // Get all Blade components
                let components = self.get_all_blade_components().await;

                // Build completion items, filtering by prefix (case-insensitive)
                let prefix_lower = component_ctx.prefix.to_lowercase();
                let items: Vec<CompletionItem> = components
                    .into_iter()
                    .filter(|c| c.name.to_lowercase().starts_with(&prefix_lower))
                    .map(|c| CompletionItem {
                        label: c.name.clone(),
                        kind: Some(CompletionItemKind::CLASS),
                        detail: Some(c.path),
                        documentation: None,
                        text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                            range: Range {
                                start: Position {
                                    line: position.line,
                                    character: component_ctx.start_col,
                                },
                                end: Position {
                                    line: position.line,
                                    character: component_ctx.end_col,
                                },
                            },
                            new_text: c.name.clone(),
                        })),
                        ..Default::default()
                    })
                    .collect();

                debug!(
                    "   Returning {} Blade component completion items",
                    items.len()
                );

                return if items.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(CompletionResponse::List(CompletionList {
                        is_incomplete: false,
                        items,
                    })))
                };
            }

            // Check for Livewire component context (<livewire:... or @livewire('...'))
            if let Some(livewire_prefix) =
                Self::get_livewire_component_context(line_text, position.character)
            {
                debug!(
                    "   Livewire component context, filter prefix: '{}'",
                    livewire_prefix
                );

                // Get all Livewire components
                let components = self.get_all_livewire_components().await;

                // Build completion items, filtering by prefix (case-insensitive)
                let prefix_lower = livewire_prefix.to_lowercase();
                let items: Vec<CompletionItem> = components
                    .into_iter()
                    .filter(|c| c.name.to_lowercase().starts_with(&prefix_lower))
                    .map(|c| CompletionItem {
                        label: c.name.clone(),
                        kind: Some(CompletionItemKind::CLASS),
                        detail: Some(c.path),
                        documentation: None,
                        ..Default::default()
                    })
                    .collect();

                debug!(
                    "   Returning {} Livewire component completion items",
                    items.len()
                );

                return if items.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(CompletionResponse::List(CompletionList {
                        is_incomplete: false,
                        items,
                    })))
                };
            }

            // Check for asset() context
            if let Some(asset_ctx) = Self::get_asset_call_context(line_text, position.character) {
                debug!("   Asset context, filter prefix: '{}'", asset_ctx.prefix);

                let root = match self.root_path.read().await.clone() {
                    Some(r) => r,
                    None => return Ok(None),
                };

                let public_dir = root.join("public");
                let files = self.get_directory_files(&public_dir, None).await;

                // Build completion items, filtering by prefix
                let prefix_lower = asset_ctx.prefix.to_lowercase();
                let items: Vec<CompletionItem> = files
                    .into_iter()
                    .filter(|f| f.path.to_lowercase().starts_with(&prefix_lower))
                    .map(|f| CompletionItem {
                        label: f.path.clone(),
                        kind: Some(CompletionItemKind::FILE),
                        detail: Some("public/".to_string() + &f.path),
                        documentation: None,
                        text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                            range: Range {
                                start: Position {
                                    line: position.line,
                                    character: asset_ctx.start_col,
                                },
                                end: Position {
                                    line: position.line,
                                    character: asset_ctx.end_col,
                                },
                            },
                            new_text: f.path.clone(),
                        })),
                        ..Default::default()
                    })
                    .collect();

                debug!("   Returning {} asset completion items", items.len());

                return if items.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(CompletionResponse::List(CompletionList {
                        is_incomplete: false,
                        items,
                    })))
                };
            }

            // Check for @vite() context
            if let Some(vite_ctx) = Self::get_vite_call_context(line_text, position.character) {
                debug!("   Vite context, filter prefix: '{}'", vite_ctx.prefix);

                let root = match self.root_path.read().await.clone() {
                    Some(r) => r,
                    None => return Ok(None),
                };

                // Vite assets are typically in resources/ directory
                let resources_dir = root.join("resources");
                let vite_extensions = &[
                    "js", "ts", "jsx", "tsx", "css", "scss", "sass", "less", "vue", "svelte",
                ];
                let files = self
                    .get_directory_files(&resources_dir, Some(vite_extensions))
                    .await;

                // Prefix paths with "resources/" for proper Vite resolution
                let prefix_lower = vite_ctx.prefix.to_lowercase();
                let items: Vec<CompletionItem> = files
                    .into_iter()
                    .map(|f| format!("resources/{}", f.path))
                    .filter(|p| p.to_lowercase().starts_with(&prefix_lower))
                    .map(|path| CompletionItem {
                        label: path.clone(),
                        kind: Some(CompletionItemKind::FILE),
                        detail: None,
                        documentation: None,
                        text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                            range: Range {
                                start: Position {
                                    line: position.line,
                                    character: vite_ctx.start_col,
                                },
                                end: Position {
                                    line: position.line,
                                    character: vite_ctx.end_col,
                                },
                            },
                            new_text: path.clone(),
                        })),
                        ..Default::default()
                    })
                    .collect();

                debug!("   Returning {} Vite completion items", items.len());

                return if items.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(CompletionResponse::List(CompletionList {
                        is_incomplete: false,
                        items,
                    })))
                };
            }

            // Check for path helper context (app_path, base_path, storage_path, etc.)
            if let Some((helper, path_prefix)) =
                Self::get_path_helper_context(line_text, position.character)
            {
                debug!(
                    "   Path helper context: {}, filter prefix: '{}'",
                    helper, path_prefix
                );

                let root = match self.root_path.read().await.clone() {
                    Some(r) => r,
                    None => return Ok(None),
                };

                let base_dir = self.get_path_helper_base_dir(helper, &root);
                let files = self.get_directory_files(&base_dir, None).await;

                // Build completion items, filtering by prefix
                let prefix_lower = path_prefix.to_lowercase();
                let items: Vec<CompletionItem> = files
                    .into_iter()
                    .filter(|f| f.path.to_lowercase().starts_with(&prefix_lower))
                    .map(|f| CompletionItem {
                        label: f.path.clone(),
                        kind: Some(CompletionItemKind::FILE),
                        detail: None,
                        documentation: None,
                        ..Default::default()
                    })
                    .collect();

                debug!("   Returning {} path helper completion items", items.len());

                return if items.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(CompletionResponse::List(CompletionList {
                        is_incomplete: false,
                        items,
                    })))
                };
            }

            // Check for container binding context (app('...'), resolve('...'))
            if let Some(binding_ctx) = Self::get_binding_call_context(line_text, position.character)
            {
                debug!(
                    "   Binding context, filter prefix: '{}'",
                    binding_ctx.prefix
                );

                // Get all bindings from Salsa
                let bindings = match self.salsa.get_all_parsed_bindings().await {
                    Ok(b) => b,
                    Err(e) => {
                        debug!("   Failed to get bindings: {}", e);
                        Vec::new()
                    }
                };

                // Build completion items, filtering by prefix (case-insensitive)
                let prefix_lower = binding_ctx.prefix.to_lowercase();
                let items: Vec<CompletionItem> = bindings
                    .into_iter()
                    .filter(|b| b.abstract_name.to_lowercase().starts_with(&prefix_lower))
                    .map(|b| {
                        let source = b
                            .source_file
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("unknown");

                        CompletionItem {
                            label: b.abstract_name.clone(),
                            kind: Some(CompletionItemKind::CLASS),
                            detail: Some(format!("{} ({})", b.concrete_class, source)),
                            documentation: None,
                            text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                                range: Range {
                                    start: Position {
                                        line: position.line,
                                        character: binding_ctx.start_col,
                                    },
                                    end: Position {
                                        line: position.line,
                                        character: binding_ctx.end_col,
                                    },
                                },
                                new_text: b.abstract_name.clone(),
                            })),
                            ..Default::default()
                        }
                    })
                    .collect();

                debug!("   Returning {} binding completion items", items.len());

                return if items.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(CompletionResponse::List(CompletionList {
                        is_incomplete: false,
                        items,
                    })))
                };
            }

            // Check for route context
            if let Some(route_ctx) = Self::get_route_call_context(line_text, position.character) {
                debug!("   Route context, filter prefix: '{}'", route_ctx.prefix);

                // Get all route names
                let route_names = self.get_all_route_names().await;

                // Build completion items, filtering by prefix (case-sensitive)
                let items: Vec<CompletionItem> = route_names
                    .into_iter()
                    .filter(|r| r.name.starts_with(&route_ctx.prefix))
                    .map(|r| CompletionItem {
                        label: r.name.clone(),
                        kind: Some(CompletionItemKind::CONSTANT),
                        detail: Some(format!("({})", r.source)),
                        documentation: None,
                        text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                            range: Range {
                                start: Position {
                                    line: position.line,
                                    character: route_ctx.start_col,
                                },
                                end: Position {
                                    line: position.line,
                                    character: route_ctx.end_col,
                                },
                            },
                            new_text: r.name.clone(),
                        })),
                        ..Default::default()
                    })
                    .collect();

                debug!("   Returning {} route completion items", items.len());

                return if items.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(CompletionResponse::List(CompletionList {
                        is_incomplete: false,
                        items,
                    })))
                };
            }

            // Check for middleware context
            // Pass previous lines for multi-line array detection
            let previous_lines: Vec<&str> = lines[..position.line as usize].to_vec();
            let prev_lines_slice: Option<&[&str]> = if previous_lines.is_empty() {
                None
            } else {
                Some(&previous_lines)
            };
            if let Some(middleware_ctx) =
                Self::get_middleware_call_context(line_text, position.character, prev_lines_slice)
            {
                debug!(
                    "   Middleware context, filter prefix: '{}'",
                    middleware_ctx.prefix
                );

                // Get all middleware from Salsa
                let middleware_list = match self.salsa.get_all_parsed_middleware().await {
                    Ok(mw) => mw,
                    Err(e) => {
                        debug!("   Failed to get middleware: {}", e);
                        Vec::new()
                    }
                };

                // Build completion items, filtering by prefix (case-insensitive)
                let prefix_lower = middleware_ctx.prefix.to_lowercase();
                let items: Vec<CompletionItem> = middleware_list
                    .into_iter()
                    .filter(|m| m.alias.to_lowercase().starts_with(&prefix_lower))
                    .map(|m| {
                        let source = m
                            .source_file
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("unknown");

                        CompletionItem {
                            label: m.alias.clone(),
                            kind: Some(CompletionItemKind::MODULE),
                            detail: Some(format!("{} ({})", m.class_name, source)),
                            documentation: None,
                            text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                                range: Range {
                                    start: Position {
                                        line: position.line,
                                        character: middleware_ctx.start_col,
                                    },
                                    end: Position {
                                        line: position.line,
                                        character: middleware_ctx.end_col,
                                    },
                                },
                                new_text: m.alias.clone(),
                            })),
                            ..Default::default()
                        }
                    })
                    .collect();

                debug!("   Returning {} middleware completion items", items.len());

                return if items.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(CompletionResponse::List(CompletionList {
                        is_incomplete: false,
                        items,
                    })))
                };
            }

            // Check for feature context (Laravel Pennant)
            info!("   🔍 Checking feature context for line: '{}'", line_text);
            if let Some(feature_ctx) = Self::get_feature_call_context(line_text, position.character)
            {
                info!(
                    "   ✅ Feature context detected, filter prefix: '{}'",
                    feature_ctx.prefix
                );

                // Get project root
                let features = {
                    let root_guard = self.root_path.read().await;
                    match root_guard.as_ref() {
                        Some(root) => {
                            info!("   📁 Scanning for features in: {:?}", root);
                            let found = scan_feature_classes(root);
                            info!("   📋 Found {} feature classes", found.len());
                            found
                        }
                        None => {
                            debug!("warn: No root path available");
                            Vec::new()
                        }
                    }
                };

                // Build completion items, filtering by prefix (case-insensitive)
                let prefix_lower = feature_ctx.prefix.to_lowercase();
                let items: Vec<CompletionItem> = features
                    .into_iter()
                    .filter(|f| f.feature_key.to_lowercase().starts_with(&prefix_lower))
                    .map(|f| CompletionItem {
                        label: f.feature_key.clone(),
                        kind: Some(CompletionItemKind::CLASS),
                        detail: Some(format!("Feature: {}", f.class_name)),
                        documentation: Some(Documentation::String(f.full_class.clone())),
                        text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                            range: Range {
                                start: Position {
                                    line: position.line,
                                    character: feature_ctx.start_col,
                                },
                                end: Position {
                                    line: position.line,
                                    character: feature_ctx.end_col,
                                },
                            },
                            new_text: f.feature_key.clone(),
                        })),
                        ..Default::default()
                    })
                    .collect();

                debug!("   Returning {} feature completion items", items.len());

                return if items.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(CompletionResponse::List(CompletionList {
                        is_incomplete: false,
                        items,
                    })))
                };
            }

            // Check for translation context
            if let Some(trans_ctx) =
                Self::get_translation_call_context(line_text, position.character)
            {
                debug!(
                    "   Translation context, filter prefix: '{}'",
                    trans_ctx.prefix
                );

                // Get all translation keys
                let translation_keys = self.get_all_translation_keys().await;

                // Build completion items, filtering by prefix (case-sensitive)
                let items: Vec<CompletionItem> = translation_keys
                    .into_iter()
                    .filter(|t| t.key.starts_with(&trans_ctx.prefix))
                    .map(|t| {
                        let detail = if t.value.is_empty() {
                            format!("({})", t.source)
                        } else {
                            format!("{} ({})", t.value, t.source)
                        };

                        CompletionItem {
                            label: t.key.clone(),
                            kind: Some(CompletionItemKind::TEXT),
                            detail: Some(detail),
                            documentation: None,
                            text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                                range: Range {
                                    start: Position {
                                        line: position.line,
                                        character: trans_ctx.start_col,
                                    },
                                    end: Position {
                                        line: position.line,
                                        character: trans_ctx.end_col,
                                    },
                                },
                                new_text: t.key.clone(),
                            })),
                            ..Default::default()
                        }
                    })
                    .collect();

                debug!("   Returning {} translation completion items", items.len());

                return if items.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(CompletionResponse::List(CompletionList {
                        is_incomplete: false,
                        items,
                    })))
                };
            }

            // Build surrounding lines context once for both param and rule context checks
            // Pass ALL preceding lines so smart bracket tracking can find the array declaration
            let surrounding_lines: Vec<&str> = {
                let line_idx = position.line as usize;
                let mut context = Vec::new();
                // Collect all lines before current (most recent first for legacy compatibility)
                for i in 1..=line_idx {
                    if let Some(prev_line) = lines.get(line_idx - i) {
                        context.push(*prev_line);
                    }
                }
                context
            };

            // Get cached validation rule names for context detection
            let cached_rules = self.cached_validation_rule_names.read().await.clone();
            info!(
                "   📋 Cached validation rules count: {}",
                cached_rules.len()
            );
            if cached_rules.is_empty() {
                debug!("warn:  No cached rules - context detection will use fallback only");
            }

            // Check for validation rule PARAMETER context first (e.g., "exists:█" or "after:█")
            info!(
                "   🔍 Checking validation param context for line: '{}'",
                line_text
            );
            if let Some(param_context) = Self::get_validation_param_context(
                line_text,
                position.character,
                &surrounding_lines,
                &cached_rules,
            ) {
                info!(
                    "   ✅ Validation param context detected: rule='{}', param='{}', index={}",
                    param_context.rule_name, param_context.current_param, param_context.param_index
                );

                let items = self
                    .get_validation_param_completions(
                        &param_context,
                        &content,
                        position.line as usize,
                        uri,
                        position,
                    )
                    .await;

                info!("   📋 Got {} param completion items", items.len());

                if !items.is_empty() {
                    info!(
                        "   ✅ Returning {} validation param completion items",
                        items.len()
                    );
                    return Ok(Some(CompletionResponse::List(CompletionList {
                        is_incomplete: false,
                        items,
                    })));
                } else {
                    info!(
                        "   ⚠️  No param completions available for rule '{}'",
                        param_context.rule_name
                    );
                }
            } else {
                info!("   ℹ️  Not in validation param context");
            }

            // Check for validation rule context (rule name completion)
            if let Some(rule_prefix) = Self::get_validation_rule_context(
                line_text,
                position.character,
                &surrounding_lines,
                &cached_rules,
            ) {
                info!(
                    "   🔵 VALIDATION RULE context detected, filter prefix: '{}', line: '{}'",
                    rule_prefix,
                    line_text.chars().take(60).collect::<String>()
                );

                // Get all validation rules (built-in + custom)
                let validation_rules = self.get_all_validation_rules().await;

                // Build completion items, filtering by prefix (case-insensitive for rules)
                let prefix_lower = rule_prefix.to_lowercase();
                let items: Vec<CompletionItem> = validation_rules
                    .into_iter()
                    .filter(|r| r.name.to_lowercase().starts_with(&prefix_lower))
                    .map(|r| {
                        let label = if r.has_params {
                            format!("{}:", r.name)
                        } else {
                            r.name.clone()
                        };

                        CompletionItem {
                            label,
                            kind: Some(CompletionItemKind::KEYWORD),
                            detail: Some(format!("({})", r.source)),
                            documentation: Some(Documentation::String(r.description)),
                            insert_text: Some(r.name.clone()),
                            ..Default::default()
                        }
                    })
                    .collect();

                debug!(
                    "   Returning {} validation rule completion items",
                    items.len()
                );

                return if items.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(CompletionResponse::List(CompletionList {
                        is_incomplete: false,
                        items,
                    })))
                };
            }

            // Check for cast type context ($casts array or casts() method)
            if let Some(cast_prefix) =
                Self::get_cast_type_context(line_text, position.character, &surrounding_lines)
            {
                info!(
                    "   🟢 CAST TYPE context detected, filter prefix: '{}', line: '{}'",
                    cast_prefix,
                    line_text.chars().take(60).collect::<String>()
                );

                // Get all cast types (built-in primitives + scanned from vendor/app)
                let mut cast_types = get_laravel_cast_types();

                // Add casts from Laravel framework, packages, and app/Casts
                if let Some(root) = self.root_path.read().await.clone() {
                    let scanned_casts = scan_all_casts(&root);
                    debug!("   Found {} casts from vendor/app", scanned_casts.len());
                    cast_types.extend(scanned_casts);
                }

                // Build completion items, filtering by prefix (case-insensitive)
                let prefix_lower = cast_prefix.to_lowercase();
                let items: Vec<CompletionItem> = cast_types
                    .into_iter()
                    .filter(|c| c.name.to_lowercase().starts_with(&prefix_lower))
                    .map(|c| {
                        let label = if c.has_params {
                            format!("{}:", c.name)
                        } else {
                            c.name.clone()
                        };

                        CompletionItem {
                            label,
                            kind: Some(CompletionItemKind::KEYWORD),
                            detail: Some(format!("({})", c.source)),
                            documentation: Some(Documentation::String(c.description)),
                            insert_text: Some(c.name.clone()),
                            ..Default::default()
                        }
                    })
                    .collect();

                debug!("   Returning {} cast type completion items", items.len());

                return if items.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(CompletionResponse::List(CompletionList {
                        is_incomplete: false,
                        items,
                    })))
                };
            }
        }

        // Check for env context
        let env_ctx = if is_env_file {
            // In .env files, check for ${...} interpolation
            Self::get_env_interpolation_context(line_text, position.character)
        } else if is_phpunit_file {
            // In PHPUnit XML files, check for <env name="..."> or <server name="...">
            Self::get_phpunit_env_context(line_text, position.character)
        } else {
            // In PHP/Blade files, check for env('...') calls
            Self::get_env_call_context(line_text, position.character)
        };

        // If not in a valid context, return no completions
        let env_ctx = match env_ctx {
            Some(ctx) => ctx,
            None => {
                debug!("   Not in completion context");
                return Ok(None);
            }
        };

        debug!("   Env filter prefix: '{}'", env_ctx.prefix);

        // Get all env vars from Salsa (.env files)
        let env_vars = match self.salsa.get_all_parsed_env_vars().await {
            Ok(vars) => vars,
            Err(e) => {
                debug!("   Failed to get env vars: {}", e);
                Vec::new()
            }
        };

        // Build completion items, filtering by prefix
        let filter_upper = env_ctx.prefix.to_uppercase();

        // Track which var names we've seen (from .env files)
        let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();

        // Create the text_edit range once for reuse
        let edit_range = Range {
            start: Position {
                line: position.line,
                character: env_ctx.start_col,
            },
            end: Position {
                line: position.line,
                character: env_ctx.end_col,
            },
        };

        // First, add .env file vars (project-specific, higher priority)
        let mut items: Vec<CompletionItem> = env_vars
            .into_iter()
            .filter(|v| !v.is_commented)
            .filter(|v| v.name.to_uppercase().starts_with(&filter_upper))
            .map(|v| {
                seen_names.insert(v.name.clone());
                let source_file = v
                    .source_file
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(".env");

                CompletionItem {
                    label: v.name.clone(),
                    kind: Some(CompletionItemKind::VARIABLE),
                    detail: Some(format!("{} (from {})", v.value, source_file)),
                    documentation: None,
                    text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                        range: edit_range,
                        new_text: v.name.clone(),
                    })),
                    ..Default::default()
                }
            })
            .collect();

        // Then, add system env vars (lower priority, only if not already in .env)
        for (name, value) in std::env::vars() {
            if !seen_names.contains(&name) && name.to_uppercase().starts_with(&filter_upper) {
                items.push(CompletionItem {
                    label: name.clone(),
                    kind: Some(CompletionItemKind::VARIABLE),
                    detail: Some(format!("{} (from system)", value)),
                    documentation: None,
                    text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                        range: edit_range,
                        new_text: name,
                    })),
                    ..Default::default()
                });
            }
        }

        debug!("   Returning {} env completion items", items.len());

        if items.is_empty() {
            Ok(None)
        } else {
            // Use CompletionList to have more control over behavior
            Ok(Some(CompletionResponse::List(CompletionList {
                is_incomplete: false,
                items,
            })))
        }
    }

    /// Provides semantic tokens for Blade directive highlighting
    ///
    /// This overrides tree-sitter's incremental parsing which can leave stale highlights
    /// when editing directives (e.g., changing @feature to @featured). The LSP semantic
    /// tokens are re-requested on every change, providing instant highlight updates.
    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> jsonrpc::Result<Option<SemanticTokensResult>> {
        let uri = &params.text_document.uri;
        let path = uri.path();

        // Only process Blade files
        if !path.ends_with(".blade.php") {
            return Ok(None);
        }

        info!("🎨 semantic_tokens_full called for {}", uri);

        // Get document content from cache
        let content = {
            let docs = self.documents.read().await;
            match docs.get(uri) {
                Some((text, _)) => text.clone(),
                None => {
                    debug!("   Document not found in cache");
                    return Ok(None);
                }
            }
        };

        // Extract directive tokens
        let tokens = self.extract_blade_directive_tokens(&content);

        info!("   Found {} directive tokens", tokens.len());

        Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data: tokens,
        })))
    }
}

// ❌ REMOVED: code_lens helper methods (extract_view_name_from_path, find_all_references_to_view)
// Zed doesn't support custom LSP commands, so code lens was not functional.

#[cfg(test)]
mod tests;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging with environment-based filtering.
    //
    // Default to INFO for our own crate, but WARN for `salsa` — its
    // `salsa::function::execute …: executing query` traces fire once per
    // tracked-function call at INFO. On a 40k-file project that's 40k+ log
    // lines from a foreign crate during cache warming, which dwarfed every
    // other cost in the warming loop (turned 8s of real work into 71s of
    // log formatting). The downstream `RUST_LOG` env var still wins when
    // set, so debug sessions can opt back in via `RUST_LOG=salsa=debug`.
    use tracing_subscriber::EnvFilter;

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,salsa=warn"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    info!("========================================");
    info!("🚀 Laravel Language Server STARTING 🚀");
    info!("========================================");

    // Create the LSP service
    let (service, socket) = LspService::new(LaravelLanguageServer::new);

    // Read from stdin and write to stdout (standard LSP communication)
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    // Run the server
    Server::new(stdin, stdout, socket).serve(service).await;

    info!("Laravel Language Server stopped");
    Ok(())
}
