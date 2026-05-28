//! Model Analyzer for Eloquent Model Autocomplete
//!
//! Parses Laravel Eloquent model files to extract:
//! - Table name
//! - Casts (attribute type overrides)
//! - Accessors (computed properties)
//! - Relationships (belongsTo, hasMany, etc.)

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::php_outline::{extract_php_structure, PhpFileStructure, PhpStructureKind};

/// Source of a model property
#[derive(Debug, Clone, PartialEq)]
pub enum PropertySource {
    /// From database table column
    Database,
    /// From $casts property
    Cast,
    /// From accessor method (get*Attribute or Attribute return)
    Accessor,
    /// From relationship method
    Relationship,
}

/// A model property with type information
#[derive(Debug, Clone)]
pub struct ModelProperty {
    /// Property name (snake_case for database/casts, as-is for accessors)
    pub name: String,
    /// PHP type (int, string, Carbon, array, Model class, Collection, etc.)
    pub php_type: String,
    /// Where this property comes from
    pub source: PropertySource,
    /// Optional documentation/description
    pub documentation: Option<String>,
}

/// Accessor information extracted from model
#[derive(Debug, Clone)]
pub struct AccessorInfo {
    /// Property name (snake_case)
    pub property_name: String,
    /// Return type if specified
    pub return_type: Option<String>,
    /// Whether this is a new-style Attribute accessor
    pub is_attribute_style: bool,
}

/// Relationship information extracted from model
#[derive(Debug, Clone)]
pub struct RelationshipInfo {
    /// Method name (camelCase)
    pub method_name: String,
    /// Relationship type (hasMany, belongsTo, etc.)
    pub relationship_type: String,
    /// Related model class
    pub related_model: Option<String>,
}

/// Extracted metadata from a model file
#[derive(Debug, Clone, Default)]
pub struct ModelMetadata {
    /// Class name of the model
    pub class_name: String,
    /// Table name if specified ($table property)
    pub table_name: Option<String>,
    /// Casts: property_name -> cast_type
    pub casts: HashMap<String, String>,
    /// Accessors found in the model
    pub accessors: Vec<AccessorInfo>,
    /// Relationships found in the model
    pub relationships: Vec<RelationshipInfo>,
}

impl ModelMetadata {
    /// Parse a model file and extract metadata
    pub fn from_content(content: &str) -> Self {
        let class_name = Self::extract_class_name(content).unwrap_or_default();
        let table_name = Self::extract_table_name(content);
        let casts = Self::extract_casts(content);
        let accessors = Self::extract_accessors(content);
        let relationships = Self::extract_relationships(content);

        Self {
            class_name,
            table_name,
            casts,
            accessors,
            relationships,
        }
    }

    /// Parse a model file AND walk its `extends` chain, inheriting any
    /// `$table`, casts, accessors, and relationships the child doesn't
    /// declare itself.
    ///
    /// Real Laravel codebases often factor shared shape into a base
    /// class — e.g. `OAuthAccessToken extends Token` where `Token`
    /// declares `protected $table = 'oauth_access_tokens'`. Without
    /// inheritance walking, the child's `ModelMetadata` has
    /// `table_name = None` and we fall back to snake-pluralizing
    /// "OAuthAccessToken" → "o_auth_access_tokens" (wrong) instead of
    /// using the parent's explicit "oauth_access_tokens".
    ///
    /// PHP method/property resolution: child wins on conflict, parent
    /// fills in what the child doesn't declare. We mirror that:
    /// - `table_name`: child overrides; parent fills if child has none.
    /// - `casts`: union; child entries take precedence per-key.
    /// - `accessors` / `relationships`: appended from parent if not
    ///   already declared by the same name on the child.
    ///
    /// Stops at Laravel's base `Model`, at any parent class file we
    /// can't locate in the project (vendor classes without app/-side
    /// shadowing aren't searched here yet), or at a 10-deep recursion
    /// cap. Cycle-safe via a visited-set.
    pub fn from_file_with_inheritance(path: &Path, project_root: &Path) -> Option<Self> {
        let mut visited = HashSet::new();
        Self::resolve_recursive(path, project_root, &mut visited, 0)
    }

