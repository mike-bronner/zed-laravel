//! Phase 3c — locate the file(s) backing a Livewire component and
//! validate a user-supplied new name for a Livewire-component rename.
//!
//! Builds on [`crate::livewire_resolver`] for the on-disk discovery
//! (which already handles all four shapes — V4 SFC, V4 MFC, V3 Class,
//! Volt — and Livewire-config-aware paths). This module adds the
//! rename-specific bits: name validation, target-path computation per
//! kind, and class/namespace declaration finders for V3 Class (needed
//! to rewrite `class Foo extends Component` and `namespace App\Livewire`
//! when the rename moves the class file).

use std::path::{Path, PathBuf};

use crate::livewire_config::LivewireConfig;
use crate::livewire_resolver::{resolve_component, LivewireComponent, LivewireComponentKind};
use crate::livewire_version::LivewireVersion;
use crate::naming;

/// Reasons a new Livewire-component name was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LivewireNameError {
    Empty,
    ContainsSlash,
    ContainsDoubleDot,
    EmptySegment,
    HasExtension,
    InvalidCharacter(char),
    NamespacedNotSupported,
}

impl LivewireNameError {
    pub fn message(&self) -> String {
        match self {
            Self::Empty => "Livewire component name cannot be empty".to_string(),
            Self::ContainsSlash => {
                "use dots instead of slashes (e.g., admin.user-list)".to_string()
            }
            Self::ContainsDoubleDot => "Livewire component name cannot contain '..'".to_string(),
            Self::EmptySegment => "Livewire component name cannot have empty segments \
                 (no leading, trailing, or double dots)"
                .to_string(),
            Self::HasExtension => "omit the file extension (write 'counter', not \
                 'counter.blade.php')"
                .to_string(),
            Self::InvalidCharacter(c) => format!(
                "invalid character '{}' — Livewire component names may contain only \
                 letters, digits, hyphens, and underscores",
                c
            ),
            Self::NamespacedNotSupported => {
                "renaming namespaced Livewire components (with '::') is not yet \
                 implemented"
                    .to_string()
            }
        }
    }
}

/// Validate a user-typed new Livewire-component name.
pub fn validate_livewire_name(name: &str) -> Result<(), LivewireNameError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(LivewireNameError::Empty);
    }
    if trimmed.contains("::") {
        return Err(LivewireNameError::NamespacedNotSupported);
    }
    if trimmed.contains('/') || trimmed.contains('\\') {
        return Err(LivewireNameError::ContainsSlash);
    }
    if trimmed.ends_with(".blade.php") || trimmed.ends_with(".blade") || trimmed.ends_with(".php") {
        return Err(LivewireNameError::HasExtension);
    }
    for segment in trimmed.split('.') {
        if segment.is_empty() {
            return Err(LivewireNameError::EmptySegment);
        }
        if segment == ".." {
            return Err(LivewireNameError::ContainsDoubleDot);
        }
        for c in segment.chars() {
            if !c.is_alphanumeric() && c != '-' && c != '_' {
                return Err(LivewireNameError::InvalidCharacter(c));
            }
        }
    }
    Ok(())
}

/// All the artifacts participating in a discovered Livewire component,
/// plus the in-file positions that may need text edits (V3 Class only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LivewireFiles {
    pub kind: LivewireComponentKind,
    /// Every file (and the parent dir for V4 MFC) that participates in
    /// the component. First entry is the "primary" file: the blade for
    /// SFC/Volt, the directory for MFC, the class file for V3 Class.
    pub paths: Vec<PathBuf>,
    /// `class X extends Component` source span in the class file
    /// (V3 Class only).
    pub class_declaration: Option<crate::component_declaration_locator::SourceSpan>,
    /// `namespace App\Livewire[\Sub];` source span in the class file
    /// (V3 Class only). Used when the rename moves the class file
    /// across directories.
    pub namespace_declaration: Option<crate::component_declaration_locator::SourceSpan>,
}

