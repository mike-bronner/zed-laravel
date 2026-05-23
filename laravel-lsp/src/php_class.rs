//! Parsing helpers for PHP class files (Livewire components, View components, models).
//!
//! These functions extract property types, method return types, and decompose
//! generic type expressions out of PHP source. They're defined in the library
//! crate so Salsa-tracked queries can use them.

/// Simplify a fully qualified class name to just the class name.
/// `\Illuminate\Pagination\LengthAwarePaginator` -> `LengthAwarePaginator`
pub fn simplify_type(type_name: &str) -> String {
    type_name
        .rsplit('\\')
        .next()
        .unwrap_or(type_name)
        .to_string()
}

/// Find a class property's type from class definition.
/// Handles: `protected User $user;`, `private ?Model $model;`, `public $items;` with PHPDoc,
/// constructor property promotion, and `$this->prop = new Foo()` / `Model::find()` initialization.
pub fn find_property_type_in_content(content: &str, property_name: &str) -> Option<String> {
    let escaped_prop = regex::escape(property_name);

    let primitive_types = r"int|string|bool|boolean|float|double|array|object|mixed|null|void|callable|iterable|never|true|false";

    // 1. PHPDoc @var annotation above property
    let phpdoc_pattern = format!(
        r#"@var\s+\\?([A-Za-z][a-zA-Z0-9_\\|<>]*)\s*\*/\s*(?:public|protected|private)\s+(?:\?)?(?:[A-Za-z][a-zA-Z0-9_\\]*)?\s*\${}"#,
        escaped_prop
    );
    if let Some(caps) = regex::Regex::new(&phpdoc_pattern)
        .ok()
        .and_then(|re| re.captures(content))
    {
        if let Some(type_match) = caps.get(1) {
            return Some(simplify_type(type_match.as_str()));
        }
    }

    // 2a. Typed property with primitive type: protected int $count;
    let primitive_prop_pattern = format!(
        r#"(?:public|protected|private)\s+(\?)?({})(?:\s+|\s*\|\s*[a-zA-Z]+\s+)\${}\s*[;=]"#,
        primitive_types, escaped_prop
    );
    if let Some(caps) = regex::Regex::new(&primitive_prop_pattern)
        .ok()
        .and_then(|re| re.captures(content))
    {
        if let Some(type_match) = caps.get(2) {
            let nullable = caps.get(1).is_some();
            let type_name = type_match.as_str().to_string();
            return Some(if nullable {
                format!("?{}", type_name)
            } else {
                type_name
            });
        }
    }

    // 2b. Typed property with class type: protected User $user;
    let typed_prop_pattern = format!(
        r#"(?:public|protected|private)\s+(\?)?\\?([A-Z][a-zA-Z0-9_\\]*)\s+\${}\s*[;=]"#,
        escaped_prop
    );
    if let Some(caps) = regex::Regex::new(&typed_prop_pattern)
        .ok()
        .and_then(|re| re.captures(content))
    {
        if let Some(type_match) = caps.get(2) {
            let nullable = caps.get(1).is_some();
            let type_name = simplify_type(type_match.as_str());
            return Some(if nullable {
                format!("?{}", type_name)
            } else {
                type_name
            });
        }
    }

    // 3a. Constructor property promotion with primitive: __construct(protected int $count)
    let primitive_promoted_pattern = format!(
        r#"__construct\s*\([^)]*(?:public|protected|private)\s+(\?)?({})(?:\s+|\s*\|\s*[a-zA-Z]+\s+)\${}"#,
        primitive_types, escaped_prop
    );
    if let Some(caps) = regex::Regex::new(&primitive_promoted_pattern)
        .ok()
        .and_then(|re| re.captures(content))
    {
        if let Some(type_match) = caps.get(2) {
            let nullable = caps.get(1).is_some();
            let type_name = type_match.as_str().to_string();
            return Some(if nullable {
                format!("?{}", type_name)
            } else {
                type_name
            });
        }
    }

    // 3b. Constructor property promotion with class type
    let promoted_pattern = format!(
        r#"__construct\s*\([^)]*(?:public|protected|private)\s+(\?)?\\?([A-Z][a-zA-Z0-9_\\]*)\s+\${}"#,
        escaped_prop
    );
    if let Some(caps) = regex::Regex::new(&promoted_pattern)
        .ok()
        .and_then(|re| re.captures(content))
    {
        if let Some(type_match) = caps.get(2) {
            let nullable = caps.get(1).is_some();
            let type_name = simplify_type(type_match.as_str());
            return Some(if nullable {
                format!("?{}", type_name)
            } else {
                type_name
            });
        }
    }

    // 4. Property initialized in constructor with new: $this->user = new User()
    let constructor_new_pattern = format!(
        r#"\$this->{}\s*=\s*new\s+\\?([A-Z][a-zA-Z0-9_\\]*)"#,
        escaped_prop
    );
    if let Some(caps) = regex::Regex::new(&constructor_new_pattern)
        .ok()
        .and_then(|re| re.captures(content))
    {
        if let Some(class) = caps.get(1) {
            return Some(simplify_type(class.as_str()));
        }
    }

    // 5. Property initialized with Model::find() etc
    let model_assign_pattern = format!(
        r#"\$this->{}\s*=\s*\\?([A-Z][a-zA-Z0-9_\\]*)::(?:find|first|create)"#,
        escaped_prop
    );
    if let Some(caps) = regex::Regex::new(&model_assign_pattern)
        .ok()
        .and_then(|re| re.captures(content))
    {
        if let Some(model) = caps.get(1) {
            return Some(simplify_type(model.as_str()));
        }
    }

    None
}

