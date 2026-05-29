//! Laravel-aware class walker — the single source of truth for "what
//! does this class look like to Laravel?"
//!
//! Built on top of the generic [`crate::laravel_introspector::walker::extract_php_structure`]
//! walker. That helper handles per-file PHP structure parsing; this one
//! layers in Laravel's idiosyncrasies:
//!
//! - Inheritance + trait composition recursion (replaces three separate
//!   re-implementations in `model_analyzer`, `vendor_parser`, and the
//!   former `scope_extractor`)
//! - Eloquent local scope detection in both styles (prefix `scopeFoo` /
//!   `#[Scope]` attribute) with name normalisation
//! - Accessor detection (old-style `getXxxAttribute` / new-style
//!   `Attribute`-returning methods)
//! - Relationship detection (return-type or body-call patterns)
//! - `$casts` property + `casts()` method extraction
//! - `$table` extraction
//! - `__callStatic` surface for Builder-like classes (the methods
//!   exposed at `Model::|` via magic forwarding)
//!
//! ## Consumers
//!
//! - `model_analyzer::ModelMetadata` is now a thin wrapper that reads
//!   the relevant fields off a [`ClassView`].
//! - `vendor_parser::BuilderMethodIndex` assembles its three-class index
//!   (Eloquent Builder + Query Builder + Model static names) from three
//!   [`ClassView`]s.
//! - The Phase 1 method-name completion handler in `main.rs` reads
//!   `view.scopes` and the Builder's `callstatic_surface` from one place.
//!
//! ## Caching
//!
//! No file-mtime cache here yet — callers (chiefly `Backend`) already
//! hold their own per-project-root caches for the expensive bits
//! (BuilderMethodIndex for vendor parsing). User-model views are
//! recomputed per completion request; the parse is fast enough
//! (microseconds per file) that this hasn't been a bottleneck.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::class_locator::find_php_class_file;
use crate::laravel_introspector::walker::{
    extract_php_structure, PhpMethodInfo, PhpPropertyInfo, PhpStructure, PhpStructureKind,
    PhpVisibility,
};
use crate::query_chain::extract_use_aliases;

/// Maximum inheritance / trait recursion depth. Matches the bounds used
/// by the previous separate walkers; protects against pathological
/// trait composition graphs.
const MAX_DEPTH: usize = 10;

/// Fully-qualified Laravel base classes / attributes referenced during
/// classification. Centralised so version-bumps to Laravel that move
/// these only require one edit.
pub const ELOQUENT_MODEL_FQCN: &str = "Illuminate\\Database\\Eloquent\\Model";
pub const ELOQUENT_BUILDER_FQCN: &str = "Illuminate\\Database\\Eloquent\\Builder";
pub const QUERY_BUILDER_FQCN: &str = "Illuminate\\Database\\Query\\Builder";
pub const SCOPE_ATTRIBUTE_FQCN: &str = "Illuminate\\Database\\Eloquent\\Attributes\\Scope";
pub const SOFT_DELETES_FQCN: &str = "Illuminate\\Database\\Eloquent\\SoftDeletes";
pub const AUTHENTICATABLE_TRAIT_FQCN: &str = "Illuminate\\Auth\\Authenticatable";
pub const FOUNDATION_AUTH_USER_FQCN: &str = "Illuminate\\Foundation\\Auth\\User";

/// What kind of Laravel class this is. Drives which fields on
/// [`ClassView`] are populated — there's no `scopes` on a
/// Builder file and no `callstatic_surface` on a leaf Model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LaravelClassKind {
    /// Extends `Illuminate\Database\Eloquent\Model` (transitively).
    Model,
    /// `Illuminate\Database\Eloquent\Builder` itself.
    EloquentBuilder,
    /// `Illuminate\Database\Query\Builder` itself.
    QueryBuilder,
    /// Anything else — not a model, not a builder. Used for traits,
    /// helper classes, etc. View's Laravel-specific fields stay empty.
    Other,
}

/// A method (or property) gathered from somewhere in the class
/// hierarchy. Tracks both the underlying [`PhpMethodInfo`] and the
/// FQCN of the class that actually declared it — useful for jumping
/// to the right source file and for the docs-panel header line.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResolvedMember<T> {
    pub value: T,
    /// Fully-qualified name of the class / trait that declared this
    /// member. May be the view's own class, a parent class, or a
    /// composed trait.
    pub source_class: String,
    /// `0` = directly declared on the view's class. `> 0` = inherited
    /// (parent, grandparent, …) or trait-composed.
    pub depth: u32,
    /// `true` when this came in via `use TraitName;` rather than
    /// `extends`.
    pub from_trait: bool,
}

/// An Eloquent local scope. Surfaces in the completion popup as a
/// callable method on `Model::|`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ScopeInfo {
    /// Callable name — `active` (from `scopeActive`), `published`
    /// (from `#[Scope] public function published(...)`).
    pub name: String,
    /// FQCN of the class that declared the underlying method.
    pub source_class: String,
    /// Method signature with the auto-supplied first `Builder $query`
    /// parameter stripped and the method name rewritten to the
    /// callable form. Renders in the docs panel's PHP code block.
    pub signature: String,
    /// PHPDoc body with markers stripped.
    pub doc_body: Option<String>,
    /// First non-empty, non-`@` line of the PHPDoc.
    pub summary: Option<String>,
    /// Which detection pattern matched.
    pub style: ScopeStyle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScopeStyle {
    /// `public function scopeActive(...)`.
    Prefix,
    /// `#[Scope] public function active(...)`.
    Attribute,
}

