//! Parse `vendor/laravel/framework/.../Builder.php` and the underlying
//! `Query/Builder.php` to extract the public method surface that's
//! accessible at `Model::|` via `__callStatic`.
//!
//! Why parse instead of hardcoding? Two reasons:
//!
//! 1. **Accuracy across Laravel versions.** A hardcoded list captures one
//!    snapshot of the API. Parsing the user's actual `vendor/` reflects
//!    whatever Laravel version they're running, including new methods,
//!    deprecations, and renames.
//! 2. **No drift.** Laravel adds Builder methods regularly. Hardcoded lists
//!    rot. Parsing rots only when the framework's file layout changes, which
//!    is rare and easy to detect.
//!
//! ## The Eloquent / Query split
//!
//! `Eloquent\Builder` has `@mixin \Illuminate\Database\Query\Builder` —
//! the static analyzer's hint that all `Query\Builder` methods are
//! reachable on an Eloquent Builder via `__call` forwarding. We mirror
//! that by parsing both files and merging the method lists, with Eloquent
//! winning on collision (it's the more specific class).
//!
//! ## Trait recursion
//!
//! Many Builder methods live on traits, not on the class directly —
//! `first`, `sole`, `firstWhere`, `pluck`, `chunk`, etc. come from
//! `Illuminate\Database\Concerns\BuildsQueries`; `whereDate` / `whereYear` /
//! `whereMonth` / `whereTime` / `whereDay` come from `BuildsWhereDateClauses`.
//! A parse that only walks the class body misses dozens of high-traffic
//! methods.
//!
//! So we recursively follow `use TraitName;` declarations inside each
//! class/trait body, resolving trait names through the file's use
//! aliases and parsing each trait's methods. PHP trait composition uses
//! "first wins" precedence — a method defined directly on the class
//! shadows the same name from a trait — which we mirror with a dedup
//! check during accumulation.
//!
//! ## Single class walker
//!
//! All structural walking goes through [`crate::php_outline::extract_php_structure`]
//! — the shared PHP class walker. We never call tree-sitter directly here;
//! we just consume the `PhpFileStructure` it returns. That keeps
//! "what methods/properties does this class have" logic in one place
//! across the LSP.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::parser::parse_php;
use crate::php_outline::{extract_php_structure, PhpVisibility};
use crate::query_chain::{extract_use_aliases, resolve_class_name};

/// Relative path from project root to Laravel's Eloquent Builder source.
pub const ELOQUENT_BUILDER_REL_PATH: &str =
    "vendor/laravel/framework/src/Illuminate/Database/Eloquent/Builder.php";

/// Relative path from project root to Laravel's base Query Builder source.
/// Reached via Eloquent Builder's `@mixin` annotation.
pub const QUERY_BUILDER_REL_PATH: &str =
    "vendor/laravel/framework/src/Illuminate/Database/Query/Builder.php";

/// Relative path to Laravel's Eloquent Model — parsed for its real public
/// static methods so we can avoid shadowing them with our Builder method
/// emissions (Model's `with($relations)` has a different signature from
/// Builder's `with($relations, $callback = null)` and Intelephense shows
/// the Model one at the static position).
pub const ELOQUENT_MODEL_REL_PATH: &str =
    "vendor/laravel/framework/src/Illuminate/Database/Eloquent/Model.php";

/// Fully-qualified name of the Eloquent Builder — stamped as `source_class`
/// on every method reachable through it (including trait methods).
pub const ELOQUENT_BUILDER_FQCN: &str = "Illuminate\\Database\\Eloquent\\Builder";

/// Fully-qualified name of the base Query Builder.
pub const QUERY_BUILDER_FQCN: &str = "Illuminate\\Database\\Query\\Builder";

/// Maximum trait-resolution recursion depth. PHP allows arbitrary trait
/// nesting; this bound prevents pathological infinite-recursion bugs
/// without restricting any realistic Laravel composition.
const MAX_TRAIT_DEPTH: usize = 10;

