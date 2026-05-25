//! Parse `config/livewire.php` for the keys Phase 3 rename needs.
//!
//! Seven keys matter for rename:
//!
//!   - `component_locations` — root directories the resolver walks looking
//!     for view-based SFC and MFC components
//!   - `component_namespaces` — `prefix => path` map for `<livewire:prefix::name>`
//!   - `make_command.type` — `sfc | mfc | class`, the default format for
//!     new components (used when a cross-dir rename creates files in a
//!     previously empty namespace)
//!   - `make_command.emoji` — whether new SFC/MFC filenames get the `⚡`
//!     prefix; rename mirrors the *existing* state on disk and only
//!     consults this when creating new files
//!   - `class_namespace` — root namespace for class-based components
//!   - `class_path` — disk path paired with `class_namespace`
//!   - `view_path` — fallback view directory when a class-based component
//!     omits its `render()` method
//!
//! Defaults come from Livewire 4's ship config. Each override is parsed
//! tolerantly — a malformed value falls through to the default rather than
//! returning an error. The user-visible failure mode is "rename picks the
//! wrong file" not "the LSP refuses to start".
//!
//! Parsing uses substring scanning rather than full PHP AST traversal,
//! matching the style of [`crate::salsa_impl::parse_view_config`] and
//! [`crate::salsa_impl::parse_composer_json`]. Default configs are simple
//! enough that this is robust; the cost of a parse miss is a fallback to
//! defaults, which is correct for the unmodified-config case.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Default format the `make:livewire` command emits when none is specified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentFormat {
    /// Single-file component — one `.blade.php` with inline `new class
    /// extends Component`.
    Sfc,
    /// Multi-file component — a directory `⚡{name}/` containing
    /// `{name}.php` + `{name}.blade.php` (+ optional js/css).
    Mfc,
    /// Class-based component — separate class file under `class_path`
    /// and view file under `view_path`. The v3 carry-over shape.
    Class,
}

/// Fully resolved Livewire configuration with PHP helper calls already
/// expanded against the project root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LivewireConfig {
    pub component_locations: Vec<PathBuf>,
    pub component_namespaces: HashMap<String, PathBuf>,
    pub make_command_type: ComponentFormat,
    pub make_command_emoji: bool,
    pub class_namespace: String,
    pub class_path: PathBuf,
    pub view_path: PathBuf,
}

impl LivewireConfig {
    /// Defaults from Livewire 4's ship `config/livewire.php`. Used when the
    /// file is missing or when an individual key fails to parse.
    pub fn defaults(root: &Path) -> Self {
        let mut component_namespaces = HashMap::new();
        component_namespaces.insert("layouts".to_string(), root.join("resources/views/layouts"));
        component_namespaces.insert("pages".to_string(), root.join("resources/views/pages"));

        Self {
            component_locations: vec![
                root.join("resources/views/components"),
                root.join("resources/views/livewire"),
            ],
            component_namespaces,
            make_command_type: ComponentFormat::Sfc,
            make_command_emoji: true,
            class_namespace: "App\\Livewire".to_string(),
            class_path: root.join("app/Livewire"),
            view_path: root.join("resources/views/livewire"),
        }
    }
}

/// Parse a `config/livewire.php` source string into a fully resolved
/// [`LivewireConfig`]. Missing or malformed keys fall back to defaults.
pub fn parse(source: &str, root: &Path) -> LivewireConfig {
    let mut config = LivewireConfig::defaults(root);

    if let Some(val) = extract_make_command_bool(source, "emoji") {
        config.make_command_emoji = val;
    }
    if let Some(val) = extract_make_command_string(source, "type") {
        if let Some(fmt) = parse_component_format(&val) {
            config.make_command_type = fmt;
        }
    }
    if let Some(val) = extract_top_level_string(source, "class_namespace") {
        config.class_namespace = unescape_php_namespace(&val);
    }
    if let Some(val) = extract_top_level_path(source, "class_path", root) {
        config.class_path = val;
    }
    if let Some(val) = extract_top_level_path(source, "view_path", root) {
        config.view_path = val;
    }
    if let Some(val) = extract_path_array(source, "component_locations", root) {
        config.component_locations = val;
    }
    if let Some(val) = extract_path_map(source, "component_namespaces", root) {
        config.component_namespaces = val;
    }

    config
}

// ---------- key location ----------