/// A database column known about this model at the source level —
/// i.e. derivable from the model's own PHP source (`$fillable`,
/// `$casts`, `$dates`, `$attributes`), its trait composition
/// (`SoftDeletes` implies `deleted_at`), or Laravel's hard
/// conventions (`id`, `created_at`, `updated_at`).
///
/// Drives Phase 3 dynamic `where{Column}` completion-item synthesis.
/// The DB schema can supplement this when the source surface is
/// sparse (`$guarded = []` models that declare nothing) — that
/// fallback happens at the emission site, not here.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ColumnInfo {
    /// snake_case column name as it appears in the DB / migration.
    pub name: String,
    /// PHP type a user typing `where{Column}($value)` should pass.
    /// `string`, `int`, `bool`, `float`, `array`, `Carbon`, or
    /// `mixed` when we have no signal.
    pub php_type: String,
    /// Where this column knowledge came from. Surfaces in the
    /// completion doc panel as a brief provenance line.
    pub source: ColumnSource,
}

/// Provenance signal for a [`ColumnInfo`]. Each variant maps to a
/// distinct extraction path so the doc panel can show *why* we
/// believe this column exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColumnSource {
    /// Listed in the model's `$fillable` array.
    Fillable,
    /// Listed as a key in the model's `$casts` array (or
    /// `casts()` method on Laravel 11+).
    Cast,
    /// Listed in the legacy `$dates` array.
    Dates,
    /// Listed as a key in the model's `$attributes` (defaults) array.
    Attributes,
    /// Laravel hard convention: `id`, `created_at`, `updated_at`.
    Convention,
    /// Implied by a composed trait — e.g. `SoftDeletes` adds
    /// `deleted_at`.
    Trait,
    /// Implied by a parent class — e.g. `Authenticatable` adds
    /// `email_verified_at`, `remember_token`, `password`.
    ParentClass,
}

/// Eloquent accessor — `getXxxAttribute(): Type` (old-style) or a
/// method returning `Attribute` (new-style).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AccessorInfo {
    /// Property name as the user accesses it (`$model->first_name`)
    /// — snake_cased.
    pub property_name: String,
    /// Declared PHP return type, if any. `None` for new-style
    /// `Attribute`-returning accessors (type comes from the
    /// `Attribute::make()` call body, which we don't parse here).
    pub return_type: Option<String>,
    /// `true` for `firstName(): Attribute` style.
    pub is_attribute_style: bool,
    /// FQCN of the class that declared the accessor.
    pub source_class: String,
}

/// Eloquent relationship — a method that returns a `Relation` subclass
/// (or calls one in its body).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RelationshipInfo {
    pub method_name: String,
    /// `hasMany`, `belongsTo`, `morphMany`, etc.
    pub relationship_type: String,
    /// Related model FQCN (resolved through use aliases), when the
    /// method body's first `SomeModel::class` arg could be extracted.
    pub related_model: Option<String>,
    /// FQCN of the class that declared the relationship method.
    pub source_class: String,
}

/// A method extracted from a Builder-like class (or one of its traits)
/// that's reachable at `Model::|` via `__callStatic` forwarding.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BuilderMethod {
    pub name: String,
    /// FQCN of the **entry class** (always Builder, even for trait
    /// methods) — matches what the user calls.
    pub source_class: String,
    pub signature: String,
    /// Inferred return type with `$this`/`self`/`static` resolved to
    /// the entry-class form. `None` when neither PHPDoc `@return` nor
    /// a PHP return-type declaration is present.
    pub return_type: Option<String>,
    pub summary: Option<String>,
    pub doc_body: Option<String>,
}

/// Everything we know about a Laravel class — direct structure,
/// resolved inheritance + trait composition, and pre-computed
/// Laravel-specific surfaces (scopes, accessors, relationships, casts,
/// `__callStatic` surface).
///
/// Built by [`analyze`]. Consumers read whichever fields
/// they care about; fields not relevant to the class's kind stay
/// empty.
#[derive(Debug, Clone)]
pub struct ClassView {
    pub file_path: PathBuf,
    pub fqcn: String,
    pub namespace: Option<String>,
    pub class_name: String,
    pub kind: LaravelClassKind,

    /// The view's own file structure — direct methods, properties,
    /// trait_uses, etc. Use the `all_*` fields for inheritance-resolved
    /// views.
    pub direct: PhpStructure,

    /// Every method reachable on this class, including inherited and
    /// trait-composed ones. First-declared wins on name collision
    /// (matches PHP's actual resolution).
    pub all_methods: Vec<ResolvedMember<PhpMethodInfo>>,
    pub all_properties: Vec<ResolvedMember<PhpPropertyInfo>>,

    // ---- Model-specific surfaces ----

    /// Eloquent local scopes. Empty for non-models.
    pub scopes: Vec<ScopeInfo>,
    /// Old-style + new-style accessors. Empty for non-models.
    pub accessors: Vec<AccessorInfo>,
    /// Eloquent relationships. Empty for non-models.
    pub relationships: Vec<RelationshipInfo>,
    /// Cast map (`'col' => 'type'`). Empty for non-models.
    pub casts: HashMap<String, String>,
    /// Explicit `$table = '...'`. `None` for non-models or when
    /// implicit naming applies.
    pub table_name: Option<String>,
    /// Source-derived column surface — every column this model
    /// can be reasoned about without touching the DB. Drives
    /// Phase 3 dynamic `where{Column}` item synthesis. Empty for
    /// non-models or `$guarded = []` models that declare nothing
    /// (the emission site falls back to the DB schema for those).
    pub column_surface: Vec<ColumnInfo>,

    // ---- Builder-specific surface ----

    /// Methods exposed at `Model::|` via `__callStatic`. Populated
    /// for Eloquent Builder and Query Builder; empty for everything
    /// else.
    pub callstatic_surface: Vec<BuilderMethod>,
}

