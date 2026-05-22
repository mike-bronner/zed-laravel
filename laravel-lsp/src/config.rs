//! Laravel project configuration utilities
//!
//! This module provides utilities for discovering Laravel projects
//! and working with Laravel naming conventions.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::info;

/// Find the Laravel project root by walking up from a file path
///
/// Looks for Laravel-specific markers:
/// - composer.json + artisan (Laravel app)
/// - composer.json + app/ + resources/ (Laravel app)
/// - composer.json + src/ + vendor/ (Laravel package)
///
/// Returns None if no Laravel project root is found.
pub fn find_project_root(file_path: &Path) -> Option<PathBuf> {
    let mut current = file_path;

    // If it's a file, start from its parent directory
    if current.is_file() {
        current = current.parent()?;
    }

    // Walk up the directory tree
    loop {
        // Check for Laravel markers
        let has_composer = current.join("composer.json").exists();
        let has_artisan = current.join("artisan").exists();
        let has_app = current.join("app").is_dir();
        let has_resources = current.join("resources").is_dir();
        let has_src = current.join("src").is_dir();
        let has_vendor = current.join("vendor").is_dir();

        // If we find composer.json + artisan, it's very likely a Laravel app
        if has_composer && has_artisan {
            info!("Found Laravel project root at {:?} (composer.json + artisan)", current);
            return Some(current.to_path_buf());
        }

        // Or if we find composer.json + app/ + resources/ (Laravel app)
        if has_composer && has_app && has_resources {
            info!("Found Laravel project root at {:?} (composer.json + app + resources)", current);
            return Some(current.to_path_buf());
        }

        // Or if we find composer.json + src/ + vendor/ (Laravel package)
        // This pattern recognizes Laravel package development
        if has_composer && has_src && has_vendor {
            info!("Found Laravel package root at {:?} (composer.json + src + vendor)", current);
            return Some(current.to_path_buf());
        }

        // Move up one directory
        current = current.parent()?;
    }
}

/// Load Blade component aliases from all known sources.
///
/// Three independent sources are merged into a single `HashMap<alias, view-dot-path>`,
/// in **priority order** (later sources override earlier ones):
///
/// 0. **Vendor packages** (weakest) — service-provider files under `vendor/`
///    that look like `*ServiceProvider*.php` and contain `Blade::component()` /
///    `$blade->component()` calls. Results are cached on disk and invalidated
///    when `composer.lock` changes.
/// 1. **Config-driven** — `config/component.php`'s `'aliases'` array. Common
///    convention for projects that register many aliases through a single
///    config-loop in their `AppServiceProvider`.
/// 2. **App service providers** (strongest) — `$blade->component($view, $alias)` and
///    `Blade::component($view, $alias)` invocations inside `app/Providers/*.php`.
///    Closest to runtime truth, wins on conflict.
///
/// All sources gracefully no-op when their respective files/dirs are absent.
pub fn load_component_aliases(root: &Path) -> HashMap<String, String> {
    let mut aliases = HashMap::new();

    // Source 0: Vendor packages (weakest priority).
    aliases.extend(scan_vendor_for_component_aliases(root));

    // Source 1: config/component.php (overrides vendor defaults).
    let config_path = root.join("config/component.php");
    if let Ok(source) = fs::read_to_string(&config_path) {
        parse_component_aliases(&source, &mut aliases);
    }

    // Source 2: app/Providers/*.php — direct $blade->component() / Blade::component() calls.
    let providers_dir = root.join("app/Providers");
    if providers_dir.is_dir() {
        if let Ok(entries) = fs::read_dir(&providers_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("php") {
                    continue;
                }
                let Ok(source) = fs::read_to_string(&path) else { continue };
                extract_provider_blade_aliases(&source, &mut aliases);
            }
        }
    }

    aliases
}

// ============================================================================
// Vendor scanning + on-disk cache
// ============================================================================

const VENDOR_ALIAS_CACHE_FILENAME: &str = "vendor_component_aliases.json";

/// Current cache schema version. Bump whenever the cache shape changes so
/// older cache files force a re-scan instead of silently returning stale data
/// for fields that didn't exist when the cache was written.
///
/// History:
///   v0 (implicit) — only `composer_lock_mtime_secs` + `aliases`.
///   v1 — added `icon_aliases` for blade-icons SVG resolution.
const VENDOR_CACHE_SCHEMA_VERSION: u32 = 1;

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct VendorAliasCache {
    #[serde(default)]
    schema_version: u32,
    composer_lock_mtime_secs: u64,
    aliases: HashMap<String, String>,
    #[serde(default)]
    icon_aliases: HashMap<String, String>,
}