/// Locate the on-disk representation of a Livewire component plus any
/// in-file decl positions that participate in rename. Returns `None`
/// when no on-disk file matches the name (nothing to rename) or when
/// the name is namespaced (3c refuses these explicitly).
pub fn locate(
    name: &str,
    config: &LivewireConfig,
    version: LivewireVersion,
) -> Option<LivewireFiles> {
    if name.contains("::") {
        return None;
    }

    let component = resolve_component(name, config, version)?;

    let (class_declaration, namespace_declaration) =
        if component.kind == LivewireComponentKind::V3Class {
            class_decl_positions(&component)
        } else {
            (None, None)
        };

    Some(LivewireFiles {
        kind: component.kind,
        paths: component.paths,
        class_declaration,
        namespace_declaration,
    })
}

/// For a V3 Class component, the first `paths` entry is the class file.
/// Read it and locate the `class X` and `namespace ...;` source spans
/// using the shared Blade-component helpers (same regex pattern works
/// for both — class declarations have the same shape).
fn class_decl_positions(
    component: &LivewireComponent,
) -> (
    Option<crate::component_declaration_locator::SourceSpan>,
    Option<crate::component_declaration_locator::SourceSpan>,
) {
    let class_file = match component.paths.first() {
        Some(p) => p,
        None => return (None, None),
    };
    let Ok(content) = std::fs::read_to_string(class_file) else {
        return (None, None);
    };
    (
        crate::component_declaration_locator::find_class_declaration(&content, class_file),
        crate::component_declaration_locator::find_namespace_declaration(&content, class_file),
    )
}

/// Compute the target paths for a Livewire-component rename, preserving
/// the on-disk shape and the per-file conventions. Returns the new
/// `LivewireFiles` shape — caller pairs it with the old one and emits
/// one `FileRename` per (old, new) path pair (in array order).
pub fn compute_target_paths(
    old_name: &str,
    new_name: &str,
    current: &LivewireFiles,
    config: &LivewireConfig,
) -> Option<LivewireFiles> {
    let kind = current.kind;
    let new_paths = match kind {
        LivewireComponentKind::V4Sfc => {
            compute_v4_sfc_target(&current.paths, old_name, new_name, config)?
        }
        LivewireComponentKind::V4Mfc => {
            compute_v4_mfc_targets(&current.paths, old_name, new_name, config)?
        }
        LivewireComponentKind::V3Class => {
            compute_v3_class_targets(&current.paths, old_name, new_name, config)?
        }
        LivewireComponentKind::Volt => {
            compute_volt_target(&current.paths, old_name, new_name, config)?
        }
    };
    Some(LivewireFiles {
        kind,
        paths: new_paths,
        // Decl spans on the NEW shape don't exist yet (file isn't there).
        // The handler uses the OLD spans to construct text edits at the
        // OLD file's position; those edits travel together with the file
        // move in the same WorkspaceEdit.
        class_declaration: None,
        namespace_declaration: None,
    })
}

fn compute_v4_sfc_target(
    current: &[PathBuf],
    _old_name: &str,
    new_name: &str,
    config: &LivewireConfig,
) -> Option<Vec<PathBuf>> {
    let blade = current.first()?;
    let base = find_owning_location(blade, config)?;
    let segments: Vec<&str> = new_name.split('.').collect();
    let leaf = segments.last()?;
    let parents = &segments[..segments.len() - 1];
    let parent_dir = parents.iter().fold(base.clone(), |acc, seg| acc.join(seg));
    let has_emoji = blade
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.contains(naming::LIVEWIRE_EMOJI))
        .unwrap_or(false);
    let file_name = if has_emoji {
        format!("{}{}.blade.php", naming::LIVEWIRE_EMOJI, leaf)
    } else {
        format!("{}.blade.php", leaf)
    };
    Some(vec![parent_dir.join(file_name)])
}