/// Single-file analysis: parse `content` and compute every Laravel
/// surface (scopes, accessors, relationships, casts, table) as if the
/// class were a Model — but without walking parent classes or composed
/// traits (they require file paths we don't have).
///
/// Used by callers that operate on in-memory PHP source (unsaved
/// buffers, test fixtures, helpers like `ModelMetadata::from_content`).
/// `kind` is forced to `Model` since we have no way to verify the
/// inheritance chain.
///
/// For full analysis with inheritance + trait composition, use
/// [`analyze`] (file-path based).
pub fn analyze_content(content: &str) -> Option<ClassView> {
    // Tree-sitter-php treats input outside `<?php` tags as HTML, so a
    // bare class fragment yields zero structures. Real Laravel files
    // always carry the open tag; this auto-prepend just makes tests
    // and ad-hoc fragments work the same way.
    if !content.trim_start().starts_with("<?php") {
        let prefixed = format!("<?php\n{}", content);
        return analyze_from_prefixed(&prefixed);
    }
    analyze_from_prefixed(content)
}

/// Inner body of [`analyze_content`] — assumes the
/// content already carries a `<?php` opening tag.
fn analyze_from_prefixed(content: &str) -> Option<ClassView> {
    let structure = extract_php_structure(content);
    let aliases = parse_use_aliases(content);

    let direct = structure
        .structures
        .iter()
        .find(|s| s.kind == PhpStructureKind::Class)
        .cloned()?;

    let class_name = direct.name.clone();
    let namespace = structure.namespace.clone();
    let fqcn = match &namespace {
        Some(ns) => format!("{ns}\\{}", class_name),
        None => class_name.clone(),
    };

    // Promote direct members to ResolvedMember<_> with depth=0 / no
    // trait origin — single-file view, everything is "direct."
    let all_methods: Vec<ResolvedMember<PhpMethodInfo>> = direct
        .methods
        .iter()
        .map(|m| ResolvedMember {
            value: m.clone(),
            source_class: fqcn.clone(),
            depth: 0,
            from_trait: false,
        })
        .collect();
    let all_properties: Vec<ResolvedMember<PhpPropertyInfo>> = direct
        .properties
        .iter()
        .map(|p| ResolvedMember {
            value: p.clone(),
            source_class: fqcn.clone(),
            depth: 0,
            from_trait: false,
        })
        .collect();

    let casts = compute_casts(&all_methods, &all_properties);
    let column_surface = compute_column_surface(&all_methods, &all_properties, &casts);
    Some(ClassView {
        file_path: PathBuf::new(),
        fqcn,
        namespace: namespace.clone(),
        class_name,
        kind: LaravelClassKind::Model,
        direct,
        scopes: compute_scopes(&all_methods, &aliases, &namespace),
        accessors: compute_accessors(&all_methods),
        relationships: compute_relationships(&all_methods, &aliases, namespace.as_deref()),
        casts,
        table_name: compute_table_name(&all_properties),
        column_surface,
        callstatic_surface: Vec::new(),
        all_methods,
        all_properties,
    })
}

/// Run the Laravel-aware class analysis on `file_path`. Returns a
/// fully-populated [`ClassView`] with inheritance + trait
/// composition resolved and Laravel surfaces (scopes, casts,
/// relationships, etc.) pre-computed.
///
/// Returns `None` only when the file can't be read or doesn't declare
/// any class — empty class bodies still yield a view (with empty
/// surfaces).
pub fn analyze(file_path: &Path, project_root: &Path) -> Option<ClassView> {
    let content = std::fs::read_to_string(file_path).ok()?;
    let structure = extract_php_structure(&content);
    let aliases = parse_use_aliases(&content);

    // Pick the first class declaration; most Laravel files have exactly
    // one. Interfaces/traits/enums aren't classified as Laravel "class"
    // views (traits get visited via composition, not directly here).
    let direct = structure
        .structures
        .iter()
        .find(|s| s.kind == PhpStructureKind::Class)
        .cloned()?;

    let class_name = direct.name.clone();
    let namespace = structure.namespace.clone();
    let fqcn = match &namespace {
        Some(ns) => format!("{ns}\\{}", class_name),
        None => class_name.clone(),
    };

    // Resolve inheritance + trait composition. Walks parent classes and
    // all `use TraitName;` declarations, collecting methods + properties
    // with origin metadata. First-declared wins on name collision (PHP
    // resolution order: child shadows parent, parent shadows trait).
    let mut all_methods: Vec<ResolvedMember<PhpMethodInfo>> = Vec::new();
    let mut all_properties: Vec<ResolvedMember<PhpPropertyInfo>> = Vec::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    walk_class_chain(
        file_path,
        project_root,
        &mut all_methods,
        &mut all_properties,
        &mut visited,
        0,
    );

    let kind = classify_kind(&fqcn, file_path, project_root);

    // Compute every surface unconditionally — the work is cheap and the
    // surfaces are empty by construction when nothing matches (e.g. a
    // class with no `scope*` methods yields `scopes = []`).
    //
    // Gating on `kind` was a footgun: when a child class extends a
    // vendor class whose file we can't locate, `classify_kind` falls
    // back to `Other` even though the child is unambiguously a Model
    // (the test fixture `Orphan extends SomeVendorClass` declares
    // `$table = 'orphans'`). The caller asked about a specific file;
    // surface whatever Laravel patterns it carries.
    let scopes = compute_scopes(&all_methods, &aliases, &namespace);
    let accessors = compute_accessors(&all_methods);
    let relationships = compute_relationships(&all_methods, &aliases, namespace.as_deref());
    let casts = compute_casts(&all_methods, &all_properties);
    let table_name = compute_table_name(&all_properties);
    let column_surface = compute_column_surface(&all_methods, &all_properties, &casts);
    let callstatic_surface = match kind {
        LaravelClassKind::EloquentBuilder | LaravelClassKind::QueryBuilder => {
            compute_callstatic_surface(&all_methods, &fqcn)
        }
        _ => Vec::new(),
    };

    Some(ClassView {
        file_path: file_path.to_path_buf(),
        fqcn,
        namespace,
        class_name,
        kind,
        direct,
        all_methods,
        all_properties,
        scopes,
        accessors,
        relationships,
        casts,
        table_name,
        column_surface,
        callstatic_surface,
    })
}