/// Extract the return type of a method from PHP class content.
/// Prefers `@return Foo<Bar>` PHPDoc over the bare return type declaration (PHPDoc carries generics).
pub fn extract_method_return_type(content: &str, method_name: &str) -> Option<String> {
    let escaped = regex::escape(method_name);

    let decl_pattern = format!(
        r#"function\s+{}\s*\([^)]*\)\s*:\s*(\??\\?[A-Za-z][A-Za-z0-9_\\|]*)"#,
        escaped
    );
    let decl_re = regex::Regex::new(&decl_pattern).ok()?;
    let decl_match = decl_re.captures(content)?;
    let method_pos = decl_match.get(0)?.start();
    let declared = decl_match
        .get(1)
        .map(|m| normalize_generic_type(m.as_str()));

    let scan_start = method_pos.saturating_sub(600);
    let preface = &content[scan_start..method_pos];

    let return_re = regex::Regex::new(r#"@return\s+([^\r\n]+)"#).ok()?;
    let phpdoc_type = return_re
        .captures_iter(preface)
        .last()
        .and_then(|c| c.get(1))
        .map(|m| {
            m.as_str()
                .trim()
                .trim_end_matches("*/")
                .trim()
                .trim_start_matches('\\')
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .map(|s| normalize_generic_type(&s));

    phpdoc_type.or(declared)
}

/// Normalize a (possibly generic) type by stripping leading backslashes and simplifying namespaces
/// on the base type while preserving generic arguments.
/// `"\Illuminate\Pagination\LengthAwarePaginator<int, \App\Audit>"` -> `"LengthAwarePaginator<int, Audit>"`
pub fn normalize_generic_type(type_str: &str) -> String {
    let trimmed = type_str
        .trim()
        .trim_start_matches('?')
        .trim_start_matches('\\');
    if let Some((base, args)) = parse_generic_args(trimmed) {
        let base_simple = simplify_type(&base);
        let args_simple: Vec<String> = args
            .into_iter()
            .map(|a| normalize_generic_type(&a))
            .collect();
        format!("{}<{}>", base_simple, args_simple.join(", "))
    } else {
        simplify_type(trimmed)
    }
}

/// Decompose a generic type expression like `Foo<A, B<C>>` into `("Foo", ["A", "B<C>"])`.
/// Returns None if the input has no top-level generic args.
pub fn parse_generic_args(type_str: &str) -> Option<(String, Vec<String>)> {
    let open = type_str.find('<')?;
    if !type_str.ends_with('>') {
        return None;
    }
    let base = type_str[..open].trim().to_string();
    let inner = &type_str[open + 1..type_str.len() - 1];

    let mut args = Vec::new();
    let mut depth = 0i32;
    let mut current = String::new();
    for ch in inner.chars() {
        match ch {
            '<' => {
                depth += 1;
                current.push(ch);
            }
            '>' => {
                depth -= 1;
                current.push(ch);
            }
            ',' if depth == 0 => {
                args.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        args.push(current.trim().to_string());
    }

    Some((base, args))
}

/// Given a (possibly generic) iterable type, return the element type if inferable.
/// `Collection<Audit>` -> `Some("Audit")`
/// `LengthAwarePaginator<int, Audit>` -> `Some("Audit")` (last generic arg)
/// `Collection` (no generics) -> `None`
pub fn iterable_element_type(type_str: &str) -> Option<String> {
    let (_, args) = parse_generic_args(type_str.trim_start_matches('?'))?;
    args.last().cloned()
}

/// Resolve a `$this->X` member access against a component file's PHP source.
/// Tries property type first, then method return type. Returns the resolved type string.
pub fn resolve_member_type(content: &str, member_name: &str) -> Option<String> {
    find_property_type_in_content(content, member_name)
        .or_else(|| extract_method_return_type(content, member_name))
}

/// Detect whether `content` contains at least one inline anonymous-class
/// Livewire component signature. Matches `new (#[Attr])* class extends [\Namespace\]*Component`:
///   - bare `Component` (after `use Livewire\Component;`)
///   - fully-qualified `\Livewire\Component`
///   - leading PHP attributes such as `#[Layout('layouts.app')]`
///
/// Used to detect Livewire 4 single-file and multi-file component sources.
pub fn detect_inline_livewire_class(content: &str) -> bool {
    let pattern = r"new\s+(?:#\[[^\]]+\]\s+)*class\s+extends\s+\\?(?:[A-Za-z_][A-Za-z0-9_]*\\\\?)*Component\b";
    regex::Regex::new(pattern)
        .ok()
        .map(|re| re.is_match(content))
        .unwrap_or(false)
}

/// Scan every `function mount(...)` signature in `content` and return the type
/// declared on the first parameter named `param_name`. Returns `None` for
/// untyped params — type *refinement* of an existing declaration must not
/// downgrade the property to `"mixed"` just because the matching `mount()`
/// param has no type-hint.
///
/// Returns:
///   - `Some("ClassName")` for class-typed params (FQCN simplified)
///   - `Some("int" / "string" / ...)` for primitive-typed params
///   - `Some("?Type")` when the param is nullable
///   - `None` when the param is untyped, or no matching param exists
///
/// **This does NOT synthesize properties.** Livewire 4 requires a `public $name`
/// declaration for `$name` to appear in Blade. This helper only supplies type
/// information for properties that are already declared without an explicit type
/// — Livewire's runtime auto-assigns the matching mount() param's value into
/// the property, so the IDE can safely show the param's type.
pub fn find_mount_param_type(content: &str, param_name: &str) -> Option<String> {
    for params in iter_mount_param_lists(content) {
        for chunk in split_params(&params) {
            let parsed = parse_mount_param(&chunk)?;
            if parsed.name == param_name {
                return parsed.php_type;
            }
        }
    }
    None
}

/// Extract every `public` property declaration from `content`, returning
/// `(name, type)` pairs. Untyped declarations resolve to `"mixed"`. Used as
/// the generic property lister for class types that aren't Eloquent models
/// (Livewire components, Livewire Forms, plain DTOs, value objects).
///
/// Skips `static` properties and non-public visibility. The regex scans the
/// whole content, so works on both standard class files and inline
/// anonymous-class Livewire SFC sources.
pub fn extract_class_properties(content: &str) -> Vec<(String, String)> {
    let prop_re = match regex::Regex::new(
        r#"public\s+(?:(\?)?\\?([A-Za-z_][A-Za-z0-9_\\]*)\s+)?\$([a-zA-Z_][a-zA-Z0-9_]*)\s*[;=]"#,
    ) {
        Ok(re) => re,
        Err(_) => return Vec::new(),
    };

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut props = Vec::new();
    for cap in prop_re.captures_iter(content) {
        let Some(name) = cap.get(3) else { continue };
        let name = name.as_str().to_string();
        if !seen.insert(name.clone()) {
            continue;
        }
        let php_type = match cap.get(2) {
            Some(t) => {
                let nullable = cap.get(1).is_some();
                let simple = simplify_type(t.as_str());
                if nullable {
                    format!("?{}", simple)
                } else {
                    simple
                }
            }
            None => "mixed".to_string(),
        };
        props.push((name, php_type));
    }
    props
}

/// Locate the source position (0-based line, column range) of a `public ... $name`
/// declaration in `content`. Returns `None` if the property isn't declared.
///
/// The returned column range covers the `$name` identifier (including the
/// `$` sigil), which is what LSP goto-definition wants — clicking lands the
/// cursor on the property name rather than the surrounding declaration.
pub fn find_property_declaration_position(
    content: &str,
    property_name: &str,
) -> Option<(u32, u32, u32)> {
    let escaped = regex::escape(property_name);
    let pattern = format!(
        r"public\s+(?:\??\\?[A-Za-z_][A-Za-z0-9_\\]*\s+)?(\${})\b",
        escaped
    );
    let re = regex::Regex::new(&pattern).ok()?;
    let caps = re.captures(content)?;
    let sigil_match = caps.get(1)?;
    let byte_offset = sigil_match.start();

    let mut line = 0u32;
    let mut last_line_start = 0usize;
    for (i, ch) in content[..byte_offset].char_indices() {
        if ch == '\n' {
            line += 1;
            last_line_start = i + 1;
        }
    }
    let start_col = (byte_offset - last_line_start) as u32;
    let end_col = start_col + sigil_match.as_str().len() as u32;
    Some((line, start_col, end_col))
}

/// Find the 0-based line number where a property is first defined on a class.
/// Used by the Blade-variable hover to render `Defined at: <file>:<line>`.
///
/// Recognises the following shapes (whichever matches earliest in the file
/// wins — when an Eloquent model declares the same column in both `$casts`
/// and `$fillable`, the cast usually appears first and is the more useful
/// landing spot):
///
/// - **Visibility-modified property**: `public Type $foo`, `protected $foo`,
///   `private static ?Foo $foo`.
/// - **PHPDoc property tag**: `@property Type $foo`, `@property-read`,
///   `@property-write`.
/// - **Array key in a property block**: `'foo' =>` — captures `$casts`,
///   `$fillable`, `$attributes`, and any other class-level array literal that
///   uses the property as a key.
/// - **Method**: `public function foo(...)` — catches Eloquent relationship
///   methods accessed as properties (`$user->posts`).
pub fn find_property_definition_line(content: &str, property: &str) -> Option<u32> {
    let escaped = regex::escape(property);
    let patterns = [
        // (a) visibility-modified declaration
        format!(
            r"\b(?:public|private|protected)\s+(?:static\s+)?(?:\??\\?[A-Za-z_][A-Za-z0-9_\\|]*(?:<[^>]*>)?\s+)?\${}\b",
            escaped
        ),
        // (b) @property PHPDoc
        format!(r"@property(?:-read|-write)?\s+\S+\s+\${}\b", escaped),
        // (c) array-key form — catches $casts, $fillable, $attributes
        format!(r#"['"]{}['"]\s*=>"#, escaped),
        // (d) method definition (matches relationship methods accessed as props)
        format!(r"\bfunction\s+{}\s*\(", escaped),
    ];

    let mut best_offset: Option<usize> = None;
    for pat in &patterns {
        let Ok(re) = regex::Regex::new(pat) else {
            continue;
        };
        if let Some(m) = re.find(content) {
            best_offset = Some(best_offset.map_or(m.start(), |b| b.min(m.start())));
        }
    }

    let offset = best_offset?;
    let mut line = 0u32;
    for (i, ch) in content.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
        }
    }
    Some(line)
}

/// Read a single line (0-based) from a file, with trailing whitespace
/// trimmed but leading whitespace preserved (so indentation context is
/// kept in hover snippets). Returns `None` on I/O failure or out-of-range
/// line numbers. Used by the route hover to pull the
/// `Route::verb(...)->name(...)` line straight from source.
pub fn read_line_from_file(path: &std::path::Path, line: u32) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let target = content.lines().nth(line as usize)?;
    Some(target.trim_end().to_string())
}

/// Extract the fully qualified class name from a PHP source file. Combines
/// the `namespace ...;` declaration (when present) with the first `class
/// Foo`/`interface Foo`/`trait Foo` declaration.
///
/// Returns `Some("App\\Livewire\\Counter")` or `None` when neither piece can
/// be found. Used by hovers that want to surface the FQN as their bold
/// header — gives the reader a fully-resolved type they don't see at the
/// cursor (`<livewire:counter>` → `App\Livewire\Counter`).
pub fn extract_class_fqn(path: &std::path::Path) -> Option<String> {
    use lazy_static::lazy_static;
    use regex::Regex;
    lazy_static! {
        static ref NS_RE: Regex =
            Regex::new(r"(?m)^\s*namespace\s+([A-Za-z_][A-Za-z0-9_\\]*)\s*;").unwrap();
        static ref CLASS_NAME_RE: Regex = Regex::new(
            r"(?m)^\s*(?:(?:final|abstract|readonly)\s+)*(?:class|interface|trait|enum)\s+(\w+)",
        )
        .unwrap();
    }
    let content = std::fs::read_to_string(path).ok()?;
    let class_name = CLASS_NAME_RE
        .captures(&content)?
        .get(1)?
        .as_str()
        .to_string();
    let namespace = NS_RE
        .captures(&content)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str());
    Some(match namespace {
        Some(ns) => format!("{}\\{}", ns, class_name),
        None => class_name,
    })
}

/// Extract the first `class Foo extends Bar` signature line from a PHP class
/// file. Skips PHP attributes (`#[...]`), PHPDoc, `namespace`, and `use`
/// statements that precede the class declaration. Returns the single line
/// containing the `class` keyword, trimmed.
///
/// Used by the Livewire hover to show the component's class signature so
/// the reader sees the type at a glance — same vibe as intelephense's
/// hover showing a class signature line in a `php` code block.
///
/// Recognises `final class Foo`, `abstract class Foo`, `readonly class Foo`,
/// and combinations. Anchored at start-of-line so `class` substrings inside
/// string literals or comments don't trigger false matches.
pub fn extract_class_signature(path: &std::path::Path) -> Option<String> {
    use lazy_static::lazy_static;
    use regex::Regex;
    lazy_static! {
        static ref CLASS_RE: Regex =
            Regex::new(r"(?m)^\s*(?:(?:final|abstract|readonly)\s+)*class\s+\w+[^{\n]*").unwrap();
    }
    let content = std::fs::read_to_string(path).ok()?;
    let m = CLASS_RE.find(&content)?;
    Some(m.as_str().trim().to_string())
}

/// Rich information about a property/method declaration extracted from a
/// PHP class source — declaration text, PHPDoc summary, and tags. Used by
/// the Blade-variable hover to render intelephense-style tooltips.
#[derive(Debug, Clone)]
pub struct PropertyDeclaration {
    /// The single line of source containing the declaration, trimmed.
    /// For methods, this is just the signature line (not the body).
    pub declaration_text: String,
    /// 0-based line number of the declaration.
    pub line: u32,
    /// Free-form description from any PHPDoc block immediately above the
    /// declaration. `None` when there's no PHPDoc or it has only tags.
    pub description: Option<String>,
    /// Parsed PHPDoc tag lines in document order (e.g. `@param mixed $x`,
    /// `@return Response`, `@throws AuthException`). Each entry is the
    /// full tag text without the leading `*` decoration.
    pub phpdoc_tags: Vec<String>,
}

/// Locate a property's declaration and parse the PHPDoc block above it.
/// Builds on [`find_property_definition_line`] for the position lookup, then
/// extracts the declaration line and any preceding `/** ... */` block.
///
/// The PHPDoc parser is intentionally loose: it strips leading `*` and
/// concatenates description lines, then collects any `@tag` lines verbatim.
/// Mirrors what intelephense's hover does — show the user the same prose the
/// PHPDoc author wrote, without imposing a strict tag schema.
pub fn extract_property_declaration(content: &str, property: &str) -> Option<PropertyDeclaration> {
    let line = find_property_definition_line(content, property)?;
    let lines: Vec<&str> = content.lines().collect();
    let declaration_text = lines.get(line as usize)?.trim().to_string();

    // Walk backward looking for a PHPDoc block (`/** ... */`) immediately
    // above. Skip blank lines, but anything else (other code, attributes)
    // breaks the association.
    let mut phpdoc_lines: Vec<&str> = Vec::new();
    if (line as usize) > 0 {
        let mut idx = line as usize;
        // Skip blank lines between declaration and a potential PHPDoc close.
        while idx > 0 {
            idx -= 1;
            let l = lines[idx].trim();
            if l.is_empty() {
                continue;
            }
            if l.ends_with("*/") {
                // Walk back to the matching `/**` opener.
                phpdoc_lines.push(l);
                while idx > 0 {
                    idx -= 1;
                    let pl = lines[idx];
                    phpdoc_lines.push(pl);
                    if pl.trim_start().starts_with("/**") {
                        break;
                    }
                }
                phpdoc_lines.reverse();
            }
            break;
        }
    }
    let (description, phpdoc_tags) = parse_phpdoc(&phpdoc_lines);

    Some(PropertyDeclaration {
        declaration_text,
        line,
        description,
        phpdoc_tags,
    })
}

/// Split a captured PHPDoc block into (description, tags). Strips
/// `/**`, `*/`, and leading `*` decoration from each line. Description
/// stops at the first `@tag` line.
fn parse_phpdoc(lines: &[&str]) -> (Option<String>, Vec<String>) {
    let mut description_parts: Vec<String> = Vec::new();
    let mut tags: Vec<String> = Vec::new();
    let mut in_tags = false;

    for raw in lines {
        // Strip block delimiters and leading * decoration.
        let cleaned = raw
            .trim()
            .trim_start_matches("/**")
            .trim_end_matches("*/")
            .trim()
            .trim_start_matches('*')
            .trim();

        if cleaned.is_empty() {
            continue;
        }
        if let Some(stripped) = cleaned.strip_prefix('@') {
            in_tags = true;
            tags.push(format!("@{}", stripped));
        } else if !in_tags {
            description_parts.push(cleaned.to_string());
        }
    }

    let description = if description_parts.is_empty() {
        None
    } else {
        Some(description_parts.join(" "))
    };
    (description, tags)
}

/// Iterate every typed parameter across all `function mount(...)` signatures
/// in `content`. Untyped params are skipped (see `find_mount_param_type` for
/// the rationale). Duplicates by name keep the first occurrence.
pub fn find_all_mount_param_types(content: &str) -> Vec<(String, String)> {
    let mut results: Vec<(String, String)> = Vec::new();
    for params in iter_mount_param_lists(content) {
        for chunk in split_params(&params) {
            if let Some(parsed) = parse_mount_param(&chunk) {
                if let Some(php_type) = parsed.php_type {
                    if !results.iter().any(|(n, _)| n == &parsed.name) {
                        results.push((parsed.name, php_type));
                    }
                }
            }
        }
    }
    results
}

struct MountParam {
    name: String,
    /// `None` for untyped params — callers decide how to handle the absence.
    php_type: Option<String>,
}

fn iter_mount_param_lists(content: &str) -> Vec<String> {
    let re = match regex::Regex::new(r"function\s+mount\s*\(([^)]*)\)") {
        Ok(re) => re,
        Err(_) => return Vec::new(),
    };
    re.captures_iter(content)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
        .collect()
}

fn split_params(params: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut depth = 0i32;
    let mut current = String::new();
    for ch in params.chars() {
        match ch {
            '[' | '(' | '{' => {
                depth += 1;
                current.push(ch);
            }
            ']' | ')' | '}' => {
                depth -= 1;
                current.push(ch);
            }
            ',' if depth == 0 => {
                chunks.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    let last = current.trim().to_string();
    if !last.is_empty() {
        chunks.push(last);
    }
    chunks
}

fn parse_mount_param(chunk: &str) -> Option<MountParam> {
    let trimmed = chunk.trim_start_matches(|c: char| c.is_whitespace() || c == '&');
    let trimmed = trimmed.trim_start_matches("...");
    let trimmed = trimmed.trim();

    let re =
        regex::Regex::new(r"^(?:(\?)?\\?([A-Za-z_][A-Za-z0-9_\\]*)\s+)?\$([a-zA-Z_][a-zA-Z0-9_]*)")
            .ok()?;
    let caps = re.captures(trimmed)?;
    let name = caps.get(3)?.as_str().to_string();

    let php_type = match (caps.get(1), caps.get(2)) {
        (Some(_), Some(t)) => Some(format!("?{}", simplify_type(t.as_str()))),
        (None, Some(t)) => Some(simplify_type(t.as_str())),
        _ => None,
    };

    Some(MountParam { name, php_type })
}
