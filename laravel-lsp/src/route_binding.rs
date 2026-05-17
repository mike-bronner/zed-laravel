//! Parse `Route::livewire('/path/{param}', 'component-name')` calls and infer
//! Eloquent model types for route-bound parameters that match an untyped public
//! property on the bound component.
//!
//! Laravel's route-model-binding documentation says a typed `public Post $post;`
//! property is auto-populated from the route's `{post}` parameter. When the
//! property is declared *untyped* (`public $post;`), the IDE has no source of
//! truth for the type — but the route's URI shape tells us the param name, and
//! by Laravel convention the class can be inferred from the name.
//!
//! This module powers the third tier of LSP type resolution for Livewire 4
//! component properties:
//!   1. Explicit property type (`public Post $post`) — handled in `php_class`
//!   2. Matching `mount()` parameter type — handled in `php_class`
//!   3. Matching route binding (this module)
//!
//! Resolution is best-effort: if no `Route::livewire(...)` call binds the
//! current component, or the route has no matching param, this module returns
//! `None` and falls back to the property's bare declaration (typically `mixed`).

use std::path::{Path, PathBuf};

use walkdir::WalkDir;

/// A single `Route::livewire(...)` registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteBinding {
    /// Component name string passed as the second argument (e.g. `"pages::contact-us"`,
    /// `"posts.create"`, or a class name).
    pub component_name: String,
    /// Parameter names extracted from `{param}` segments in the URI path, in
    /// declaration order. `Route::livewire('/posts/{post}/comments/{comment}', ...)`
    /// yields `["post", "comment"]`.
    pub params: Vec<String>,
}

/// Parse every `Route::livewire('/uri/{param}', 'name')` call out of a single
/// PHP file's contents. Returns one `RouteBinding` per match in source order.
pub fn parse_livewire_route_bindings(content: &str) -> Vec<RouteBinding> {
    let re = match regex::Regex::new(
        r#"Route::livewire\s*\(\s*['"]([^'"]+)['"]\s*,\s*['"]([^'"]+)['"]"#,
    ) {
        Ok(re) => re,
        Err(_) => return Vec::new(),
    };
    let param_re = match regex::Regex::new(r"\{([a-zA-Z_][a-zA-Z0-9_]*)\??\}") {
        Ok(re) => re,
        Err(_) => return Vec::new(),
    };

    re.captures_iter(content)
        .filter_map(|caps| {
            let uri = caps.get(1)?.as_str();
            let component_name = caps.get(2)?.as_str().to_string();
            let params = param_re
                .captures_iter(uri)
                .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
                .collect();
            Some(RouteBinding {
                component_name,
                params,
            })
        })
        .collect()
}

/// Walk `routes/**/*.php` for route-registration files. Returns absolute paths.
/// Skips vendor and node_modules.
pub fn discover_route_files(root: &Path) -> Vec<PathBuf> {
    let routes_dir = root.join("routes");
    if !routes_dir.is_dir() {
        return Vec::new();
    }

    WalkDir::new(&routes_dir)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !matches!(name.as_ref(), "vendor" | "node_modules")
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("php"))
        .collect()
}

/// Derive every Livewire component name a given Blade view path could resolve
/// to. Covers Livewire 4's three discovery roots:
///   - `resources/views/components/` → default namespace (`foo.bar`)
///   - `resources/views/pages/`      → `pages::` namespace
///   - `resources/views/layouts/`    → `layouts::` namespace
///   - `resources/views/livewire/`   → class-based carry-over (`foo.bar`)
///
/// The `⚡` (U+26A1) marker prefix on file or directory names is stripped per
/// Livewire 4 convention. Multi-file components are detected when the blade
/// file's stem matches its parent directory name (minus the marker).
pub fn livewire_component_names_for_blade(blade_path: &Path, root: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let stem = match blade_path.file_name().and_then(|n| n.to_str()) {
        Some(name) => match name.strip_suffix(".blade.php") {
            Some(s) => strip_marker(s),
            None => return names,
        },
        None => return names,
    };

    for (root_subdir, ns_prefix) in [
        ("components", ""),
        ("pages", "pages::"),
        ("layouts", "layouts::"),
        ("livewire", ""),
    ] {
        let discovery_root = root.join("resources").join("views").join(root_subdir);
        let Ok(relative) = blade_path.strip_prefix(&discovery_root) else {
            continue;
        };

        let mut segments: Vec<String> = relative
            .parent()
            .map(|p| {
                p.iter()
                    .filter_map(|s| s.to_str().map(strip_marker_owned))
                    .collect()
            })
            .unwrap_or_default();

        // Multi-file component: parent directory name (minus marker) matches stem.
        // Drop the redundant directory segment so the component name is `pages::foo`
        // rather than `pages::foo.foo`.
        if let Some(last) = segments.last() {
            if last == &stem {
                segments.pop();
            }
        }

        segments.push(stem.clone());
        names.push(format!("{}{}", ns_prefix, segments.join(".")));
    }

    names
}

/// Strip Livewire 4's `⚡` marker (U+26A1) from a single file/directory name.
fn strip_marker(name: &str) -> String {
    name.strip_prefix('\u{26A1}').unwrap_or(name).to_string()
}

fn strip_marker_owned(name: &str) -> String {
    strip_marker(name)
}

/// Infer an Eloquent model class name from a route parameter name via Laravel's
/// convention: PascalCase the param. `post` → `Post`, `user_settings` →
/// `UserSettings`. Does *not* singularize — that's a heuristic with bad failure
/// modes (`media` → `Medium`, etc.). Returns `None` for empty input.
pub fn infer_model_class_from_param(param_name: &str) -> Option<String> {
    if param_name.is_empty() {
        return None;
    }
    let mut result = String::with_capacity(param_name.len());
    let mut upper_next = true;
    for ch in param_name.chars() {
        if ch == '_' || ch == '-' {
            upper_next = true;
        } else if upper_next {
            result.extend(ch.to_uppercase());
            upper_next = false;
        } else {
            result.push(ch);
        }
    }
    Some(result)
}

/// Find a route-binding-derived type for `property_name` against this blade
/// view. Walks routes files, matches the component name, looks for a matching
/// param. Returns the inferred model class name, or `None` when nothing
/// applies.
pub fn find_route_binding_type(
    blade_path: &Path,
    root: &Path,
    property_name: &str,
) -> Option<String> {
    let candidate_names = livewire_component_names_for_blade(blade_path, root);
    if candidate_names.is_empty() {
        return None;
    }

    for route_file in discover_route_files(root) {
        let Ok(content) = std::fs::read_to_string(&route_file) else {
            continue;
        };
        for binding in parse_livewire_route_bindings(&content) {
            if !candidate_names.contains(&binding.component_name) {
                continue;
            }
            if binding.params.iter().any(|p| p == property_name) {
                return infer_model_class_from_param(property_name);
            }
        }
    }

    None
}