/// Walk the inheritance + trait composition chain rooted at `path`.
/// Pushes each method / property onto `methods` / `properties` with
/// origin metadata (depth, source class, from_trait). First-declared
/// wins on name collision — matches PHP's method resolution order.
///
/// Stops at Laravel's base `Model` (parent walk only — we don't
/// enumerate the framework's methods as "inherited" on user models;
/// that's `vendor_parser`'s job for Builder-style classes).
fn walk_class_chain(
    path: &Path,
    project_root: &Path,
    methods: &mut Vec<ResolvedMember<PhpMethodInfo>>,
    properties: &mut Vec<ResolvedMember<PhpPropertyInfo>>,
    visited: &mut HashSet<PathBuf>,
    depth: u32,
) {
    if depth as usize > MAX_DEPTH {
        return;
    }
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if !visited.insert(canonical) {
        return;
    }

    let Ok(content) = std::fs::read_to_string(path) else {
        return;
    };
    let structure = extract_php_structure(&content);
    let aliases = parse_use_aliases(&content);
    let namespace = structure.namespace.clone();

    for class in &structure.structures {
        // The FQCN to stamp on each member this class declares. Traits
        // get stamped with their own FQCN (resolved when walked).
        let source_class = match &namespace {
            Some(ns) => format!("{ns}\\{}", class.name),
            None => class.name.clone(),
        };
        let from_trait = matches!(class.kind, PhpStructureKind::Trait);

        for method in &class.methods {
            if !methods.iter().any(|m| m.value.name == method.name) {
                methods.push(ResolvedMember {
                    value: method.clone(),
                    source_class: source_class.clone(),
                    depth,
                    from_trait,
                });
            }
        }
        // Properties are NOT deduped by name — multiple classes in the
        // chain can declare the same property (`$casts`, `$fillable`,
        // etc.) and the semantics depend on the consumer. The cast
        // extractor merges entries across the chain; the table-name
        // extractor takes the child's value as the override. Leaving
        // all definitions in the list lets each consumer pick.
        for property in &class.properties {
            properties.push(ResolvedMember {
                value: property.clone(),
                source_class: source_class.clone(),
                depth,
                from_trait,
            });
        }

        // Recurse into composed traits at this depth (traits compose
        // INTO the class; they're not "deeper" inheritance).
        for trait_name in &class.trait_uses {
            let fqcn = resolve_to_fqcn(trait_name, namespace.as_deref(), &aliases);
            if let Some(trait_path) = find_php_class_file(&fqcn, project_root) {
                walk_class_chain(
                    &trait_path,
                    project_root,
                    methods,
                    properties,
                    visited,
                    depth + 1,
                );
            }
        }

        // Recurse into the parent class. Stop at Eloquent's base Model
        // — the framework's methods on Model are exposed via the
        // Builder __callStatic surface (separate concern); we don't
        // pull them in here as "inherited" on user models.
        if let Some(parent_raw) = &class.extends_raw {
            let parent_fqcn = resolve_to_fqcn(parent_raw, namespace.as_deref(), &aliases);
            if parent_fqcn != ELOQUENT_MODEL_FQCN {
                if let Some(parent_path) = find_php_class_file(&parent_fqcn, project_root) {
                    walk_class_chain(
                        &parent_path,
                        project_root,
                        methods,
                        properties,
                        visited,
                        depth + 1,
                    );
                }
            }
        }
    }
}

/// Determine the Laravel class kind for a given FQCN. Builder classes
/// are matched by FQCN; Model-ness is determined by walking up the
/// inheritance chain looking for `Illuminate\Database\Eloquent\Model`.
fn classify_kind(fqcn: &str, file_path: &Path, project_root: &Path) -> LaravelClassKind {
    if fqcn == ELOQUENT_BUILDER_FQCN {
        return LaravelClassKind::EloquentBuilder;
    }
    if fqcn == QUERY_BUILDER_FQCN {
        return LaravelClassKind::QueryBuilder;
    }
    // For everything else, check if it extends Eloquent's Model.
    if extends_eloquent_model(file_path, project_root) {
        return LaravelClassKind::Model;
    }
    LaravelClassKind::Other
}

