//! Phase 3b — locate the file(s) backing a Blade `<x-...>` component and
//! validate a user-supplied new name for a Blade-component rename.
//!
//! Blade components can take two shapes on disk:
//!
//!   - **Anonymous** — just a `.blade.php` file under one of the configured
//!     component locations (default `resources/views/components/`). The tag
//!     name maps kebab-and-dotted to a path: `<x-forms.user-input>` →
//!     `resources/views/components/forms/user-input.blade.php`.
//!   - **Class-based** — same `.blade.php` plus a PHP class file under
//!     `app/View/Components/` (default). The class's name matches the
//!     tag's PascalCase form, and its namespace mirrors the file's
//!     directory path. Renaming the tag forces renaming the class file,
//!     the `class X extends Component` declaration, and (when the file
//!     moves across directories) the `namespace ...;` declaration too.
//!
//! Phase 3b refuses two flavors that need follow-up work:
//!   - Namespaced components (`<x-courier::alert>`) — would need to walk
//!     `Blade::componentNamespace(...)` registrations.
//!   - Aliased components (`Blade::component('x', 'y')`) — renaming the
//!     alias is conceptually different from renaming the underlying file.

use std::path::{Path, PathBuf};

use crate::naming;
use crate::salsa_impl::LaravelConfigData;

/// Reasons a new component name was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentNameError {
    Empty,
    ContainsSlash,
    ContainsDoubleDot,
    EmptySegment,
    HasExtension,
    InvalidCharacter(char),
    NamespacedNotSupported,
}

impl ComponentNameError {
    pub fn message(&self) -> String {
        match self {
            Self::Empty => "component name cannot be empty".to_string(),
            Self::ContainsSlash => "use dots instead of slashes (e.g., forms.input)".to_string(),
            Self::ContainsDoubleDot => "component name cannot contain '..'".to_string(),
            Self::EmptySegment => {
                "component name cannot have empty segments (no leading, trailing, \
                 or double dots)"
                    .to_string()
            }
            Self::HasExtension => "omit the file extension (write 'forms.input', \
                 not 'forms.input.blade.php')"
                .to_string(),
            Self::InvalidCharacter(c) => {
                format!(
                    "invalid character '{}' — component names may contain only \
                     letters, digits, hyphens, and underscores",
                    c
                )
            }
            Self::NamespacedNotSupported => {
                "renaming namespaced components (with '::') is not yet implemented".to_string()
            }
        }
    }
}

/// Validate a user-typed new component name. Mirrors view-name rules plus an
/// explicit refusal for `::` namespace prefixes.
pub fn validate_component_name(name: &str) -> Result<(), ComponentNameError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(ComponentNameError::Empty);
    }
    if trimmed.contains("::") {
        return Err(ComponentNameError::NamespacedNotSupported);
    }
    if trimmed.contains('/') || trimmed.contains('\\') {
        return Err(ComponentNameError::ContainsSlash);
    }
    if trimmed.ends_with(".blade.php") || trimmed.ends_with(".blade") || trimmed.ends_with(".php") {
        return Err(ComponentNameError::HasExtension);
    }
    for segment in trimmed.split('.') {
        if segment.is_empty() {
            return Err(ComponentNameError::EmptySegment);
        }
        if segment == ".." {
            return Err(ComponentNameError::ContainsDoubleDot);
        }
        for c in segment.chars() {
            if !c.is_alphanumeric() && c != '-' && c != '_' {
                return Err(ComponentNameError::InvalidCharacter(c));
            }
        }
    }
    Ok(())
}

/// All the files participating in a discovered Blade-component definition,
/// plus the in-file positions needing text edits when the class moves.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ComponentFiles {
    /// `.blade.php` view file. Always present for anonymous components;
    /// usually present for class-based ones too (Laravel convention).
    pub blade_file: Option<PathBuf>,
    /// PHP class file under the class-components root. Present only for
    /// class-based components.
    pub class_file: Option<PathBuf>,
    /// Position of the class name in `class_file` (the `Foo` in
    /// `class Foo extends Component`). Used to rewrite the class name
    /// when the rename changes it.
    pub class_declaration: Option<SourceSpan>,
    /// Position of the namespace path in `class_file`'s
    /// `namespace App\...;` line. Used when the rename moves the class
    /// across directories, which requires the namespace to track.
    pub namespace_declaration: Option<SourceSpan>,
}

