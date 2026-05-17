//! Parsing helpers for PHP class files (Livewire components, View components, models).
//!
//! These functions extract property types, method return types, and decompose
//! generic type expressions out of PHP source. They're defined in the library
//! crate so Salsa-tracked queries can use them.

/// Simplify a fully qualified class name to just the class name.
/// `\Illuminate\Pagination\LengthAwarePaginator` -> `LengthAwarePaginator`
pub fn simplify_type(type_name: &str) -> String {
    type_name.rsplit('\\').next().unwrap_or(type_name).to_string()
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
    if let Some(caps) = regex::Regex::new(&phpdoc_pattern).ok().and_then(|re| re.captures(content)) {
        if let Some(type_match) = caps.get(1) {
            return Some(simplify_type(type_match.as_str()));
        }
    }

    // 2a. Typed property with primitive type: protected int $count;
    let primitive_prop_pattern = format!(
        r#"(?:public|protected|private)\s+(\?)?({})(?:\s+|\s*\|\s*[a-zA-Z]+\s+)\${}\s*[;=]"#,
        primitive_types, escaped_prop
    );
    if let Some(caps) = regex::Regex::new(&primitive_prop_pattern).ok().and_then(|re| re.captures(content)) {
        if let Some(type_match) = caps.get(2) {
            let nullable = caps.get(1).is_some();
            let type_name = type_match.as_str().to_string();
            return Some(if nullable { format!("?{}", type_name) } else { type_name });
        }
    }

    // 2b. Typed property with class type: protected User $user;
    let typed_prop_pattern = format!(
        r#"(?:public|protected|private)\s+(\?)?\\?([A-Z][a-zA-Z0-9_\\]*)\s+\${}\s*[;=]"#,
        escaped_prop
    );
    if let Some(caps) = regex::Regex::new(&typed_prop_pattern).ok().and_then(|re| re.captures(content)) {
        if let Some(type_match) = caps.get(2) {
            let nullable = caps.get(1).is_some();
            let type_name = simplify_type(type_match.as_str());
            return Some(if nullable { format!("?{}", type_name) } else { type_name });
        }
    }

    // 3a. Constructor property promotion with primitive: __construct(protected int $count)
    let primitive_promoted_pattern = format!(
        r#"__construct\s*\([^)]*(?:public|protected|private)\s+(\?)?({})(?:\s+|\s*\|\s*[a-zA-Z]+\s+)\${}"#,
        primitive_types, escaped_prop
    );
    if let Some(caps) = regex::Regex::new(&primitive_promoted_pattern).ok().and_then(|re| re.captures(content)) {
        if let Some(type_match) = caps.get(2) {
            let nullable = caps.get(1).is_some();
            let type_name = type_match.as_str().to_string();
            return Some(if nullable { format!("?{}", type_name) } else { type_name });
        }
    }

    // 3b. Constructor property promotion with class type
    let promoted_pattern = format!(
        r#"__construct\s*\([^)]*(?:public|protected|private)\s+(\?)?\\?([A-Z][a-zA-Z0-9_\\]*)\s+\${}"#,
        escaped_prop
    );
    if let Some(caps) = regex::Regex::new(&promoted_pattern).ok().and_then(|re| re.captures(content)) {
        if let Some(type_match) = caps.get(2) {
            let nullable = caps.get(1).is_some();
            let type_name = simplify_type(type_match.as_str());
            return Some(if nullable { format!("?{}", type_name) } else { type_name });
        }
    }

    // 4. Property initialized in constructor with new: $this->user = new User()
    let constructor_new_pattern = format!(
        r#"\$this->{}\s*=\s*new\s+\\?([A-Z][a-zA-Z0-9_\\]*)"#,
        escaped_prop
    );
    if let Some(caps) = regex::Regex::new(&constructor_new_pattern).ok().and_then(|re| re.captures(content)) {
        if let Some(class) = caps.get(1) {
            return Some(simplify_type(class.as_str()));
        }
    }

    // 5. Property initialized with Model::find() etc
    let model_assign_pattern = format!(
        r#"\$this->{}\s*=\s*\\?([A-Z][a-zA-Z0-9_\\]*)::(?:find|first|create)"#,
        escaped_prop
    );
    if let Some(caps) = regex::Regex::new(&model_assign_pattern).ok().and_then(|re| re.captures(content)) {
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
    let declared = decl_match.get(1).map(|m| normalize_generic_type(m.as_str()));

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
    let trimmed = type_str.trim().trim_start_matches('?').trim_start_matches('\\');
    if let Some((base, args)) = parse_generic_args(trimmed) {
        let base_simple = simplify_type(&base);
        let args_simple: Vec<String> = args.into_iter().map(|a| normalize_generic_type(&a)).collect();
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
            '<' => { depth += 1; current.push(ch); }
            '>' => { depth -= 1; current.push(ch); }
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

/// Detect whether `content` contains at least one Volt anonymous-class signature.
/// Matches `new (#[Attr])* class extends [\Namespace\]*Component`, covering:
///   - bare `Component` (used after `use Livewire\Volt\Component;`)
///   - fully-qualified `\Livewire\Volt\Component` or `\Livewire\Component`
///   - leading PHP attributes such as `#[Layout('layouts.app')]`
///
/// Used to gate Volt-only behaviors (mount-param promotion) — classic Livewire
/// classes live in a separate `.php` file under `app/Livewire/` and never match
/// this pattern.
pub fn detect_inline_volt_class(content: &str) -> bool {
    let pattern = r"new\s+(?:#\[[^\]]+\]\s+)*class\s+extends\s+\\?(?:[A-Za-z_][A-Za-z0-9_]*\\\\?)*Component\b";
    regex::Regex::new(pattern)
        .ok()
        .map(|re| re.is_match(content))
        .unwrap_or(false)
}

/// Volt promotes typed `mount()` parameters to public properties.
/// Scan every `function mount(...)` signature in `content` and return the first
/// parameter named `param_name`. Returns:
///   - `Some("ClassName")` for class-typed params (FQCN simplified)
///   - `Some("int" / "string" / ...)` for primitive-typed params
///   - `Some("?Type")` when the param is nullable
///   - `Some("mixed")` for untyped params
///   - `None` when `param_name` does not appear in any `mount()` signature
pub fn find_mount_promoted_type(content: &str, param_name: &str) -> Option<String> {
    for params in iter_mount_param_lists(content) {
        for chunk in split_params(&params) {
            let parsed = parse_mount_param(&chunk)?;
            if parsed.name == param_name {
                return Some(parsed.php_type);
            }
        }
    }
    None
}

/// Like `find_mount_promoted_type` but returns every parameter across all
/// `function mount(...)` signatures in the file. Duplicates by name keep the
/// first occurrence (matches `find_mount_promoted_type` lookup order).
pub fn find_all_mount_promoted_params(content: &str) -> Vec<(String, String)> {
    let mut results: Vec<(String, String)> = Vec::new();
    for params in iter_mount_param_lists(content) {
        for chunk in split_params(&params) {
            if let Some(parsed) = parse_mount_param(&chunk) {
                if !results.iter().any(|(n, _)| n == &parsed.name) {
                    results.push((parsed.name, parsed.php_type));
                }
            }
        }
    }
    results
}

struct MountParam {
    name: String,
    php_type: String,
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

    let re = regex::Regex::new(
        r"^(?:(\?)?\\?([A-Za-z_][A-Za-z0-9_\\]*)\s+)?\$([a-zA-Z_][a-zA-Z0-9_]*)",
    )
    .ok()?;
    let caps = re.captures(trimmed)?;
    let name = caps.get(3)?.as_str().to_string();

    let php_type = match (caps.get(1), caps.get(2)) {
        (Some(_), Some(t)) => format!("?{}", simplify_type(t.as_str())),
        (None, Some(t)) => simplify_type(t.as_str()),
        _ => "mixed".to_string(),
    };

    Some(MountParam { name, php_type })
}