/// Walk a class's `extends` chain and return `true` if any ancestor is
/// `Illuminate\Database\Eloquent\Model`. The canonical implementation —
/// `model_analyzer::ModelMetadata::extends_eloquent_model` is a thin
/// wrapper that delegates here so the LSP has one walker, period.
///
/// Bounded by [`MAX_DEPTH`] and cycle-safe via a visited set. Uses
/// [`crate::class_locator::find_php_class_file_in_app_or_vendor`] so
/// vendor-side base classes (e.g. an SDK's `BaseModel`) are found
/// through Composer's autoload data.
pub fn extends_eloquent_model(file_path: &Path, project_root: &Path) -> bool {
    let mut visited: HashSet<PathBuf> = HashSet::new();
    let mut current = Some(file_path.to_path_buf());
    let mut depth = 0usize;
    while let Some(p) = current.take() {
        if depth > MAX_DEPTH {
            return false;
        }
        depth += 1;
        let canonical = p.canonicalize().unwrap_or_else(|_| p.clone());
        if !visited.insert(canonical) {
            return false;
        }
        let Ok(content) = std::fs::read_to_string(&p) else {
            return false;
        };
        let structure = extract_php_structure(&content);
        let aliases = parse_use_aliases(&content);
        let Some(class) = structure
            .structures
            .iter()
            .find(|s| s.kind == PhpStructureKind::Class)
        else {
            return false;
        };
        let Some(parent_raw) = class.extends_raw.as_deref() else {
            return false;
        };
        let parent_fqcn = resolve_to_fqcn(parent_raw, structure.namespace.as_deref(), &aliases);
        let resolved_basename = parent_fqcn.rsplit('\\').next().unwrap_or(&parent_fqcn);
        if resolved_basename == "Model" {
            return true;
        }
        current =
            crate::class_locator::find_php_class_file_in_app_or_vendor(&parent_fqcn, project_root);
    }
    false
}

// ---- Scope computation -------------------------------------------------

fn compute_scopes(
    methods: &[ResolvedMember<PhpMethodInfo>],
    aliases: &HashMap<String, String>,
    file_namespace: &Option<String>,
) -> Vec<ScopeInfo> {
    let mut scopes: Vec<ScopeInfo> = Vec::new();
    for member in methods {
        if member.value.visibility != PhpVisibility::Public
            || member.value.name.starts_with("__")
        {
            continue;
        }

        let (callable_name, style) = if has_scope_attribute(&member.value, aliases) {
            (member.value.name.clone(), ScopeStyle::Attribute)
        } else if let Some(rest) = member.value.name.strip_prefix("scope") {
            // Real scopes always have an uppercase char after `scope`
            // — filters out coincidental `scope`/`scoped`/etc.
            let Some(first) = rest.chars().next() else {
                continue;
            };
            if !first.is_ascii_uppercase() {
                continue;
            }
            (lowercase_first_char(rest), ScopeStyle::Prefix)
        } else {
            continue;
        };

        // First-declared wins (matches PHP resolution since
        // walk_class_chain already enforces it on the member list).
        if scopes.iter().any(|s| s.name == callable_name) {
            continue;
        }

        let renamed = rename_method_in_signature(
            &member.value.raw_signature,
            &member.value.name,
            &callable_name,
        );
        let signature = signature_without_query_param(&renamed);

        scopes.push(ScopeInfo {
            name: callable_name,
            source_class: member.source_class.clone(),
            signature,
            doc_body: member.value.docblock.clone(),
            summary: member
                .value
                .docblock
                .as_deref()
                .and_then(first_non_tag_line),
            style,
        });
        let _ = file_namespace; // kept for symmetry / future use
    }
    scopes
}

fn has_scope_attribute(
    method: &PhpMethodInfo,
    aliases: &HashMap<String, String>,
) -> bool {
    method.attributes.iter().any(|attr| {
        let fqcn = resolve_to_fqcn(attr, None, aliases);
        fqcn == SCOPE_ATTRIBUTE_FQCN
    })
}

// ---- Accessor computation ----------------------------------------------

fn compute_accessors(methods: &[ResolvedMember<PhpMethodInfo>]) -> Vec<AccessorInfo> {
    let mut accessors = Vec::new();
    for member in methods {
        let name = &member.value.name;

        // Old-style: getXxxAttribute
        if let Some(middle) = name.strip_prefix("get").and_then(|s| s.strip_suffix("Attribute"))
        {
            if !middle.is_empty() {
                accessors.push(AccessorInfo {
                    property_name: pascal_to_snake(middle),
                    return_type: member.value.return_type_raw.clone(),
                    is_attribute_style: false,
                    source_class: member.source_class.clone(),
                });
                continue;
            }
        }

        // New-style: any method returning Attribute (possibly namespaced).
        let returns_attribute = member
            .value
            .return_type_raw
            .as_deref()
            .map(|t| {
                t.trim_start_matches('?')
                    .rsplit('\\')
                    .next()
                    .unwrap_or("")
                    == "Attribute"
            })
            .unwrap_or(false);
        if returns_attribute {
            accessors.push(AccessorInfo {
                property_name: camel_to_snake(name),
                return_type: None,
                is_attribute_style: true,
                source_class: member.source_class.clone(),
            });
        }
    }
    accessors
}

// ---- Relationship computation ------------------------------------------

const RELATIONSHIP_KINDS: &[&str] = &[
    "belongsToMany",
    "belongsTo",
    "hasManyThrough",
    "hasOneThrough",
    "hasMany",
    "hasOne",
    "morphToMany",
    "morphedByMany",
    "morphMany",
    "morphOne",
    "morphTo",
];

fn compute_relationships(
    methods: &[ResolvedMember<PhpMethodInfo>],
    aliases: &HashMap<String, String>,
    file_namespace: Option<&str>,
) -> Vec<RelationshipInfo> {
    let mut out: Vec<RelationshipInfo> = Vec::new();
    let resolve_class = |bare: String| -> String {
        crate::laravel_introspector::model_metadata::resolve_to_fqcn(&bare, file_namespace, aliases)
    };

    for member in methods {
        let method = &member.value;
        if method.visibility != PhpVisibility::Public {
            continue;
        }
        let mut kind: Option<&'static str> = None;

        // Return-type strategy.
        if let Some(ret) = method.return_type_raw.as_deref() {
            let basename = ret
                .trim_start_matches('?')
                .rsplit('\\')
                .next()
                .unwrap_or("")
                .trim();
            kind = RELATIONSHIP_KINDS
                .iter()
                .find(|k| basename.eq_ignore_ascii_case(k))
                .copied();
        }

        // Body strategy.
        if kind.is_none() {
            if let Some(body) = method.body_source.as_deref() {
                for k in RELATIONSHIP_KINDS {
                    let needle = format!("$this->{}(", k);
                    if body.contains(&needle) {
                        kind = Some(*k);
                        break;
                    }
                }
            }
        }

        let Some(rel_type) = kind else {
            continue;
        };
        if out.iter().any(|r| r.method_name == method.name) {
            continue;
        }
        let related_model = method
            .body_source
            .as_deref()
            .and_then(first_class_constant_arg)
            .map(&resolve_class);

        out.push(RelationshipInfo {
            method_name: method.name.clone(),
            relationship_type: rel_type.to_string(),
            related_model,
            source_class: member.source_class.clone(),
        });
    }
    out
}

