//! Model Analyzer for Eloquent Model Autocomplete
//!
//! Parses Laravel Eloquent model files to extract:
//! - Table name
//! - Casts (attribute type overrides)
//! - Accessors (computed properties)
//! - Relationships (belongsTo, hasMany, etc.)

use std::collections::HashMap;
use std::path::Path;

use crate::laravel_introspector::walker::extract_php_structure;

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
    /// Parse a model from in-memory PHP source and extract its metadata.
    ///
    /// Single-file analysis — no inheritance walking (we have no file
    /// path to read parents from). For full inheritance resolution, use
    /// [`Self::from_file_with_inheritance`].
    ///
    /// Delegates to the unified
    /// [`crate::laravel_introspector::chain::analyze_content`]
    /// walker and maps its [`ClassView`] surfaces onto this
    /// type. The mapping is straightforward — accessor/relationship/
    /// cast shapes are identical.
    pub fn from_content(content: &str) -> Self {
        match crate::laravel_introspector::chain::analyze_content(content) {
            Some(view) => Self::from_view(&view),
            None => Self::default(),
        }
    }

    /// Map a [`ClassView`] onto a [`ModelMetadata`]. The two
    /// types are shape-equivalent for the Model surfaces; this is a
    /// pure conversion. Lives here (not on the view) because
    /// `ModelMetadata` is the public Model API used across the LSP.
    fn from_view(view: &crate::laravel_introspector::chain::ClassView) -> Self {
        Self {
            class_name: view.class_name.clone(),
            table_name: view.table_name.clone(),
            casts: view.casts.clone(),
            accessors: view
                .accessors
                .iter()
                .map(|a| AccessorInfo {
                    property_name: a.property_name.clone(),
                    return_type: a.return_type.clone(),
                    is_attribute_style: a.is_attribute_style,
                })
                .collect(),
            relationships: view
                .relationships
                .iter()
                .map(|r| RelationshipInfo {
                    method_name: r.method_name.clone(),
                    relationship_type: r.relationship_type.clone(),
                    related_model: r.related_model.clone(),
                })
                .collect(),
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
        // Delegates to the unified Laravel-aware walker, which already
        // resolves inheritance + trait composition with first-wins
        // precedence. The view's `casts` / `accessors` / `relationships` /
        // `table_name` are merged across the chain — exactly what the
        // previous `merge_inherited` produced manually.
        let view = crate::laravel_introspector::chain::analyze(path, project_root)?;
        Some(Self::from_view(&view))
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
        // Delegates to the unified Laravel-aware walker — same
        // implementation, single source of truth for "is this file an
        // Eloquent model?"
        crate::laravel_introspector::chain::extends_eloquent_model(path, project_root)
    }

    // resolve_recursive: removed. `from_file_with_inheritance` now
    // delegates straight to `laravel_class::analyze`,
    // which performs the same `extends` + trait walk with first-wins
    // precedence and cycle protection.

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
    /// Sourced from the shared structure walker — auto-prepends a
    /// `<?php` tag when the content omits it (tests pass fragments).
    pub fn extract_namespace(content: &str) -> Option<String> {
        let owned;
        let input: &str = if content.trim_start().starts_with("<?php") {
            content
        } else {
            owned = format!("<?php\n{}", content);
            &owned
        };
        extract_php_structure(input).namespace
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

    // extract_parent_class / first_class / parse_structure: removed.
    // The only remaining caller (`extends_eloquent_model`) now lives
    // in `laravel_class`.

    // merge_inherited / extract_class_name / extract_table_name /
    // extract_casts / extract_casts_property / extract_casts_method /
    // extract_accessors / extract_relationships: removed. The unified
    // `crate::laravel_introspector::chain::analyze[_from_content]`
    // walker computes all of these surfaces from one pass over the
    // file structure. `from_view` maps them back onto this type.

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

    // camel_to_snake: removed (was an alias for pascal_to_snake, only
    // used internally by the now-deleted accessor extractor).
}

/// Parse a PHP `[ 'key' => 'value', 'k2' => Class::class ]` array
/// expression into a `String → String` map. Used by the unified
/// [`crate::laravel_introspector`] walker for `$casts` extraction.
///
/// Accepts either form:
/// - Inner text (`"'col' => 'json', 'meta' => 'array'"`)
/// - Full expression (`"['col' => 'json', ...]"`)
///
/// Wraps the content in `<?php $x = [content];` and walks the
/// resulting `array_creation_expression` via tree-sitter — robust
/// against escaped quotes, nested arrays, comments mid-entry.
// Free-function aliases for the module's text utilities — exposed at
// `laravel_introspector::*` for callers who don't want to type
// `ModelMetadata::pascal_to_snake(...)` etc. Each delegates to the
// associated function on `ModelMetadata` so there's exactly one
// implementation.

/// Free-function shim for [`ModelMetadata::pascal_to_snake`].
pub fn pascal_to_snake(s: &str) -> String {
    ModelMetadata::pascal_to_snake(s)
}

/// Free-function shim for [`ModelMetadata::resolve_to_fqcn`].
pub fn resolve_to_fqcn(
    name: &str,
    file_namespace: Option<&str>,
    use_aliases: &HashMap<String, String>,
) -> String {
    ModelMetadata::resolve_to_fqcn(name, file_namespace, use_aliases)
}

/// Free-function shim for [`ModelMetadata::extract_namespace`].
pub fn extract_namespace(content: &str) -> Option<String> {
    ModelMetadata::extract_namespace(content)
}

/// Free-function shim for [`ModelMetadata::extract_use_aliases_from_php`].
pub fn extract_use_aliases(content: &str) -> HashMap<String, String> {
    ModelMetadata::extract_use_aliases_from_php(content)
}

/// Free-function alias for the cast-array parser, exposed at the module
/// root as `parse_cast_array`. Implementation below.
pub fn parse_cast_array(expr: &str) -> HashMap<String, String> {
    parse_cast_array_public(expr)
}

pub fn parse_cast_array_public(expr: &str) -> std::collections::HashMap<String, String> {
    let inner = array_literal_inner(expr).unwrap_or(expr);
    let wrapped = format!("<?php $x = [{}];", inner);
    let Ok(tree) = crate::parser::parse_php(&wrapped) else {
        return HashMap::new();
    };
    let bytes = wrapped.as_bytes();
    let mut casts = HashMap::new();
    walk_for_first_array(tree.root_node(), bytes, &mut casts);
    casts
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
// detect_relationship_kind_in_body / first_class_constant_arg:
// removed. Both were used by the now-deleted `extract_relationships`;
// the equivalents live inside `laravel_class` as private helpers for
// the unified relationship surface.

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