/// Walk `vendor/` for service providers that register Blade components, and
/// return the merged alias map. Results are cached to disk and only rebuilt
/// when `composer.lock` mtime changes — so the cost is paid once per
/// `composer install` / `composer update`, not on every LSP boot.
pub fn scan_vendor_for_component_aliases(root: &Path) -> HashMap<String, String> {
    let lock_mtime = composer_lock_mtime(root);

    // Cache hit: composer.lock hasn't changed AND the schema matches.
    if let Some(cached) = read_vendor_cache(root) {
        if lock_mtime > 0
            && cached.composer_lock_mtime_secs == lock_mtime
            && cached.schema_version == VENDOR_CACHE_SCHEMA_VERSION
        {
            return cached.aliases;
        }
    }

    let aliases = scan_vendor_uncached(root);
    let icon_aliases = scan_vendor_icons_uncached(root);

    if lock_mtime > 0 {
        write_vendor_cache(
            root,
            &VendorAliasCache {
                schema_version: VENDOR_CACHE_SCHEMA_VERSION,
                composer_lock_mtime_secs: lock_mtime,
                aliases: aliases.clone(),
                icon_aliases,
            },
        );
    }

    aliases
}

/// Scan vendor packages for **icon-set component registrations** (blade-icons
/// Factory pattern). Returns a map of full tag name (e.g., `"heroicon-o-clock"`)
/// to the absolute SVG file path.
///
/// blade-icons registers each icon dynamically at runtime via a loop over a
/// filesystem manifest, so static AST analysis can't extract the pairs. We
/// shortcut that by walking the manifest ourselves: any vendor package with the
/// blade-icons-shaped layout (`resources/svg/` directory + `config/blade-*.php`
/// declaring `'prefix' => '...'`) is treated as an icon set. Each SVG file
/// becomes a `<x-{prefix}-{filename-stem}>` registration.
///
/// Results are cached on disk alongside the component-alias map; invalidation
/// triggers on `composer.lock` mtime change.
pub fn scan_vendor_for_icon_sets(root: &Path) -> HashMap<String, String> {
    let lock_mtime = composer_lock_mtime(root);

    if let Some(cached) = read_vendor_cache(root) {
        if lock_mtime > 0
            && cached.composer_lock_mtime_secs == lock_mtime
            && cached.schema_version == VENDOR_CACHE_SCHEMA_VERSION
        {
            return cached.icon_aliases;
        }
    }

    let icon_aliases = scan_vendor_icons_uncached(root);

    // Refresh the unified cache. We re-scan component aliases too to keep
    // the cache coherent, since both maps share invalidation.
    if lock_mtime > 0 {
        let aliases = scan_vendor_uncached(root);
        write_vendor_cache(
            root,
            &VendorAliasCache {
                schema_version: VENDOR_CACHE_SCHEMA_VERSION,
                composer_lock_mtime_secs: lock_mtime,
                aliases,
                icon_aliases: icon_aliases.clone(),
            },
        );
    }

    icon_aliases
}

fn scan_vendor_icons_uncached(root: &Path) -> HashMap<String, String> {
    let vendor = root.join("vendor");
    if !vendor.is_dir() {
        return HashMap::new();
    }

    let mut icons = HashMap::new();

    // Vendor layout: vendor/{vendor-name}/{package-name}/...
    let Ok(vendor_entries) = fs::read_dir(&vendor) else {
        return icons;
    };
    for vendor_entry in vendor_entries.flatten() {
        let vendor_dir = vendor_entry.path();
        if !vendor_dir.is_dir() {
            continue;
        }
        let Ok(pkg_entries) = fs::read_dir(&vendor_dir) else {
            continue;
        };
        for pkg_entry in pkg_entries.flatten() {
            let pkg_dir = pkg_entry.path();
            if !pkg_dir.is_dir() {
                continue;
            }

            let svg_dir = pkg_dir.join("resources/svg");
            let config_dir = pkg_dir.join("config");
            if !svg_dir.is_dir() || !config_dir.is_dir() {
                continue;
            }

            let Some(prefix) = extract_prefix_from_blade_config_dir(&config_dir) else {
                continue;
            };

            walk_svg_dir_into(&svg_dir, &prefix, &mut icons);
        }
    }

    icons
}