// ---- Cast computation --------------------------------------------------

fn compute_casts(
    methods: &[ResolvedMember<PhpMethodInfo>],
    properties: &[ResolvedMember<PhpPropertyInfo>],
) -> HashMap<String, String> {
    let mut casts = HashMap::new();

    // `$casts` is declared on potentially every class in the chain
    // (model + parent + traits). Properties are listed child-first
    // (depth 0 → N), so iterating in order and using `entry().or_insert`
    // means child entries win on key collision — same semantics PHP
    // applies when methods on the child access `$this->casts`.
    for prop in properties.iter().filter(|p| p.value.name == "casts") {
        if let Some(default) = prop.value.default_value.as_deref() {
            for (k, v) in parse_php_array_to_string_map(default) {
                casts.entry(k).or_insert(v);
            }
        }
    }

    // `casts()` method (Laravel 11+ style). Same child-wins semantics.
    for method in methods
        .iter()
        .filter(|m| m.value.name == "casts" && m.value.parameters.is_empty())
    {
        if let Some(body) = method.value.body_source.as_deref() {
            if let Some(return_idx) = body.find("return") {
                let after = &body[return_idx + "return".len()..];
                for (k, v) in parse_php_array_to_string_map(after) {
                    casts.entry(k).or_insert(v);
                }
            }
        }
    }
    casts
}

// ---- Column-surface computation ----------------------------------------

/// Build the source-derived column surface for a model. Combines every
/// signal the walker has about which columns this model knows:
/// `$fillable` / `$casts` / `$dates` / `$attributes` arrays, the
/// SoftDeletes / Authenticatable trait + parent implications, and
/// Laravel's hard conventions (`id`, `created_at`, `updated_at` unless
/// `$timestamps = false`).
///
/// First-seen-wins on column-name collision so cast-typed entries
/// beat untyped `$fillable` mentions (you'd want
/// `whereOptions(array $v)` from `'options' => 'array'`, not
/// `whereOptions(mixed $v)` from the bare `'options'` in `$fillable`).
///
/// Returns an empty vector for non-models (the caller's emission path
/// already gates on `extends_eloquent_model`, so non-empty surfaces
/// here mean a real Model).
fn compute_column_surface(
    methods: &[ResolvedMember<PhpMethodInfo>],
    properties: &[ResolvedMember<PhpPropertyInfo>],
    casts: &HashMap<String, String>,
) -> Vec<ColumnInfo> {
    let mut columns: Vec<ColumnInfo> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    let add = |name: String, php_type: String, source: ColumnSource,
               columns: &mut Vec<ColumnInfo>,
               seen: &mut HashSet<String>| {
        if seen.insert(name.clone()) {
            columns.push(ColumnInfo {
                name,
                php_type,
                source,
            });
        }
    };

    // Casts come first — their types are explicit and beat any
    // weaker signal (e.g. an untyped `$fillable` mention of the
    // same column).
    let mut cast_keys: Vec<&String> = casts.keys().collect();
    cast_keys.sort();
    for key in cast_keys {
        let php_type =
            crate::laravel_introspector::model_metadata::map_cast_to_php_type(&casts[key]);
        add(
            key.clone(),
            php_type,
            ColumnSource::Cast,
            &mut columns,
            &mut seen,
        );
    }

    // `$fillable` — list of strings, no types. Default each to a
    // conventional type based on column name.
    for prop in properties.iter().filter(|p| p.value.name == "fillable") {
        if let Some(default) = prop.value.default_value.as_deref() {
            for name in
                crate::laravel_introspector::model_metadata::parse_string_array_public(default)
            {
                let php_type = conventional_php_type(&name);
                add(
                    name,
                    php_type,
                    ColumnSource::Fillable,
                    &mut columns,
                    &mut seen,
                );
            }
        }
    }

    // `$dates` (legacy) — list of strings, all dates.
    for prop in properties.iter().filter(|p| p.value.name == "dates") {
        if let Some(default) = prop.value.default_value.as_deref() {
            for name in
                crate::laravel_introspector::model_metadata::parse_string_array_public(default)
            {
                add(
                    name,
                    "Carbon".to_string(),
                    ColumnSource::Dates,
                    &mut columns,
                    &mut seen,
                );
            }
        }
    }

    // `$attributes` — keyed array of defaults. Keys are column names;
    // values may be bool/int/null/expression so we use a key-only
    // parser that doesn't care about value shape.
    for prop in properties.iter().filter(|p| p.value.name == "attributes") {
        if let Some(default) = prop.value.default_value.as_deref() {
            for key in
                crate::laravel_introspector::model_metadata::parse_array_keys_public(default)
            {
                let php_type = conventional_php_type(&key);
                add(
                    key,
                    php_type,
                    ColumnSource::Attributes,
                    &mut columns,
                    &mut seen,
                );
            }
        }
    }

    // SoftDeletes trait → `deleted_at` (datetime). Detected by
    // looking for any inherited member whose source_class is the
    // SoftDeletes trait — `walk_class_chain` stamps trait FQCN onto
    // members it composes in.
    if chain_contains(methods, properties, SOFT_DELETES_FQCN) {
        add(
            "deleted_at".to_string(),
            "Carbon".to_string(),
            ColumnSource::Trait,
            &mut columns,
            &mut seen,
        );
    }

    // Authenticatable trait / Foundation\Auth\User parent →
    // adds the canonical auth columns. Both signals point at the
    // same column set; we only need one to fire.
    if chain_contains(methods, properties, AUTHENTICATABLE_TRAIT_FQCN)
        || chain_contains(methods, properties, FOUNDATION_AUTH_USER_FQCN)
    {
        for (name, ty) in [
            ("name", "string"),
            ("email", "string"),
            ("email_verified_at", "Carbon"),
            ("password", "string"),
            ("remember_token", "string"),
        ] {
            add(
                name.to_string(),
                ty.to_string(),
                ColumnSource::ParentClass,
                &mut columns,
                &mut seen,
            );
        }
    }

    // Laravel conventions. `id` is always present unless explicitly
    // suppressed (which we don't try to detect — opt-out is rare and
    // the worst case is one spurious `whereId` item that PHP would
    // still resolve via Builder's `whereId`-routed dynamicWhere).
    add(
        "id".to_string(),
        "int".to_string(),
        ColumnSource::Convention,
        &mut columns,
        &mut seen,
    );

    // Timestamps default-on; only suppress when `$timestamps = false`
    // appears as a property default. We don't bother with the rare
    // `public $timestamps = false;` static-equivalent forms — those
    // are exotic enough that a false positive is acceptable.
    if !timestamps_disabled(properties) {
        add(
            "created_at".to_string(),
            "Carbon".to_string(),
            ColumnSource::Convention,
            &mut columns,
            &mut seen,
        );
        add(
            "updated_at".to_string(),
            "Carbon".to_string(),
            ColumnSource::Convention,
            &mut columns,
            &mut seen,
        );
    }

    columns
}