/// A single method extracted from a Laravel framework file.
///
/// Carries enough metadata to render an Intelephense-style markdown
/// documentation panel: the source class for the header line, the
/// signature for the syntax-highlighted code block, and the PHPDoc body
/// split into summary / tags for the prose section.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ParsedMethod {
    /// Method name as the user would type it (e.g. `"where"`).
    pub name: String,
    /// Fully-qualified name of the **entry class** the method is attributed
    /// to — `Illuminate\Database\Eloquent\Builder` or
    /// `Illuminate\Database\Query\Builder`. Methods from composed traits
    /// (e.g. `BuildsQueries::first`) report the entry class, not the
    /// trait, because that matches the user's mental model: the user typed
    /// `Model::first` thinking of "a Builder method," not "a trait method."
    pub source_class: String,
    /// Method signature as it appears in source, normalised to a single
    /// line: `public function with($relations, $callback = null)`. Includes
    /// the visibility modifier, `function` keyword, name, parameter list,
    /// and return type when present. Body braces stripped.
    pub signature: String,
    /// Inferred return type, displayed inline next to the method name in
    /// the completion popup (matches Intelephense's row shape).
    ///
    /// Priority:
    /// 1. Type from the PHPDoc `@return` tag — usually richer than the PHP
    ///    declaration (e.g. `Builder<TModel>` vs untyped).
    /// 2. PHP return-type declaration parsed out of the signature
    ///    (`function foo(): SomeType`).
    /// 3. `None` when neither is present.
    pub return_type: Option<String>,
    /// First non-empty, non-`@` line of the PHPDoc — the conventional summary.
    /// `None` when the method has no docblock.
    pub summary: Option<String>,
    /// PHPDoc body with `/** */` and per-line `*` markers stripped. Used as
    /// raw source for the rendered markdown documentation. `None` when the
    /// method has no docblock.
    pub doc_body: Option<String>,
}

/// Method surface available at `Model::|` via `__callStatic`. Lazily parsed
/// from the user's `vendor/`, cached per project root on the `Backend`.
#[derive(Debug, Clone)]
pub struct BuilderMethodIndex {
    /// Methods directly on `Illuminate\Database\Eloquent\Builder` plus
    /// methods from every trait it composes (`BuildsQueries`,
    /// `ForwardsCalls`, `QueriesRelationships`, …) and their transitive
    /// trait dependencies. Dedup by name with first-wins precedence.
    pub eloquent_builder: Vec<ParsedMethod>,
    /// Methods on `Illuminate\Database\Query\Builder` plus its composed
    /// traits. Reachable from Eloquent Builder via `@mixin` and from
    /// `DB::table(...)` directly.
    pub query_builder: Vec<ParsedMethod>,
    /// Names of real public static methods on `Model` (and its composed
    /// traits). Used to suppress Builder methods of the same name at the
    /// static call position: PHP resolves `Portfolio::with(...)` to
    /// `Model::with` directly (no `__callStatic` involved), so Model's
    /// signature is authoritative — Builder's would mislead. We trust
    /// Intelephense to show Model's version for these.
    pub model_static_method_names: HashSet<String>,
}