/// Look for a `blade-*.php` config file in the directory and extract its
/// `'prefix' => 'NAME'` value. Returns None when no such file exists or no
/// prefix is declared.
fn extract_prefix_from_blade_config_dir(config_dir: &Path) -> Option<String> {
    let entries = fs::read_dir(config_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(filename) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !filename.starts_with("blade-") || !filename.ends_with(".php") {
            continue;
        }
        let Ok(source) = fs::read_to_string(&path) else {
            continue;
        };
        if let Some(prefix) = scan_prefix_string(&source) {
            return Some(prefix);
        }
    }
    None
}

/// Find `'prefix' => 'value'` (or `"prefix" => "value"`) in a PHP source.
fn scan_prefix_string(source: &str) -> Option<String> {
    for key in ["'prefix'", "\"prefix\""] {
        let mut search_from = 0;
        while let Some(rel) = source[search_from..].find(key) {
            let pos = search_from + rel;
            let after = source[pos + key.len()..].trim_start();
            let Some(after_arrow) = after.strip_prefix("=>") else {
                search_from = pos + key.len();
                continue;
            };
            let after_arrow = after_arrow.trim_start();
            let Some(quote) = after_arrow.chars().next() else {
                return None;
            };
            if quote != '\'' && quote != '"' {
                search_from = pos + key.len();
                continue;
            }
            let body = &after_arrow[1..];
            let Some(end) = body.find(quote) else {
                return None;
            };
            return Some(body[..end].to_string());
        }
    }
    None
}

/// Walk an SVG directory and register each file with its `{prefix}-{name}` tag.
/// Nested directories produce dash-separated tag names (e.g., `outline/clock.svg`
/// under prefix `heroicon` becomes `heroicon-outline-clock`).
fn walk_svg_dir_into(svg_dir: &Path, prefix: &str, out: &mut HashMap<String, String>) {
    for entry in walkdir::WalkDir::new(svg_dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("svg") {
            continue;
        }
        let Ok(rel) = path.strip_prefix(svg_dir) else {
            continue;
        };
        let Some(rel_str) = rel.to_str() else {
            continue;
        };
        let Some(stem) = rel_str.strip_suffix(".svg") else {
            continue;
        };
        // Normalize directory separators to dashes so nested + flat layouts
        // both produce dashed tag names.
        let icon_name = stem.replace(std::path::MAIN_SEPARATOR, "-");
        let tag = format!("{}-{}", prefix, icon_name);
        let Some(abs_str) = path.to_str() else {
            continue;
        };
        out.insert(tag, abs_str.to_string());
    }
}

fn scan_vendor_uncached(root: &Path) -> HashMap<String, String> {
    let vendor = root.join("vendor");
    if !vendor.is_dir() {
        return HashMap::new();
    }

    let mut aliases = HashMap::new();

    for entry in walkdir::WalkDir::new(&vendor)
        .max_depth(10)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("php") {
            continue;
        }

        // Filename gate (cheap): only consider files whose name contains
        // "ServiceProvider". This covers ~99% of real Laravel package providers
        // and trims a ~50k-file vendor walk down to a few hundred parse candidates.
        let filename_matches = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.contains("ServiceProvider"))
            .unwrap_or(false);
        if !filename_matches {
            continue;
        }

        let Ok(source) = fs::read_to_string(path) else {
            continue;
        };

        // Content gate (cheap substring): must look like a Blade component
        // registration. Avoids parsing files that happen to be named
        // *ServiceProvider* but register middleware, bindings, etc.
        let has_component_call = source.contains("Blade::component(")
            || source.contains("->component(");
        if !has_component_call {
            continue;
        }

        extract_provider_blade_aliases(&source, &mut aliases);
    }

    aliases
}

fn composer_lock_mtime(root: &Path) -> u64 {
    fs::metadata(root.join("composer.lock"))
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn vendor_cache_path(root: &Path) -> Option<PathBuf> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let proj_dirs = directories::ProjectDirs::from("com", "genealabs", "laravel-lsp")?;
    let cache_base = proj_dirs.cache_dir();

    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut hasher = DefaultHasher::new();
    canonical.hash(&mut hasher);
    let project_hash = format!("{:x}", hasher.finish());

    Some(cache_base.join(project_hash).join(VENDOR_ALIAS_CACHE_FILENAME))
}

fn read_vendor_cache(root: &Path) -> Option<VendorAliasCache> {
    let path = vendor_cache_path(root)?;
    let source = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&source).ok()
}