/// Find the byte offset just past the `=>` of `'key' => ` or `"key" => `.
/// Returns the slice starting at the value (whitespace already skipped).
fn locate_value_after_key<'a>(source: &'a str, key: &str) -> Option<&'a str> {
    for quote in ['\'', '"'] {
        let needle = format!("{}{}{}", quote, key, quote);
        let mut cursor = 0usize;
        while let Some(rel) = source[cursor..].find(&needle) {
            let abs = cursor + rel;
            // Reject false positives inside a comment block by checking that
            // the line containing the match isn't a `//` or `#` comment line.
            // Block-comment elision is intentionally not done — config files
            // very rarely guard real keys with `/* ... */`.
            if is_on_comment_line(source, abs) {
                cursor = abs + needle.len();
                continue;
            }
            let after = &source[abs + needle.len()..];
            let trimmed = after.trim_start();
            if let Some(rest) = trimmed.strip_prefix("=>") {
                return Some(rest.trim_start());
            }
            cursor = abs + needle.len();
        }
    }
    None
}

fn is_on_comment_line(source: &str, position: usize) -> bool {
    let line_start = source[..position].rfind('\n').map(|p| p + 1).unwrap_or(0);
    let line = &source[line_start..position];
    let trimmed = line.trim_start();
    trimmed.starts_with("//") || trimmed.starts_with('#')
}

// ---------- value extraction ----------

fn extract_top_level_string(source: &str, key: &str) -> Option<String> {
    let value_start = locate_value_after_key(source, key)?;
    extract_quoted_string(value_start)
}

fn extract_top_level_path(source: &str, key: &str, root: &Path) -> Option<PathBuf> {
    let value_start = locate_value_after_key(source, key)?;
    resolve_path_expression(value_start, root)
}

fn extract_make_command_bool(source: &str, inner_key: &str) -> Option<bool> {
    let mc_value = locate_value_after_key(source, "make_command")?;
    let inner = locate_value_after_key(mc_value, inner_key)?;
    extract_bool(inner)
}

fn extract_make_command_string(source: &str, inner_key: &str) -> Option<String> {
    let mc_value = locate_value_after_key(source, "make_command")?;
    let inner = locate_value_after_key(mc_value, inner_key)?;
    extract_quoted_string(inner)
}

fn extract_path_array(source: &str, key: &str, root: &Path) -> Option<Vec<PathBuf>> {
    let value_start = locate_value_after_key(source, key)?;
    let inner = extract_array_inner(value_start)?;

    let mut paths = Vec::new();
    for raw in split_top_level_commas(inner) {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(p) = resolve_path_expression(trimmed, root) {
            paths.push(p);
        }
    }
    if paths.is_empty() {
        None
    } else {
        Some(paths)
    }
}