    /// Walk the `extends` chain from `path` and return `true` if any
    /// ancestor is Eloquent's base `Model`. Used to decide whether to
    /// surface Eloquent-style property completions (DB columns + casts +
    /// accessors + relationships) versus generic public-property scans.
    ///
    /// The chain may pass through any number of intermediate base classes
    /// — `OAuthAccessToken extends Token extends BaseModel extends Model`
    /// counts as Eloquent. A literal `extends Model` regex misses that
    /// shape entirely.
    ///
    /// Bounded by the same 10-deep recursion cap as
    /// [`from_file_with_inheritance`] and cycle-safe via a visited set.
    /// Uses [`crate::class_locator::find_php_class_file_in_app_or_vendor`]
    /// so vendor-side base classes (e.g. an SDK's `BaseModel`) are found
    /// through Composer's autoload data.
    pub fn extends_eloquent_model(path: &Path, project_root: &Path) -> bool {
        let mut visited: HashSet<PathBuf> = HashSet::new();
        let mut current = Some(path.to_path_buf());
        let mut depth = 0usize;
        while let Some(p) = current.take() {
            if depth > 10 {
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
            let Some(parent_raw) = Self::extract_parent_class(&content) else {
                return false;
            };
            // Resolve the raw parent name through the file's use aliases
            // FIRST, then check the basename. The raw name can be an alias
            // (`use Illuminate\Database\Eloquent\Model as EloquentModel;`),
            // in which case the basename of `parent_raw` is "EloquentModel"
            // but the resolved FQCN ends in "Model" — that's what we want
            // to match.
            let aliases = Self::extract_use_aliases_from_php(&content);
            let ns = Self::extract_namespace(&content);
            let parent_fqcn = Self::resolve_to_fqcn(&parent_raw, ns.as_deref(), &aliases);
            let resolved_basename = parent_fqcn.rsplit('\\').next().unwrap_or(&parent_fqcn);
            if resolved_basename == "Model" {
                // Confirmed an Eloquent ancestor. Stop walking — same
                // sentinel `from_file_with_inheritance` uses.
                return true;
            }
            current = crate::class_locator::find_php_class_file_in_app_or_vendor(
                &parent_fqcn,
                project_root,
            );
        }
        false
    }

    fn resolve_recursive(
        path: &Path,
        project_root: &Path,
        visited: &mut HashSet<PathBuf>,
        depth: usize,
    ) -> Option<Self> {
        if depth > 10 {
            return None;
        }
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if !visited.insert(canonical) {
            // Cycle — A extends B extends A. Shouldn't happen in valid
            // PHP, but parsing is best-effort and we'd rather bail than
            // recurse forever.
            return None;
        }

        let content = std::fs::read_to_string(path).ok()?;
        let mut metadata = Self::from_content(&content);

        if let Some(parent_raw) = Self::extract_parent_class(&content) {
            // Stop at Eloquent's base class. Real Laravel models extend
            // `Model` directly OR via `Authenticatable` (which itself
            // extends Model). We only halt on Model — Authenticatable
            // and Pivot etc. are worth walking into (they may declare
            // their own $table fallbacks for specific use cases).
            let basename = parent_raw.rsplit('\\').next().unwrap_or(&parent_raw);
            if basename != "Model" {
                // Resolve the parent through the child file's use
                // statements + namespace so we get the actual FQCN
                // (e.g. `Token` → `Laravel\Passport\Token`). Then map
                // FQCN → file path via the Laravel/Composer
                // convention. Falls back to a basename search if the
                // FQCN path doesn't exist on disk — covers projects
                // with non-standard PSR-4 layouts.
                let use_aliases = Self::extract_use_aliases_from_php(&content);
                let file_namespace = Self::extract_namespace(&content);
                let parent_fqcn =
                    Self::resolve_to_fqcn(&parent_raw, file_namespace.as_deref(), &use_aliases);

                // Single call covers both PSR-4 paths (App\ and vendor/)
                // and falls back to basename walk if neither shape exists.
                // The PSR-4 ordering inside `find_php_class_file_in_app_or_vendor`
                // means project-local classes shadow vendor classes.
                let parent_path = crate::class_locator::find_php_class_file_in_app_or_vendor(
                    &parent_fqcn,
                    project_root,
                );

                if let Some(parent_path) = parent_path {
                    if let Some(parent_meta) =
                        Self::resolve_recursive(&parent_path, project_root, visited, depth + 1)
                    {
                        metadata.merge_inherited(parent_meta);
                    }
                }
                // No parent file found: built-in PHP class, missing
                // dependency, or unconventional autoload. Walking stops.
            }
        }

        Some(metadata)
    }

    /// Extract `use Foo\Bar;` / `use Foo\Bar as Baz;` statements from
    /// PHP source. Returns a `local_name → FQCN` map.
    ///
    /// Backed by the AST-based [`crate::query_chain::extract_use_aliases`]
    /// helper — same walker the chain extractor uses. Handles flat,
    /// grouped, and aliased forms.
    pub fn extract_use_aliases_from_php(content: &str) -> HashMap<String, String> {
        let Ok(tree) = crate::parser::parse_php(content) else {
            return HashMap::new();
        };
        crate::query_chain::extract_use_aliases(&tree, content)
    }

    /// Extract the `namespace Foo\Bar;` declaration. Returns the
    /// namespace without the leading backslash. Returns `None` for
    /// files without a namespace declaration (global namespace).
    ///
    /// Sourced from the shared structure walker.
    pub fn extract_namespace(content: &str) -> Option<String> {
        Self::parse_structure(content).namespace
    }

    /// Resolve a class reference (as it appears in `extends`,
    /// `implements`, etc.) to its fully-qualified class name.
    ///
    /// Resolution rules mirror PHP:
    /// - `\Foo\Bar` (leading backslash) is already fully qualified —
    ///   strip the backslash.
    /// - `Foo\Bar` (no leading backslash, contains backslash) is
    ///   partially qualified — first segment may be an aliased use,
    ///   else prepend the file's namespace.
    /// - `Bar` (unqualified) — look in use aliases first, else
    ///   prepend the file's namespace.
    pub fn resolve_to_fqcn(
        name: &str,
        file_namespace: Option<&str>,
        use_aliases: &HashMap<String, String>,
    ) -> String {
        if let Some(stripped) = name.strip_prefix('\\') {
            return stripped.to_string();
        }
        if name.contains('\\') {
            let first = name.split('\\').next().unwrap_or("");
            if let Some(prefix) = use_aliases.get(first) {
                let rest = &name[first.len()..]; // includes leading `\`
                return format!("{prefix}{rest}");
            }
            if let Some(ns) = file_namespace {
                return format!("{ns}\\{name}");
            }
            return name.to_string();
        }
        if let Some(fqcn) = use_aliases.get(name) {
            return fqcn.clone();
        }
        if let Some(ns) = file_namespace {
            return format!("{ns}\\{name}");
        }
        name.to_string()
    }

    // `find_class_file_by_fqcn` lived here until Phase 5.9, then moved
    // into class_locator so non-inheritance callers (columns_for_builder,
    // relations, resolve_related_model) also benefit. The inheritance
    // walker now calls `class_locator::find_php_class_file_in_app_or_vendor`
    // directly — it already chains the FQCN-aware lookup with the
    // basename-walk fallback.

    /// Extract the parent class name from `class X extends Y`. Returns
    /// the parent as written (may be a simple name like `Token` or a
    /// qualified name like `\App\Models\Token`); the caller resolves
    /// it to a file via `class_locator::find_php_class_file`, which
    /// matches by basename.
    fn extract_parent_class(content: &str) -> Option<String> {
        Self::first_class(&Self::parse_structure(content))?
            .extends_raw
            .clone()
    }

    /// Find the first `class` (not interface / trait / enum) structure in
    /// a parsed file. Models are always classes; everything else is
    /// ignored. Returns `None` for files that don't declare a class.
    fn first_class(structure: &PhpFileStructure) -> Option<&crate::php_outline::PhpStructure> {
        structure
            .structures
            .iter()
            .find(|s| s.kind == PhpStructureKind::Class)
    }

    /// Wrapper over [`extract_php_structure`] that tolerates content
    /// without a leading `<?php` tag. Tree-sitter-php treats everything
    /// outside `<?php` tags as HTML text, so a snippet like
    /// `class Foo extends Bar { … }` parses as text and yields zero
    /// structures. Real model files always start with `<?php` so this
    /// only matters for tests and ad-hoc analysis of code fragments.
    fn parse_structure(content: &str) -> PhpFileStructure {
        if content.trim_start().starts_with("<?php") {
            extract_php_structure(content)
        } else {
            // Cheap: prepend the tag and parse the augmented string.
            let mut prefixed = String::with_capacity(content.len() + 6);
            prefixed.push_str("<?php\n");
            prefixed.push_str(content);
            extract_php_structure(&prefixed)
        }
    }

    /// PHP-style inheritance merge: child fields win, parent fills the
    /// gaps. Called only after the parent's full chain is already
    /// resolved (so grandparent fields are already merged into
    /// `parent`).
    fn merge_inherited(&mut self, parent: ModelMetadata) {
        if self.table_name.is_none() {
            self.table_name = parent.table_name;
        }
        for (k, v) in parent.casts {
            self.casts.entry(k).or_insert(v);
        }
        for acc in parent.accessors {
            if !self
                .accessors
                .iter()
                .any(|a| a.property_name == acc.property_name)
            {
                self.accessors.push(acc);
            }
        }
        for rel in parent.relationships {
            if !self
                .relationships
                .iter()
                .any(|r| r.method_name == rel.method_name)
            {
                self.relationships.push(rel);
            }
        }
        // `class_name` always stays the child's — Laravel's
        // `static::class` semantics inside model methods resolves to
        // the concrete class, not its parents.
    }

    /// Extract the class name from the model file. Returns the name of
    /// the first `class` declaration that has an `extends` clause —
    /// model files always extend something (`Model`, a base class, etc.),
    /// and skipping un-extended classes filters out incidental helper
    /// classes that shouldn't be treated as models.
    fn extract_class_name(content: &str) -> Option<String> {
        let structure = Self::parse_structure(content);
        structure
            .structures
            .iter()
            .find(|s| s.kind == PhpStructureKind::Class && s.extends.is_some())
            .map(|s| s.name.clone())
    }

    /// Extract `$table = 'table_name'` from the model. Looks for a
    /// property literally named `table` whose default value is a quoted
    /// string. Visibility doesn't matter (Laravel uses `protected` but
    /// `public`/`private` parses the same).
    fn extract_table_name(content: &str) -> Option<String> {
        let structure = Self::parse_structure(content);
        let class = Self::first_class(&structure)?;
        let prop = class.properties.iter().find(|p| p.name == "table")?;
        let default = prop.default_value.as_deref()?;
        unquote_string_literal(default)
    }

    /// Extract casts from $casts property or casts() method
    fn extract_casts(content: &str) -> HashMap<String, String> {
        let mut casts = HashMap::new();

        // Match $casts = [...] property
        if let Some(property_casts) = Self::extract_casts_property(content) {
            casts.extend(property_casts);
        }

        // Match casts(): array method (Laravel 11+ style)
        if let Some(method_casts) = Self::extract_casts_method(content) {
            casts.extend(method_casts);
        }

        casts
    }

    /// Extract casts from `$casts = [...]` property.
    ///
    /// Reads the `casts` property's default-value expression from the
    /// shared structure walker, then text-parses the inner array (PHP
    /// array literals are well-bounded; `parse_cast_array` handles both
    /// `'col' => 'json'` and `'col' => Cast::class` entries).
    fn extract_casts_property(content: &str) -> Option<HashMap<String, String>> {
        let structure = Self::parse_structure(content);
        let class = Self::first_class(&structure)?;
        let prop = class.properties.iter().find(|p| p.name == "casts")?;
        let default = prop.default_value.as_deref()?;
        let inner = array_literal_inner(default)?;
        Some(Self::parse_cast_array(inner))
    }

    /// Extract casts from `casts(): array` method (Laravel 11+ style).
    ///
    /// Reads the body of the `casts` method from the shared structure
    /// walker, then text-parses the `return [ ... ]` payload.
    fn extract_casts_method(content: &str) -> Option<HashMap<String, String>> {
        let structure = Self::parse_structure(content);
        let class = Self::first_class(&structure)?;
        let method = class
            .methods
            .iter()
            .find(|m| m.name == "casts" && m.parameters.is_empty())?;
        let body = method.body_source.as_deref()?;
        // Body looks like `{ return [ … ]; }` — find the array literal
        // following `return`.
        let return_idx = body.find("return")?;
        let after_return = &body[return_idx + "return".len()..];
        let inner = array_literal_inner(after_return)?;
        Some(Self::parse_cast_array(inner))
    }

    /// Parse a PHP array of casts. `content` is the **inner text** of an
    /// array literal — what comes between the outermost `[` and `]`. The
    /// supported entry shapes are:
    ///
    /// - `'key' => 'value'` — string-keyed string cast (`'json'`, `'array'`, …)
    /// - `'key' => SomeClass::class` — class-constant cast (`AsCollection::class`, …)
    /// - `'key' => \Fully\Qualified\Class::class` — same, FQCN form
    ///
    /// Implementation parses the content through tree-sitter (wrapped in
    /// `<?php $x = [content];` so it's a valid program) and walks the
    /// resulting `array_creation_expression`. This is robust against
    /// nested arrays, escaped quotes, and comments mid-entry — all of
    /// which broke the previous regex approach.
    fn parse_cast_array(content: &str) -> HashMap<String, String> {
        let wrapped = format!("<?php $x = [{}];", content);
        let Ok(tree) = crate::parser::parse_php(&wrapped) else {
            return HashMap::new();
        };
        let bytes = wrapped.as_bytes();
        let mut casts = HashMap::new();
        walk_for_first_array(tree.root_node(), bytes, &mut casts);
        casts
    }

    /// Extract accessor methods from the model.
    ///
    /// Two flavours, both surfaced via the shared structure walker:
    /// - **Old style**: `getFirstNameAttribute(): string` — magic method
    ///   pattern; property name comes from stripping `get`/`Attribute`
    ///   and snake_casing the middle.
    /// - **New style**: `firstName(): Attribute` — Laravel 9+; any method
    ///   whose PHP return type is `Attribute` becomes a property whose
    ///   name is the method's camelCase name snake_cased.
    fn extract_accessors(content: &str) -> Vec<AccessorInfo> {
        let mut accessors = Vec::new();
        let structure = Self::parse_structure(content);
        let Some(class) = Self::first_class(&structure) else {
            return accessors;
        };

        for m in &class.methods {
            // Old style: `getXxxAttribute`
            if let Some(middle) = m
                .name
                .strip_prefix("get")
                .and_then(|s| s.strip_suffix("Attribute"))
            {
                if !middle.is_empty() {
                    let property_name = Self::pascal_to_snake(middle);
                    let return_type = m.return_type_raw.clone();
                    accessors.push(AccessorInfo {
                        property_name,
                        return_type,
                        is_attribute_style: false,
                    });
                    continue;
                }
            }
            // New style: any method returning `Attribute` (possibly
            // namespaced, e.g. `\Illuminate\Database\Eloquent\Casts\Attribute`).
            // Check the raw return type's final segment.
            let returns_attribute = m
                .return_type_raw
                .as_deref()
                .map(|t| t.trim_start_matches('?').rsplit('\\').next().unwrap_or("") == "Attribute")
                .unwrap_or(false);
            if returns_attribute {
                let property_name = Self::camel_to_snake(&m.name);
                accessors.push(AccessorInfo {
                    property_name,
                    return_type: None, // type lives inside the Attribute::make() call
                    is_attribute_style: true,
                });
            }
        }

        accessors
    }

    /// Extract relationship methods from the model.
    ///
    /// Iterates the class's methods (via the shared structure walker)
    /// and identifies relationship methods two ways:
    ///
    /// - **By PHP return type**: `function posts(): HasMany` — the
    ///   `return_type_raw` field's basename matches a known relationship
    ///   kind (`HasMany`, `BelongsTo`, …; case-insensitive).
    /// - **By body**: `function posts() { return $this->hasMany(Post::class); }`
    ///   — the method body's `return $this->RELATIONSHIP(...)` call
    ///   names one of the relationship kinds.
    ///
    /// Either way, the first `SomeModel::class` argument inside the body
    /// becomes the related model (resolved to FQCN via the file's
    /// namespace + use aliases).
    fn extract_relationships(content: &str) -> Vec<RelationshipInfo> {
        let mut relationships: Vec<RelationshipInfo> = Vec::new();
        let structure = Self::parse_structure(content);
        let Some(class) = Self::first_class(&structure) else {
            return relationships;
        };

        // Resolve `Post::class` references through THIS file's namespace +
        // use statements once, then reuse for every relationship. If we
        // stored just the basename (`"Post"`), a later dotted-path hop
        // would basename-walk and could land on the wrong file (e.g.
        // `app/Nova/Filters/Post.php` instead of the actual model).
        let file_namespace = Self::extract_namespace(content);
        let use_aliases = Self::extract_use_aliases_from_php(content);
        let resolve_class = |bare: String| -> String {
            Self::resolve_to_fqcn(&bare, file_namespace.as_deref(), &use_aliases)
        };

        // Known Eloquent relationship-builder method names. Recognised in
        // any case (so `HasMany` as a return type matches `hasMany` as a
        // method call). Longest-first to disambiguate `belongsToMany`
        // from `belongsTo` when matching in body text.
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

        for method in &class.methods {
            // Determine the relationship kind, if any.
            let mut kind: Option<&'static str> = None;

            // Strategy 1: PHP return-type declaration.
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

            // Strategy 2: body `return $this->KIND(...)` pattern.
            if kind.is_none() {
                if let Some(body) = method.body_source.as_deref() {
                    kind = detect_relationship_kind_in_body(body, RELATIONSHIP_KINDS);
                }
            }

            let Some(rel_type) = kind else {
                continue;
            };

            // Dedup by method name (won't happen in valid PHP but be safe).
            if relationships
                .iter()
                .any(|r| r.method_name == method.name)
            {
                continue;
            }

            // Pull the first `SomeModel::class` argument out of the body.
            let related_model = method
                .body_source
                .as_deref()
                .and_then(first_class_constant_arg)
                .map(&resolve_class);

            relationships.push(RelationshipInfo {
                method_name: method.name.clone(),
                relationship_type: rel_type.to_string(),
                related_model,
            });
        }

        relationships
    }

    /// Convert PascalCase to snake_case
    /// e.g., FirstName -> first_name
    pub fn pascal_to_snake(s: &str) -> String {
        let mut result = String::new();
        for (i, c) in s.chars().enumerate() {
            if c.is_uppercase() {
                if i > 0 {
                    result.push('_');
                }
                result.push(c.to_lowercase().next().unwrap_or(c));
            } else {
                result.push(c);
            }
        }
        result
    }

    /// Convert camelCase to snake_case
    /// e.g., firstName -> first_name
    fn camel_to_snake(s: &str) -> String {
        Self::pascal_to_snake(s)
    }
}

/// Map cast type to PHP type
pub fn map_cast_to_php_type(cast: &str) -> String {
    let cast_lower = cast.to_lowercase();

    match cast_lower.as_str() {
        // Date/time casts -> Carbon
        "datetime" | "date" | "timestamp" | "immutable_datetime" | "immutable_date" => {
            "Carbon".to_string()
        }
        // Array/collection casts
        "array" | "json" | "collection" | "object" => "array".to_string(),
        // Boolean casts
        "boolean" | "bool" => "bool".to_string(),
        // Integer casts
        "integer" | "int" => "int".to_string(),
        // Float casts
        "float" | "double" | "decimal" | "real" => "float".to_string(),
        // String cast
        "string" => "string".to_string(),
        // Encrypted casts (return the base type)
        "encrypted" => "string".to_string(),
        "encrypted:array" | "encrypted:collection" | "encrypted:object" => "array".to_string(),
        // AsStringable
        "asstringable" => "Stringable".to_string(),
        // AsArrayObject / AsCollection
        "asarrayobject" => "ArrayObject".to_string(),
        "ascollection" => "Collection".to_string(),
        // Custom cast class - return the class name
        _ => cast.to_string(),
    }
}

/// Get the PHP type for a relationship.
///
/// `related_model` may be a FQCN (`App\Models\Post`) or a bare class
/// name (`Post`). The displayed label always uses the simple name —
/// completion details should be short, and the model's namespace
/// rarely adds value at a glance.
pub fn relationship_to_php_type(rel_type: &str, related_model: Option<&str>) -> String {
    let model_full = related_model.unwrap_or("Model");
    let model = model_full.rsplit('\\').next().unwrap_or(model_full);

    match rel_type {
        // Single model relationships
        "hasOne" | "belongsTo" | "morphOne" | "morphTo" | "hasOneThrough" => {
            format!("?{}", model)
        }
        // Collection relationships
        "hasMany" | "belongsToMany" | "morphMany" | "morphToMany" | "morphedByMany"
        | "hasManyThrough" => {
            format!("Collection<{}>", model)
        }
        _ => "mixed".to_string(),
    }
}

/// Walk a tree-sitter PHP tree looking for the first `array_creation_expression`
/// and collect its key/value entries into `out`. Used by `parse_cast_array`
/// after wrapping the source in `<?php $x = [...];` to get a parseable program.
fn walk_for_first_array(
    node: tree_sitter::Node,
    bytes: &[u8],
    out: &mut HashMap<String, String>,
) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "array_creation_expression" {
            collect_cast_entries(child, bytes, out);
            return true;
        }
        if walk_for_first_array(child, bytes, out) {
            return true;
        }
    }
    false
}