/// A source-range span suitable for building a rename `TextEdit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSpan {
    pub file_path: PathBuf,
    /// 0-based row.
    pub line: u32,
    /// 0-based column of the first character of the span.
    pub start_column: u32,
    /// 0-based column one past the last character of the span.
    pub end_column: u32,
    /// The current source-text content of the span. Used by callers that
    /// need to confirm an expected value or skip a rename whose decl
    /// already matches the new name.
    pub current_text: String,
}

/// Discover the file(s) backing a Blade-component tag name. Returns `None`
/// when neither a `.blade.php` nor a class file exists for the name —
/// nothing to rename.
pub fn locate_component(name: &str, config: &LaravelConfigData) -> Option<ComponentFiles> {
    if name.contains("::") {
        // Namespaced components walk a different resolution path
        // (component_namespaces / view_namespaces). Phase 3b refuses
        // these at the validation layer; this defensive guard catches
        // any caller that bypasses validation.
        return None;
    }

    let blade_file = config
        .resolve_component_path(name)
        .into_iter()
        .find(|p| p.is_file());

    let class_file_path = conventional_class_file_path(name, config);
    let class_file = if class_file_path.is_file() {
        Some(class_file_path)
    } else {
        None
    };

    if blade_file.is_none() && class_file.is_none() {
        return None;
    }

    let (class_declaration, namespace_declaration) = match &class_file {
        Some(p) => {
            let content = std::fs::read_to_string(p).ok();
            match content {
                Some(c) => (
                    find_class_declaration(&c, p),
                    find_namespace_declaration(&c, p),
                ),
                None => (None, None),
            }
        }
        None => (None, None),
    };

    Some(ComponentFiles {
        blade_file,
        class_file,
        class_declaration,
        namespace_declaration,
    })
}

/// The conventional PHP class file path for a tag name, regardless of
/// whether the file actually exists. Used both to look up an existing file
/// and to compute the target path for a rename.
pub fn conventional_class_file_path(name: &str, config: &LaravelConfigData) -> PathBuf {
    config
        .root
        .join("app/View/Components")
        .join(naming::dotted_to_class_path(name))
        .with_extension("php")
}

/// The conventional PHP namespace for a class file at the given path, given
/// a project root. `app/View/Components/Forms/Input.php` →
/// `App\View\Components\Forms`. Returns the empty string if `class_file`
/// isn't under `{root}/app/`.
pub fn conventional_namespace_for(class_file: &Path, root: &Path) -> String {
    let app_root = root.join("app");
    let Ok(rel) = class_file.strip_prefix(&app_root) else {
        return String::new();
    };
    let parent = match rel.parent() {
        Some(p) => p,
        None => return "App".to_string(),
    };
    let mut segments = vec!["App".to_string()];
    for component in parent.components() {
        if let std::path::Component::Normal(s) = component {
            if let Some(s) = s.to_str() {
                segments.push(s.to_string());
            }
        }
    }
    segments.join("\\")
}

/// The Pascal-cased leaf segment of a dotted component name — what the
/// class itself is named. `forms.user-input` → `UserInput`.
pub fn class_name_for(component_name: &str) -> String {
    let leaf = component_name.split('.').next_back().unwrap_or("");
    naming::kebab_to_pascal(leaf)
}