fn write_vendor_cache(root: &Path, cache: &VendorAliasCache) {
    let Some(path) = vendor_cache_path(root) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(cache) {
        let _ = fs::write(&path, json);
    }
}

/// Extract `$blade->component()` / `Blade::component()` alias registrations
/// from a service-provider PHP file. Inserts pairs into `aliases`. Calls with
/// non-literal arguments (e.g., `$blade->component($component, $alias)` in a
/// loop) produce no captures — those rely on the config-driven source.
fn extract_provider_blade_aliases(source: &str, aliases: &mut HashMap<String, String>) {
    use crate::parser::{language_php, parse_php};
    use crate::queries::extract_all_php_patterns;

    let Ok(tree) = parse_php(source) else {
        return;
    };
    let lang = language_php();
    let Ok(patterns) = extract_all_php_patterns(&tree, source, &lang) else {
        return;
    };

    for m in &patterns.blade_component_aliases {
        // Skip class-FQN-shaped views. PSR-4 class names start with an uppercase
        // letter; view dot-paths are kebab/snake-cased lowercase by convention.
        // Tree-sitter's PHP grammar splits strings at escape sequences, so a
        // literal like `'App\\View\\Components\\Alert'` can surface here with
        // only the leading segment captured — guarding on the first-char case
        // catches both that truncation and unescaped FQNs.
        let first_char_is_uppercase = m.view.chars().next().is_some_and(|c| c.is_uppercase());
        if first_char_is_uppercase || m.view.contains('\\') {
            continue;
        }
        aliases.insert(m.alias.to_string(), m.view.to_string());
    }
}

/// Extract `'alias' => 'view.path'` pairs from a PHP config file's source.
///
/// Scans the file for the `'aliases'` key and walks the inner array literal,
/// pulling out single-quoted alias/view pairs. Skips entries whose value is a
/// `Class::class` reference (those are PHP component classes, not view paths).
fn parse_component_aliases(source: &str, aliases: &mut HashMap<String, String>) {
    // Find the start of the aliases block: 'aliases' => [
    let Some(aliases_pos) = source.find("'aliases'").or_else(|| source.find("\"aliases\"")) else {
        return;
    };

    // Find the opening bracket of the alias array after 'aliases' =>
    let after_key = &source[aliases_pos..];
    let Some(open_bracket_rel) = after_key.find('[') else {
        return;
    };

    // Walk character-by-character to find the matching close bracket so we
    // don't pick up entries from sibling top-level config keys.
    let block_start = aliases_pos + open_bracket_rel + 1;
    let mut depth: i32 = 1;
    let mut block_end = block_start;
    for (idx, ch) in source[block_start..].char_indices() {
        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    block_end = block_start + idx;
                    break;
                }
            }
            _ => {}
        }
    }
    if depth != 0 {
        return;
    }

    let block = &source[block_start..block_end];

    for raw_line in block.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with("//") || line.starts_with('#') || line.starts_with("/*") {
            continue;
        }

        let Some((alias, value)) = split_arrow_pair(line) else {
            continue;
        };

        // Skip ::class references — those point at PHP classes, not view paths.
        if value.contains("::class") {
            continue;
        }

        let Some(view_path) = unquote(value) else {
            continue;
        };
        let Some(alias_name) = unquote(alias) else {
            continue;
        };

        aliases.insert(alias_name.to_string(), view_path.to_string());
    }
}

/// Split a PHP array entry like `'alias' => 'view.path',` into (key, value).
fn split_arrow_pair(line: &str) -> Option<(&str, &str)> {
    let arrow_pos = line.find("=>")?;
    let key = line[..arrow_pos].trim();
    let after_arrow = line[arrow_pos + 2..].trim();
    // Strip trailing comma if present
    let value = after_arrow.trim_end_matches(',').trim();
    Some((key, value))
}

/// Extract the contents of a single- or double-quoted PHP string literal.
fn unquote(input: &str) -> Option<&str> {
    let bytes = input.as_bytes();
    if bytes.len() < 2 {
        return None;
    }
    let quote = bytes[0];
    if quote != b'\'' && quote != b'"' {
        return None;
    }
    if bytes[bytes.len() - 1] != quote {
        return None;
    }
    Some(&input[1..input.len() - 1])
}

/// Convert kebab-case to PascalCase
///
/// Used for converting Livewire component names to class names.
/// Examples:
/// - "user-profile" -> "UserProfile"
/// - "admin-dashboard" -> "AdminDashboard"
pub fn kebab_to_pascal_case(s: &str) -> String {
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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests;