/// For each `array_element_initializer` (key/value pair) child of an
/// `array_creation_expression`, extract a string-literal key and a
/// string-literal or class-constant value. Skips non-conforming entries
/// silently — Laravel `$casts` arrays are well-shaped in practice and
/// any unexpected entry just won't surface as a cast.
fn collect_cast_entries(arr: tree_sitter::Node, bytes: &[u8], out: &mut HashMap<String, String>) {
    let mut cursor = arr.walk();
    for child in arr.children(&mut cursor) {
        if child.kind() != "array_element_initializer" {
            continue;
        }
        // Children of array_element_initializer are either [value] (for
        // un-keyed entries) or [key, =>, value]. We collect the
        // expression-like children in order; if we get exactly two, treat
        // them as key + value.
        let mut ec = child.walk();
        let exprs: Vec<tree_sitter::Node> = child
            .children(&mut ec)
            .filter(|c| !matches!(c.kind(), "=>" | "comment"))
            .collect();
        let (key_node, value_node) = match exprs.as_slice() {
            [k, v] => (*k, *v),
            _ => continue,
        };
        let Some(key) = string_literal_text(key_node, bytes) else {
            continue;
        };
        let Some(value) = string_literal_text(value_node, bytes)
            .or_else(|| class_constant_basename(value_node, bytes))
        else {
            continue;
        };
        out.insert(key, value);
    }
}