/// Heuristic PHP type for a column name absent stronger signal.
/// Laravel's conventions: `id` and `*_id` are integers (foreign keys),
/// `*_at` columns are datetimes (Laravel auto-casts timestamps), and
/// everything else is `mixed` — we don't try to infer from prefixes
/// like `is_` / `has_` because those aren't auto-cast by Laravel and
/// guessing wrong is worse than `mixed`.
fn conventional_php_type(column: &str) -> String {
    if column == "id" || column.ends_with("_id") {
        return "int".to_string();
    }
    if column.ends_with("_at") {
        return "Carbon".to_string();
    }
    "mixed".to_string()
}

/// Returns true when any inherited member's `source_class` matches
/// `target_fqcn`. Cheaper than re-walking the chain — the methods
/// and properties lists already carry the FQCN provenance we need.
fn chain_contains(
    methods: &[ResolvedMember<PhpMethodInfo>],
    properties: &[ResolvedMember<PhpPropertyInfo>],
    target_fqcn: &str,
) -> bool {
    methods.iter().any(|m| m.source_class == target_fqcn)
        || properties.iter().any(|p| p.source_class == target_fqcn)
}

/// Returns true when the model declares `public $timestamps = false`
/// (or any visibility). Conservative — only the literal `false` value
/// counts; `$timestamps = SomeConst::FALSE` and other indirections are
/// treated as "timestamps on" (the dominant case).
fn timestamps_disabled(properties: &[ResolvedMember<PhpPropertyInfo>]) -> bool {
    properties
        .iter()
        .filter(|p| p.value.name == "timestamps")
        .any(|p| {
            p.value
                .default_value
                .as_deref()
                .is_some_and(|v| v.trim() == "false")
        })
}

// ---- Table-name computation --------------------------------------------

fn compute_table_name(properties: &[ResolvedMember<PhpPropertyInfo>]) -> Option<String> {
    let prop = properties.iter().find(|p| p.value.name == "table")?;
    let default = prop.value.default_value.as_deref()?;
    unquote_string_literal(default)
}

// ---- __callStatic surface (Builder) ------------------------------------

fn compute_callstatic_surface(
    methods: &[ResolvedMember<PhpMethodInfo>],
    entry_class: &str,
) -> Vec<BuilderMethod> {
    let mut out: Vec<BuilderMethod> = Vec::new();
    for member in methods {
        let m = &member.value;
        if m.visibility != PhpVisibility::Public {
            continue;
        }
        if m.name.starts_with("__") {
            continue;
        }
        // Already deduped by walk_class_chain (first wins). Belt-and-
        // suspenders here.
        if out.iter().any(|p| p.name == m.name) {
            continue;
        }
        // @internal-marked methods are framework-internal — skip.
        if m.docblock.as_deref().is_some_and(|d| d.contains("@internal")) {
            continue;
        }

        let doc_body = m.docblock.clone();
        let summary = doc_body.as_deref().and_then(first_non_tag_line);
        let return_type = extract_return_type_with_self_resolution(
            doc_body.as_deref(),
            m.return_type_raw.as_deref(),
            entry_class,
        );

        out.push(BuilderMethod {
            name: m.name.clone(),
            source_class: entry_class.to_string(),
            signature: m.raw_signature.clone(),
            return_type,
            summary,
            doc_body,
        });
    }
    out
}

// ---- Helpers (text manipulation, re-used across surfaces) --------------

/// Parse a PHP file's `use` aliases into a `local_name → FQCN` map.
/// Wraps the shared [`extract_use_aliases`] helper with a fallback to
/// an empty map when parsing fails.
fn parse_use_aliases(content: &str) -> HashMap<String, String> {
    let Ok(tree) = crate::parser::parse_php(content) else {
        return HashMap::new();
    };
    extract_use_aliases(&tree, content)
}