fn extract_path_map(source: &str, key: &str, root: &Path) -> Option<HashMap<String, PathBuf>> {
    let value_start = locate_value_after_key(source, key)?;
    let inner = extract_array_inner(value_start)?;

    let mut out = HashMap::new();
    for raw in split_top_level_commas(inner) {
        let entry = raw.trim();
        if entry.is_empty() {
            continue;
        }
        let Some((key_part, value_part)) = split_on_fat_arrow(entry) else {
            continue;
        };
        let Some(ns_key) = extract_quoted_string(key_part.trim()) else {
            continue;
        };
        let Some(ns_path) = resolve_path_expression(value_part.trim(), root) else {
            continue;
        };
        out.insert(ns_key, ns_path);
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

// ---------- primitives ----------

fn extract_quoted_string(s: &str) -> Option<String> {
    let s = s.trim_start();
    for quote in ['\'', '"'] {
        if let Some(rest) = s.strip_prefix(quote) {
            // Walk forward respecting `\\` escape so `'App\\Livewire'` reads
            // intact through the closing quote.
            let mut out = String::new();
            let mut chars = rest.chars();
            while let Some(c) = chars.next() {
                if c == '\\' {
                    if let Some(next) = chars.next() {
                        out.push(c);
                        out.push(next);
                    }
                    continue;
                }
                if c == quote {
                    return Some(out);
                }
                out.push(c);
            }
        }
    }
    None
}

fn extract_bool(s: &str) -> Option<bool> {
    let trimmed = s.trim_start();
    if trimmed.starts_with("true") {
        Some(true)
    } else if trimmed.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

fn parse_component_format(value: &str) -> Option<ComponentFormat> {
    match value {
        "sfc" => Some(ComponentFormat::Sfc),
        "mfc" => Some(ComponentFormat::Mfc),
        "class" => Some(ComponentFormat::Class),
        _ => None,
    }
}

/// Resolve a PHP path expression — either a quoted string or one of the
/// well-known Laravel path helpers — against the project root.
fn resolve_path_expression(value: &str, root: &Path) -> Option<PathBuf> {
    let trimmed = value.trim_start();

    for (helper, subdir) in PATH_HELPERS {
        let opener = format!("{}(", helper);
        if let Some(rest) = trimmed.strip_prefix(opener.as_str()) {
            let inner = extract_string_until_close_paren(rest)?;
            return Some(if subdir.is_empty() {
                root.join(&inner)
            } else if inner.is_empty() {
                root.join(subdir)
            } else {
                root.join(subdir).join(&inner)
            });
        }
    }

    // Bare quoted path. Absolute → use as-is; relative → join with root.
    let s = extract_quoted_string(trimmed)?;
    Some(if Path::new(&s).is_absolute() {
        PathBuf::from(s)
    } else {
        root.join(&s)
    })
}

/// The Laravel path helpers we recognize. Mapping: helper name → subdir
/// under project root that the helper conventionally targets. `base_path`
/// has an empty subdir (the root itself).
const PATH_HELPERS: &[(&str, &str)] = &[
    ("resource_path", "resources"),
    ("app_path", "app"),
    ("config_path", "config"),
    ("storage_path", "storage"),
    ("public_path", "public"),
    ("database_path", "database"),
    ("lang_path", "lang"),
    ("base_path", ""),
];

fn extract_string_until_close_paren(s: &str) -> Option<String> {
    // The arg might be empty (`app_path()`) or a quoted string.
    let trimmed = s.trim_start();
    if trimmed.starts_with(')') {
        return Some(String::new());
    }
    extract_quoted_string(trimmed)
}

/// Match a single balanced `[...]` block starting at the first `[` in `s`
/// and return its inner contents. Returns `None` when no opening bracket
/// is found or the brackets aren't balanced (truncated config).
fn extract_array_inner(s: &str) -> Option<&str> {
    let trimmed = s.trim_start();
    let rest = trimmed.strip_prefix('[')?;
    let mut depth = 1usize;
    let mut in_single = false;
    let mut in_double = false;
    let mut prev_backslash = false;

    for (i, c) in rest.char_indices() {
        let escape_active = prev_backslash;
        prev_backslash = c == '\\' && !escape_active;
        match c {
            '\'' if !in_double && !escape_active => in_single = !in_single,
            '"' if !in_single && !escape_active => in_double = !in_double,
            '[' if !in_single && !in_double => depth += 1,
            ']' if !in_single && !in_double => {
                depth -= 1;
                if depth == 0 {
                    return Some(&rest[..i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Split a flat PHP array body on its top-level commas, respecting nested
/// brackets and quotes. Trailing commas produce empty segments which the
/// caller filters out.
fn split_top_level_commas(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth_bracket = 0i32;
    let mut depth_paren = 0i32;
    let mut in_single = false;
    let mut in_double = false;
    let mut prev_backslash = false;
    let mut last = 0usize;

    for (i, c) in s.char_indices() {
        let escape_active = prev_backslash;
        prev_backslash = c == '\\' && !escape_active;
        match c {
            '\'' if !in_double && !escape_active => in_single = !in_single,
            '"' if !in_single && !escape_active => in_double = !in_double,
            '[' if !in_single && !in_double => depth_bracket += 1,
            ']' if !in_single && !in_double => depth_bracket -= 1,
            '(' if !in_single && !in_double => depth_paren += 1,
            ')' if !in_single && !in_double => depth_paren -= 1,
            ',' if !in_single && !in_double && depth_bracket == 0 && depth_paren == 0 => {
                out.push(&s[last..i]);
                last = i + c.len_utf8();
            }
            _ => {}
        }
    }
    out.push(&s[last..]);
    out
}

/// Split a `key => value` PHP map entry on the first top-level `=>`.
fn split_on_fat_arrow(s: &str) -> Option<(&str, &str)> {
    let mut depth_bracket = 0i32;
    let mut depth_paren = 0i32;
    let mut in_single = false;
    let mut in_double = false;
    let mut prev_backslash = false;
    let bytes = s.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        let c = bytes[i] as char;
        let escape_active = prev_backslash;
        prev_backslash = c == '\\' && !escape_active;
        match c {
            '\'' if !in_double && !escape_active => in_single = !in_single,
            '"' if !in_single && !escape_active => in_double = !in_double,
            '[' if !in_single && !in_double => depth_bracket += 1,
            ']' if !in_single && !in_double => depth_bracket -= 1,
            '(' if !in_single && !in_double => depth_paren += 1,
            ')' if !in_single && !in_double => depth_paren -= 1,
            '=' if !in_single
                && !in_double
                && depth_bracket == 0
                && depth_paren == 0
                && bytes.get(i + 1) == Some(&b'>') =>
            {
                return Some((&s[..i], &s[i + 2..]));
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Collapse `\\` back to `\` so a parsed string like `App\\Livewire` becomes
/// the canonical `App\Livewire` form expected by PSR-4 lookups.
fn unescape_php_namespace(s: &str) -> String {
    s.replace("\\\\", "\\")
}

#[cfg(test)]
mod tests;