/// If `node` is a PHP string literal (`'foo'` / `"foo"`), return its
/// content without the quotes. Returns `None` for non-string nodes.
fn string_literal_text(node: tree_sitter::Node, bytes: &[u8]) -> Option<String> {
    let kind = node.kind();
    if kind != "string" && kind != "encapsed_string" {
        return None;
    }
    let text = std::str::from_utf8(&bytes[node.start_byte()..node.end_byte()]).ok()?;
    unquote_string_literal(text)
}

/// If `node` is a `SomeClass::class` expression, return the class's
/// basename (last `\`-segment). Returns `None` for other expressions.
fn class_constant_basename(node: tree_sitter::Node, bytes: &[u8]) -> Option<String> {
    if node.kind() != "class_constant_access_expression" {
        return None;
    }
    let text = std::str::from_utf8(&bytes[node.start_byte()..node.end_byte()]).ok()?;
    let raw = text.strip_suffix("::class")?.trim();
    Some(raw.rsplit('\\').next().unwrap_or(raw).to_string())
}

/// Scan a method body's source for `return $this->KIND(...)` where KIND
/// is one of the recognised relationship builder names. Returns the
/// matching kind (the literal name from the list, preserving its
/// original casing) on first hit. Longest-first ordering in the list
/// prevents `belongsTo` from matching a `belongsToMany` call.
///
/// The matching is loose by design — it doesn't require a strict
/// `return $this->` prefix because Laravel devs sometimes write
/// `return $this->hasMany(Post::class)->where(…)` or
/// `return tap($this->hasMany(...), ...)`. We just need to know "this
/// method builds a relationship of kind X."
fn detect_relationship_kind_in_body(
    body: &str,
    kinds: &[&'static str],
) -> Option<&'static str> {
    for kind in kinds {
        // `$this->kind(` — boundary-checked via the trailing `(` so
        // `hasMany` doesn't match `hasManyThrough`.
        let needle = format!("$this->{}(", kind);
        if body.contains(&needle) {
            return Some(*kind);
        }
    }
    None
}