/// Find the `class Foo` declaration in a PHP source string and return the
/// source span pointing at the class name. Matches the first top-level
/// class declaration — adequate for Laravel component files which always
/// have one class per file. Anonymous classes (`new class extends ...`)
/// don't match because they have no name to rewrite.
pub fn find_class_declaration(content: &str, file_path: &Path) -> Option<SourceSpan> {
    use regex::Regex;

    // Match: optional abstract/final, then `class`, whitespace, then the
    // class name. The capture group is the name itself, not the keyword.
    let re =
        Regex::new(r"(?m)^\s*(?:(?:abstract|final|readonly)\s+)*class\s+([A-Za-z_][A-Za-z0-9_]*)")
            .ok()?;
    let cap = re.captures(content)?;
    let name_match = cap.get(1)?;
    let (line, start_column) = byte_to_line_col(content, name_match.start());
    Some(SourceSpan {
        file_path: file_path.to_path_buf(),
        line,
        start_column,
        end_column: start_column + name_match.as_str().chars().count() as u32,
        current_text: name_match.as_str().to_string(),
    })
}

/// Find the `namespace App\Foo;` declaration in a PHP source string and
/// return the source span pointing at the namespace path (NOT including
/// `namespace`, the trailing `;`, or trailing whitespace).
pub fn find_namespace_declaration(content: &str, file_path: &Path) -> Option<SourceSpan> {
    use regex::Regex;

    let re = Regex::new(r"(?m)^\s*namespace\s+([A-Za-z_][A-Za-z0-9_\\]*)\s*;").ok()?;
    let cap = re.captures(content)?;
    let ns_match = cap.get(1)?;
    let (line, start_column) = byte_to_line_col(content, ns_match.start());
    Some(SourceSpan {
        file_path: file_path.to_path_buf(),
        line,
        start_column,
        end_column: start_column + ns_match.as_str().chars().count() as u32,
        current_text: ns_match.as_str().to_string(),
    })
}

fn byte_to_line_col(content: &str, byte_pos: usize) -> (u32, u32) {
    let mut line: u32 = 0;
    let mut col: u32 = 0;
    for (i, c) in content.char_indices() {
        if i >= byte_pos {
            return (line, col);
        }
        if c == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Compute the target `.blade.php` path for a rename, preserving the
/// component-locations base directory the original blade file sits under.
/// Mirrors [`crate::view_declaration_locator::compute_target_path`] but
/// uses the component-resolution logic so package and aliased base
/// directories are honored.
pub fn compute_blade_target_path(
    old_name: &str,
    new_name: &str,
    current_blade: &Path,
    config: &LaravelConfigData,
) -> Option<PathBuf> {
    let current_candidates = config.resolve_component_path(old_name);
    let new_candidates = config.resolve_component_path(new_name);
    let idx = current_candidates.iter().position(|p| p == current_blade)?;
    new_candidates.get(idx).cloned()
}

/// True if `path` lives under `{root}/vendor`. Mirrors
/// [`crate::view_declaration_locator::is_under_vendor`] — components
/// shipped by packages should not be moved by rename.
pub fn is_under_vendor(path: &Path, root: &Path) -> bool {
    path.starts_with(root.join("vendor"))
}

/// Reverse of [`locate_component`]'s blade-side lookup: given a
/// `.blade.php` path that lives under a configured `view_paths/components/`
/// directory, derive the dotted component name Laravel would resolve to it.
///
/// Returns `None` when:
///   - `path` doesn't end with `.blade.php`
///   - `path` doesn't sit under any configured `view_paths/components/`
///   - the resulting name fails validation
///
/// Used by the file-rename handler to compute the symbol name from the
/// path Zed is about to rename.
pub fn component_name_for_blade_path(path: &Path, config: &LaravelConfigData) -> Option<String> {
    let file_name = path.file_name()?.to_str()?;
    if !file_name.ends_with(".blade.php") {
        return None;
    }

    for base in &config.view_paths {
        let components_dir = base.join("components");
        let Ok(rel) = path.strip_prefix(&components_dir) else {
            continue;
        };
        let rel_str = rel.to_str()?;
        let stripped = rel_str.strip_suffix(".blade.php")?;
        let dotted = stripped.replace(['/', '\\'], ".");
        if validate_component_name(&dotted).is_ok() {
            return Some(dotted);
        }
    }
    None
}

#[cfg(test)]
mod tests;
