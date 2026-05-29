//! Build the static-position method surface a Laravel project exposes
//! at `Model::|` via `__callStatic`.
//!
//! Thin assembler over the unified [`crate::laravel_introspector`] walker.
//! Three views are pulled (Eloquent Builder, Query Builder, Model) and
//! merged into a single [`BuilderMethodIndex`] that powers the
//! method-name completion handler.
//!
//! All the heavy lifting — class structure walking, trait composition
//! recursion, `__callStatic` surface computation, `$this`/`self`/`static`
//! resolution, method attribution to the entry class — lives in
//! `laravel_class`. This file is glue.

use std::collections::HashSet;
use std::path::Path;

use crate::laravel_introspector::chain::analyze;
use crate::laravel_introspector::walker::PhpVisibility;

/// Relative path from project root to Laravel's Eloquent Builder source.
pub const ELOQUENT_BUILDER_REL_PATH: &str =
    "vendor/laravel/framework/src/Illuminate/Database/Eloquent/Builder.php";

/// Relative path from project root to Laravel's base Query Builder source.
pub const QUERY_BUILDER_REL_PATH: &str =
    "vendor/laravel/framework/src/Illuminate/Database/Query/Builder.php";

/// Relative path to Laravel's Eloquent Model — parsed for its real
/// public static methods so we can avoid shadowing them with our
/// Builder method emissions.
pub const ELOQUENT_MODEL_REL_PATH: &str =
    "vendor/laravel/framework/src/Illuminate/Database/Eloquent/Model.php";

/// A single method we'd surface at `Model::|`. Re-exported from
/// [`crate::laravel_introspector::chain::BuilderMethod`] — they're the same
/// shape, and the alias keeps existing call sites working unchanged.
pub type ParsedMethod = crate::laravel_introspector::chain::BuilderMethod;

/// Method surface available at `Model::|` via `__callStatic`. Lazily
/// parsed from the user's `vendor/`, cached per project root on the
/// `Backend`.
#[derive(Debug, Clone)]
pub struct BuilderMethodIndex {
    /// Methods directly on `Illuminate\Database\Eloquent\Builder` plus
    /// methods from every trait it composes. First-declared wins.
    pub eloquent_builder: Vec<ParsedMethod>,
    /// Methods on `Illuminate\Database\Query\Builder` plus its composed
    /// traits.
    pub query_builder: Vec<ParsedMethod>,
    /// Names of real public static methods on `Model` (and its composed
    /// traits). Used to suppress Builder methods of the same name at
    /// the static call position — PHP routes `Portfolio::with(...)`
    /// directly to Model's signature, so Builder's would mislead.
    pub model_static_method_names: HashSet<String>,
}

impl BuilderMethodIndex {
    /// Combine Eloquent and Query methods into the full static-position
    /// surface. Eloquent wins on collision (more specific override).
    /// Methods whose name appears as a real public static on Model are
    /// suppressed. Result is sorted alphabetically.
    pub fn merged_surface(&self) -> Vec<&ParsedMethod> {
        let mut out: Vec<&ParsedMethod> = self
            .eloquent_builder
            .iter()
            .filter(|m| !self.model_static_method_names.contains(&m.name))
            .collect();
        for q in &self.query_builder {
            if self.model_static_method_names.contains(&q.name) {
                continue;
            }
            if !out.iter().any(|m| m.name == q.name) {
                out.push(q);
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

/// Build the `BuilderMethodIndex` for `project_root`. Returns `None`
/// when neither the Eloquent Builder nor the Query Builder file
/// exists in vendor — the user probably hasn't run `composer install`.
///
/// Half-installs (one Builder file missing) degrade gracefully: the
/// missing file's surface is empty, the rest works.
pub fn parse_builder_methods(project_root: &Path) -> Option<BuilderMethodIndex> {
    let eloquent_path = project_root.join(ELOQUENT_BUILDER_REL_PATH);
    let query_path = project_root.join(QUERY_BUILDER_REL_PATH);
    let model_path = project_root.join(ELOQUENT_MODEL_REL_PATH);

    if !eloquent_path.exists() && !query_path.exists() {
        return None;
    }

    let eloquent_builder = analyze(&eloquent_path, project_root)
        .map(|view| view.callstatic_surface)
        .unwrap_or_default();

    let query_builder = analyze(&query_path, project_root)
        .map(|view| view.callstatic_surface)
        .unwrap_or_default();

    let model_static_method_names = if model_path.exists() {
        analyze(&model_path, project_root)
            .map(model_static_names)
            .unwrap_or_default()
    } else {
        HashSet::new()
    };

    Some(BuilderMethodIndex {
        eloquent_builder,
        query_builder,
        model_static_method_names,
    })
}

/// Collect the names of every real public static method visible on
/// Model (direct + composed traits). PHP routes `Portfolio::name(...)`
/// to a real Model static of the same name directly — no `__callStatic`
/// — so Builder methods of the same name would mislead with the wrong
/// signature.
fn model_static_names(view: crate::laravel_introspector::chain::ClassView) -> HashSet<String> {
    view.all_methods
        .into_iter()
        .filter(|m| {
            m.value.visibility == PhpVisibility::Public
                && m.value.is_static
                && !m.value.name.starts_with("__")
        })
        .map(|m| m.value.name)
        .collect()
}

// FQCN constants used to live here; canonical source is now in
// `crate::laravel_introspector` (`ELOQUENT_BUILDER_FQCN`, `QUERY_BUILDER_FQCN`).

#[cfg(test)]
mod tests;