/// Find the first `SomeName::class` constant expression in `body` and
/// return `SomeName`. Skips occurrences inside string literals. Used to
/// pull the related-model class out of a relationship method body like
/// `$this->hasMany(Post::class)`.
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
                // Walk backward from `i` over `[A-Za-z0-9_\\]` to find the
                // class name.
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
                    // The relationship API takes the BASENAME (e.g.
                    // `Post::class` resolved via use statements), not
                    // the fully-qualified form. Strip any namespace
                    // prefix so the caller's FQCN resolver works.
                    let bare = raw.rsplit('\\').next().unwrap_or(raw).to_string();
                    return Some(bare);
                }
                i += 7;
            }
            _ => i += 1,
        }
    }
    None
}

/// Find the first PHP array literal in `expr` and return its inner
/// contents (everything between the outermost matching `[` and `]`).
/// Handles nested brackets via depth counting; ignores brackets inside
/// single- or double-quoted strings.
///
/// Returns `None` if `expr` doesn't contain a `[…]` pair. Both
/// long-array (`array(…)`) syntax and trailing-comma cases are out of
/// scope — Laravel's `$casts` / `casts()` always use `[…]`.
fn array_literal_inner(expr: &str) -> Option<&str> {
    let bytes = expr.as_bytes();
    let mut depth: i32 = 0;
    let mut in_str: Option<u8> = None;
    let mut escape = false;
    let mut start: Option<usize> = None;
    for (i, &b) in bytes.iter().enumerate() {
        if let Some(q) = in_str {
            if escape {
                escape = false;
                continue;
            }
            if b == b'\\' {
                escape = true;
                continue;
            }
            if b == q {
                in_str = None;
            }
            continue;
        }
        match b {
            b'\'' | b'"' => in_str = Some(b),
            b'[' => {
                if depth == 0 {
                    start = Some(i + 1);
                }
                depth += 1;
            }
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&expr[start?..i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Strip surrounding single or double quotes from a string-literal
/// expression as it appears in PHP source. Returns `None` for non-string
/// expressions (`null`, `[]`, method calls, etc.) — the caller decides
/// what to do with those.
///
/// Tolerant of whitespace around the literal: `"  'foo'  "` → `Some("foo")`.
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

#[cfg(test)]
mod tests;