fn resolve_to_fqcn(
    name: &str,
    file_namespace: Option<&str>,
    aliases: &HashMap<String, String>,
) -> String {
    crate::laravel_introspector::model_metadata::resolve_to_fqcn(name, file_namespace, aliases)
}

fn lowercase_first_char(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn pascal_to_snake(s: &str) -> String {
    crate::laravel_introspector::model_metadata::pascal_to_snake(s)
}

fn camel_to_snake(s: &str) -> String {
    crate::laravel_introspector::model_metadata::pascal_to_snake(s)
}

fn first_non_tag_line(stripped_doc: &str) -> Option<String> {
    stripped_doc
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('@'))
        .map(str::to_string)
}

/// Rename `function {original}(` → `function {new_name}(` in a
/// signature string. No-op if the names are equal.
fn rename_method_in_signature(signature: &str, original: &str, new_name: &str) -> String {
    if original == new_name {
        return signature.to_string();
    }
    let needle = format!("function {}(", original);
    let replacement = format!("function {}(", new_name);
    signature.replacen(&needle, &replacement, 1)
}

/// Strip the first parameter (auto-supplied `Builder $query`) from a
/// scope method's signature.
fn signature_without_query_param(signature: &str) -> String {
    let Some(open) = signature.find('(') else {
        return signature.to_string();
    };
    let Some(close_rel) = signature[open + 1..].rfind(')') else {
        return signature.to_string();
    };
    let close = open + 1 + close_rel;
    let params = &signature[open + 1..close];
    if params.trim().is_empty() {
        return signature.to_string();
    }
    let new_params = strip_first_param(params).unwrap_or("");
    format!(
        "{}({}{}",
        &signature[..open],
        new_params,
        &signature[close..]
    )
}

fn strip_first_param(params: &str) -> Option<&str> {
    let bytes = params.as_bytes();
    let mut angle: i32 = 0;
    let mut paren: i32 = 0;
    let mut bracket: i32 = 0;
    let mut in_str: Option<u8> = None;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate() {
        if let Some(q) = in_str {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == q {
                in_str = None;
            }
            continue;
        }
        match b {
            b'\'' | b'"' => in_str = Some(b),
            b'<' => angle += 1,
            b'>' if angle > 0 => angle -= 1,
            b'(' => paren += 1,
            b')' if paren > 0 => paren -= 1,
            b'[' => bracket += 1,
            b']' if bracket > 0 => bracket -= 1,
            b',' if angle == 0 && paren == 0 && bracket == 0 => {
                return Some(params[i + 1..].trim_start());
            }
            _ => {}
        }
    }
    None
}

/// Extract return type from PHPDoc `@return TYPE` or signature `): TYPE`,
/// then resolve `$this`/`self`/`static` to `<entry_basename><static>`.
fn extract_return_type_with_self_resolution(
    doc_body: Option<&str>,
    php_return_type: Option<&str>,
    entry_class: &str,
) -> Option<String> {
    let raw = doc_body
        .and_then(return_type_from_phpdoc)
        .or_else(|| php_return_type.map(str::to_string))?;
    Some(crate::completion_format::resolve_self_type(&raw, entry_class))
}

fn return_type_from_phpdoc(doc_body: &str) -> Option<String> {
    let mut acc = String::new();
    let mut in_return = false;
    for raw in doc_body.lines() {
        let line = raw.trim();
        if line.starts_with("@return") {
            let after = line.trim_start_matches("@return").trim_start();
            if !after.is_empty() {
                acc.push_str(after);
            }
            in_return = true;
            continue;
        }
        if in_return {
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
    let type_only = acc.split_whitespace().next().unwrap_or("").to_string();
    if type_only.is_empty() {
        None
    } else {
        Some(type_only)
    }
}

fn unquote_string_literal(expr: &str) -> Option<String> {
    let trimmed = expr.trim();
    let bytes = trimmed.as_bytes();
    if bytes.len() < 2 {
        return None;
    }
    let first = bytes[0];
    let last = bytes[bytes.len() - 1];
    if (first == b'\'' || first == b'"') && first == last {
        Some(trimmed[1..trimmed.len() - 1].to_string())
    } else {
        None
    }
}

fn first_class_constant_arg(body: &str) -> Option<String> {
    let bytes = body.as_bytes();
    let mut in_str: Option<u8> = None;
    let mut escape = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = in_str {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' | b'"' => {
                in_str = Some(b);
                i += 1;
            }
            b':' if i + 6 < bytes.len() && &bytes[i..i + 7] == b"::class" => {
                let mut start = i;
                while start > 0 {
                    let c = bytes[start - 1];
                    if c.is_ascii_alphanumeric() || c == b'_' || c == b'\\' {
                        start -= 1;
                    } else {
                        break;
                    }
                }
                if start < i {
                    let raw = std::str::from_utf8(&bytes[start..i]).ok()?;
                    return Some(raw.rsplit('\\').next().unwrap_or(raw).to_string());
                }
                i += 7;
            }
            _ => i += 1,
        }
    }
    None
}

/// Parse a PHP array-literal expression (or text containing one) into
/// a `key → value` map. Both `'col' => 'type'` and
/// `'col' => Cast::class` entries are recognised.
///
/// Delegates to the existing tree-sitter-backed parser in
/// `model_analyzer` to avoid two implementations of "walk an
/// array_creation_expression."
fn parse_php_array_to_string_map(expr: &str) -> HashMap<String, String> {
    crate::laravel_introspector::model_metadata::parse_cast_array(expr)
}

#[cfg(test)]
mod tests;