fn compute_volt_target(
    current: &[PathBuf],
    _old_name: &str,
    new_name: &str,
    config: &LivewireConfig,
) -> Option<Vec<PathBuf>> {
    // Volt is always plain (no emoji prefix) by convention.
    let blade = current.first()?;
    let base = find_owning_location(blade, config)?;
    let segments: Vec<&str> = new_name.split('.').collect();
    let leaf = segments.last()?;
    let parents = &segments[..segments.len() - 1];
    let parent_dir = parents.iter().fold(base.clone(), |acc, seg| acc.join(seg));
    Some(vec![parent_dir.join(format!("{}.blade.php", leaf))])
}

fn compute_v4_mfc_targets(
    current: &[PathBuf],
    _old_name: &str,
    new_name: &str,
    config: &LivewireConfig,
) -> Option<Vec<PathBuf>> {
    // current[0] is the dir; current[1..] are the child files. Compute
    // the new dir, then re-emit each child under its new basename
    // (child basename always matches the emoji-stripped dir name per
    // Livewire's MultiFileParser convention).
    let old_dir = current.first()?;
    let base = find_owning_location(old_dir, config)?;
    let segments: Vec<&str> = new_name.split('.').collect();
    let leaf = segments.last()?;
    let parents = &segments[..segments.len() - 1];
    let parent_dir = parents.iter().fold(base.clone(), |acc, seg| acc.join(seg));
    let has_emoji = old_dir
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.contains(naming::LIVEWIRE_EMOJI))
        .unwrap_or(false);
    let new_dir_name = if has_emoji {
        format!("{}{}", naming::LIVEWIRE_EMOJI, leaf)
    } else {
        leaf.to_string()
    };
    let new_dir = parent_dir.join(new_dir_name);

    let mut out = vec![new_dir.clone()];
    for child in &current[1..] {
        let child_name = child.file_name()?.to_str()?;
        // Replace the OLD child basename stem with the new leaf. Child
        // names follow `{stem}.{ext}` where stem matches the dir's
        // emoji-stripped basename.
        let new_child_name = rename_mfc_child_basename(child_name, leaf)?;
        out.push(new_dir.join(new_child_name));
    }
    Some(out)
}

fn rename_mfc_child_basename(old_child_name: &str, new_leaf: &str) -> Option<String> {
    // `counter.php` → "counter" stem + ".php" suffix
    // `counter.blade.php` → "counter" stem + ".blade.php" suffix
    // `counter.global.css` → "counter" stem + ".global.css" suffix
    for ext in &[
        ".blade.php",
        ".global.css",
        ".test.php",
        ".php",
        ".js",
        ".css",
    ] {
        if let Some(_stem) = old_child_name.strip_suffix(ext) {
            return Some(format!("{}{}", new_leaf, ext));
        }
    }
    None
}

fn compute_v3_class_targets(
    current: &[PathBuf],
    _old_name: &str,
    new_name: &str,
    config: &LivewireConfig,
) -> Option<Vec<PathBuf>> {
    // current[0] is class file; current[1] (optional) is companion view.
    let _old_class = current.first()?;
    let new_class = config
        .class_path
        .join(naming::dotted_to_class_path(new_name))
        .with_extension("php");
    let mut out = vec![new_class];

    if current.len() > 1 {
        // Companion view path follows the kebab convention under view_path.
        let new_view = config
            .view_path
            .join(new_name.replace('.', "/"))
            .with_extension("blade.php");
        out.push(new_view);
    }
    Some(out)
}

/// Find which `component_locations` entry contains the given path.
/// Used to anchor target-path computation so the rename stays under the
/// same component-location root (project vs package boundaries are
/// preserved).
fn find_owning_location<'a>(path: &Path, config: &'a LivewireConfig) -> Option<&'a PathBuf> {
    config
        .component_locations
        .iter()
        .find(|loc| path.starts_with(loc))
}

/// True if `path` lives under `{root}/vendor`. Mirrors the view and
/// component variants — Livewire components shipped by packages should
/// not be renamed.
pub fn is_under_vendor(path: &Path, root: &Path) -> bool {
    path.starts_with(root.join("vendor"))
}

#[cfg(test)]
mod tests;