impl BuilderMethodIndex {
    /// Combine Eloquent and Query methods into the full static-position
    /// surface. When a method name appears in both, Eloquent wins (it's
    /// the more specific override).
    ///
    /// Methods whose name appears as a real public static on Model are
    /// **suppressed** — PHP resolves `Portfolio::name(...)` to Model's
    /// real method directly (skipping `__callStatic`), so emitting our
    /// Builder version would shadow the one that actually runs. See
    /// [`BuilderMethodIndex::model_static_method_names`] for the source
    /// of truth.
    ///
    /// Result is sorted alphabetically by method name for stable popup
    /// ordering.
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

/// Parse Laravel's Builder + Query/Builder for their public method lists,
/// including methods composed in via traits.
///
/// Returns `None` if vendor isn't installed (neither file exists). If just
/// one file is missing the parse degrades gracefully — partial surface
/// beats none.
pub fn parse_builder_methods(project_root: &Path) -> Option<BuilderMethodIndex> {
    let eloquent_path = project_root.join(ELOQUENT_BUILDER_REL_PATH);
    let query_path = project_root.join(QUERY_BUILDER_REL_PATH);
    let model_path = project_root.join(ELOQUENT_MODEL_REL_PATH);

    if !eloquent_path.exists() && !query_path.exists() {
        return None;
    }

    Some(BuilderMethodIndex {
        eloquent_builder: parse_class_recursive(
            &eloquent_path,
            project_root,
            ELOQUENT_BUILDER_FQCN,
            &mut HashSet::new(),
            0,
        )
        .unwrap_or_default(),
        query_builder: parse_class_recursive(
            &query_path,
            project_root,
            QUERY_BUILDER_FQCN,
            &mut HashSet::new(),
            0,
        )
        .unwrap_or_default(),
        // Model.php is optional — if vendor is half-installed and only the
        // Builder files exist, we degrade to "no collisions known" rather
        // than failing the whole index. The suppression is a refinement,
        // not a correctness requirement.
        model_static_method_names: if model_path.exists() {
            parse_static_method_names_recursive(&model_path, project_root, &mut HashSet::new(), 0)
        } else {
            HashSet::new()
        },
    })
}

/// Walk a class/trait file and collect the names of every public **static**
/// method defined directly on it, plus the same for every trait it
/// composes (transitively). Returns just the names — we don't need
/// signatures or docblocks for the suppression check.
///
/// Uses the shared [`extract_php_structure`] walker; the only specialty
/// here is the `is_static && public && !magic` filter and the trait
/// recursion.
fn parse_static_method_names_recursive(
    path: &Path,
    project_root: &Path,
    visited: &mut HashSet<PathBuf>,
    depth: usize,
) -> HashSet<String> {
    if depth > MAX_TRAIT_DEPTH {
        return HashSet::new();
    }
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if !visited.insert(canonical) {
        return HashSet::new();
    }

    let Ok(content) = std::fs::read_to_string(path) else {
        return HashSet::new();
    };
    // Aliases parse uses tree-sitter directly because it needs the AST;
    // structure-walking uses the shared helper.
    let Ok(tree) = parse_php(&content) else {
        return HashSet::new();
    };
    let aliases = extract_use_aliases(&tree, &content);
    let structure = extract_php_structure(&content);

    let mut names = HashSet::new();
    let mut trait_imports: Vec<String> = Vec::new();

    for s in &structure.structures {
        for m in &s.methods {
            if m.visibility != PhpVisibility::Public || !m.is_static {
                continue;
            }
            if m.name.starts_with("__") {
                continue;
            }
            names.insert(m.name.clone());
        }
        trait_imports.extend(s.trait_uses.iter().cloned());
    }

    for trait_name in trait_imports {
        let fqcn = resolve_class_name(&trait_name, &aliases);
        let Some(trait_path) = resolve_illuminate_class_to_path(&fqcn, project_root) else {
            continue;
        };
        names.extend(parse_static_method_names_recursive(
            &trait_path,
            project_root,
            visited,
            depth + 1,
        ));
    }

    names
}

/// Read a PHP file, extract its public non-magic methods via the shared
/// [`extract_php_structure`] walker, then recursively follow `use TraitName;`
/// imports to gather methods from composed traits.
///
/// `entry_class` is the FQCN stamped onto every method's `source_class`,
/// including methods pulled in from traits. We attribute trait methods to
/// the entry class (Builder) rather than the trait because that matches
/// the user's mental model — they typed `Model::first` thinking "Builder
/// method," not "BuildsQueries trait method."
///
/// `visited` carries canonical paths to prevent infinite loops in
/// pathological trait graphs. `depth` enforces `MAX_TRAIT_DEPTH`.
///
/// Returns `None` only when the file can't be read or parsed. An empty
/// class body returns `Some(vec![])`.
fn parse_class_recursive(
    path: &Path,
    project_root: &Path,
    entry_class: &str,
    visited: &mut HashSet<PathBuf>,
    depth: usize,
) -> Option<Vec<ParsedMethod>> {
    if depth > MAX_TRAIT_DEPTH {
        return Some(Vec::new());
    }
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if !visited.insert(canonical) {
        return Some(Vec::new());
    }

    let content = std::fs::read_to_string(path).ok()?;
    // Aliases parse uses tree-sitter directly because it needs the AST;
    // structure-walking uses the shared helper.
    let tree = parse_php(&content).ok()?;
    let aliases = extract_use_aliases(&tree, &content);
    let structure = extract_php_structure(&content);

    let mut methods: Vec<ParsedMethod> = Vec::new();
    let mut trait_imports: Vec<String> = Vec::new();

    for s in &structure.structures {
        for m in &s.methods {
            if m.visibility != PhpVisibility::Public {
                continue;
            }
            // Skip PHP magic methods (`__construct`, `__callStatic`, …) —
            // internal plumbing, not part of the public surface.
            if m.name.starts_with("__") {
                continue;
            }
            // Dedup within a single body (paranoia — PHP doesn't allow it).
            if methods.iter().any(|existing| existing.name == m.name) {
                continue;
            }
            // Skip methods marked `@internal` — Laravel uses this to fence
            // off implementation details that aren't part of the public API.
            if m.docblock
                .as_deref()
                .is_some_and(|d| d.contains("@internal"))
            {
                continue;
            }

            let doc_body = m.docblock.clone();
            let summary = doc_body.as_deref().and_then(extract_summary);
            let return_type = extract_return_type(
                doc_body.as_deref(),
                m.return_type_raw.as_deref(),
                entry_class,
            );

            methods.push(ParsedMethod {
                name: m.name.clone(),
                source_class: entry_class.to_string(),
                signature: m.raw_signature.clone(),
                return_type,
                summary,
                doc_body,
            });
        }
        trait_imports.extend(s.trait_uses.iter().cloned());
    }

    // Resolve each trait import to a file and merge its methods. First-wins
    // precedence: direct (already-collected) methods shadow trait methods
    // of the same name. Mirrors PHP's actual trait-composition rules — a
    // method defined on the class wins over the same name from a composed
    // trait.
    for trait_name in trait_imports {
        let fqcn = resolve_class_name(&trait_name, &aliases);
        let Some(trait_path) = resolve_illuminate_class_to_path(&fqcn, project_root) else {
            continue;
        };
        let Some(trait_methods) =
            parse_class_recursive(&trait_path, project_root, entry_class, visited, depth + 1)
        else {
            continue;
        };
        for m in trait_methods {
            if !methods.iter().any(|existing| existing.name == m.name) {
                methods.push(m);
            }
        }
    }

    Some(methods)
}

/// Best-effort extraction of the method's return type, used as the
/// inline text in the completion popup row.
///
/// Priority:
/// 1. PHPDoc `@return TYPE [description]` — Laravel's annotations are
///    typically richer than the PHP declarations (generics, conditional
///    types, `$this`, …).
/// 2. PHP return-type declaration on the signature.
/// 3. `None`.
///
/// The raw type is then [`resolve_self_type`]-d so that `$this` / `self` /
/// `static` get rewritten to the entry class basename + `<static>` —
/// matching what Intelephense displays. Laravel uses `@return $this`
/// everywhere; surfacing that literal string in the completion popup is
/// useless to the user.
fn extract_return_type(
    doc_body: Option<&str>,
    php_return_type: Option<&str>,
    entry_class: &str,
) -> Option<String> {
    let raw = doc_body
        .and_then(return_type_from_phpdoc)
        .or_else(|| php_return_type.map(str::to_string))?;
    Some(resolve_self_type(&raw, entry_class))
}

// `resolve_self_type` lived here; moved to `crate::completion_format` so the
// doc-panel renderer can apply it to `@return` / `@param` types as well.
use crate::completion_format::resolve_self_type;

/// Find the first `@return TYPE …` line in a stripped docblock and return
/// its TYPE token. Handles continuation lines: `@return SomeType<int,\n
/// string>` flattens to `SomeType<int, string>`. Stops at the next `@`
/// tag.
fn return_type_from_phpdoc(doc_body: &str) -> Option<String> {
    let mut acc = String::new();
    let mut in_return = false;
    for raw in doc_body.lines() {
        let line = raw.trim();
        if line.starts_with("@return") {
            // Take whatever comes after the keyword on this line.
            let after = line.trim_start_matches("@return").trim_start();
            if !after.is_empty() {
                acc.push_str(after);
            }
            in_return = true;
            continue;
        }
        if in_return {
            // Continue the @return value across wrapped lines, until we
            // hit a blank line or another tag.
            if line.is_empty() || line.starts_with('@') {
                break;
            }
            acc.push(' ');
            acc.push_str(line);
        }
    }
    if acc.is_empty() {
        return None;
    }
    // Strip a trailing description ("Builder<static> the new query") by
    // keeping only the first whitespace-delimited type token. Laravel's
    // PHPDoc types don't use unescaped spaces, so this is safe in practice.
    let type_only = acc.split_whitespace().next().unwrap_or("").to_string();
    if type_only.is_empty() {
        None
    } else {
        Some(type_only)
    }
}

/// First non-empty, non-`@` line of a stripped docblock. Conventional
/// PHPDoc puts the summary on the first line(s) and `@param` / `@return`
/// tags after a blank line.
fn extract_summary(stripped_doc: &str) -> Option<String> {
    stripped_doc
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('@'))
        .map(str::to_string)
}

/// Resolve a fully-qualified Illuminate class name to its file path under
/// the project's vendor directory. Returns `None` for non-Illuminate
/// namespaces (we don't follow trait composition outside the framework)
/// or when the file doesn't exist.
fn resolve_illuminate_class_to_path(fqcn: &str, project_root: &Path) -> Option<PathBuf> {
    let stripped = fqcn.trim_start_matches('\\');
    let rel = stripped.strip_prefix("Illuminate\\")?.replace('\\', "/");
    let path = project_root
        .join("vendor/laravel/framework/src/Illuminate")
        .join(rel)
        .with_extension("php");
    if path.exists() {
        Some(path)
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
