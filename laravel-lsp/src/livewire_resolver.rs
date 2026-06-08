//! Helpers for locating the PHP source backing a Blade view's Livewire component.
//!
//! Livewire ships four discoverable component shapes the rename machinery
//! needs to distinguish:
//!
//!   - **V4 SFC** (single-file) — `⚡{leaf}.blade.php` containing an inline
//!     `new class extends Component`. The `⚡` filename prefix is the on-disk
//!     marker that disambiguates from Volt.
//!   - **V4 MFC** (multi-file) — `⚡{leaf}/` directory containing
//!     `{leaf}.php`, `{leaf}.blade.php`, and optional `.js` / `.css` /
//!     `.global.css` siblings. Livewire's discovery requires child basenames
//!     to match the emoji-stripped directory name; renaming the directory
//!     forces renaming every child.
//!   - **V3 Class-based** — a class file under `class_path` paired with a
//!     view under `view_path`. The v3 carry-over shape, still supported in
//!     v4 (`'make_command.type' => 'class'`).
//!   - **Volt** — a plain `{leaf}.blade.php` (no emoji) whose front-matter
//!     PHP block uses Volt's functional API (`state()`, `action()`,
//!     `computed()`, ...) or extends `Livewire\Volt\Component`.
//!
//! [`resolve_component`] picks the right shape for a given component name by
//! walking the configured locations / namespaces and returning the first
//! match. The lower-level helpers ([`mfc_sibling`], [`blade_contains_inline_class`])
//! are kept for hover/goto callers that don't need the full resolver.
//!
//! Pure path-based, side-effect-free filesystem checks — testable from a
//! tempdir without spinning up a Backend.

use std::path::{Path, PathBuf};

use crate::livewire_config::LivewireConfig;
use crate::livewire_version::LivewireVersion;
use crate::naming;

/// If the blade file has a sibling `.php` with the same stem and that sibling
/// contains an inline Livewire component class signature, return the sibling
/// path. Returns `None` if there is no sibling, the sibling is unreadable, or
/// the sibling exists but doesn't carry the signature.
pub fn mfc_sibling(blade_path: &Path) -> Option<PathBuf> {
    let name = blade_path.file_name()?.to_str()?;
    let stem = name.strip_suffix(".blade.php")?;
    let sibling = blade_path.with_file_name(format!("{}.php", stem));
    if !sibling.exists() {
        return None;
    }
    let content = std::fs::read_to_string(&sibling).ok()?;
    if crate::php_class::detect_inline_livewire_class(&content) {
        Some(sibling)
    } else {
        None
    }
}

/// True when the blade file at `blade_path` contains an inline Livewire
/// `new class extends Component` declaration (single-file component pattern).
pub fn blade_contains_inline_class(blade_path: &Path) -> bool {
    std::fs::read_to_string(blade_path)
        .ok()
        .map(|content| crate::php_class::detect_inline_livewire_class(&content))
        .unwrap_or(false)
}

/// Given a line of text and a 0-based column position, identify what Blade
/// variable (and optional property access) the cursor is on. Returns:
///   - `("form", None)` for cursor anywhere on `$form`
///   - `("form", Some("name"))` for cursor on `name` in `$form->name`
///   - `("", None)` for cursor right after a bare `$` (used by `$` trigger completion)
///   - `None` if the cursor isn't on any `$variable` token
///
/// Used by the hover handler, the goto-definition fallback, and the `$`
/// trigger completion path.
pub fn extract_blade_variable_at_cursor(
    line: &str,
    cursor_col: u32,
) -> Option<(String, Option<String>)> {
    let cursor = cursor_col as usize;
    if cursor > line.len() {
        return None;
    }

    let bytes = line.as_bytes();

    // Walk back to find the start of the current identifier.
    let mut ident_start = cursor;
    while ident_start > 0 {
        let c = bytes[ident_start - 1];
        if c.is_ascii_alphanumeric() || c == b'_' {
            ident_start -= 1;
        } else {
            break;
        }
    }

    // Walk forward to find the end of the current identifier.
    let mut ident_end = cursor;
    while ident_end < bytes.len() {
        let c = bytes[ident_end];
        if c.is_ascii_alphanumeric() || c == b'_' {
            ident_end += 1;
        } else {
            break;
        }
    }

    if ident_start >= ident_end {
        // Cursor not on any identifier; handle the bare-`$` trigger case
        // (cursor sits immediately after a `$` with no identifier yet).
        if ident_start > 0 && bytes[ident_start - 1] == b'$' {
            return Some((String::new(), None));
        }
        return None;
    }

    let ident = &line[ident_start..ident_end];

    // Case A: cursor on the variable itself (preceded by `$`).
    if ident_start > 0 && bytes[ident_start - 1] == b'$' {
        return Some((ident.to_string(), None));
    }

    // Case B: cursor on a property name preceded by `->`. Walk back from
    // `ident_start` past `->` and look for the originating `$variable`.
    if ident_start >= 2 && &line[ident_start - 2..ident_start] == "->" {
        let mut probe = ident_start - 2;
        while probe > 0 {
            let c = bytes[probe - 1];
            if c.is_ascii_alphanumeric() || c == b'_' {
                probe -= 1;
            } else {
                break;
            }
        }
        if probe < ident_start - 2 && probe > 0 && bytes[probe - 1] == b'$' {
            let var_name = &line[probe..ident_start - 2];
            return Some((var_name.to_string(), Some(ident.to_string())));
        }
    }

    None
}

// ============================================================================
// Component resolution (Phase 3)
// ============================================================================

/// The on-disk shape of a discovered Livewire component. Phase 3 rename
/// dispatches on this — each kind drives a different rewriter (SFC moves one
/// file, MFC moves a directory plus N children, V3 moves a class + view,
/// Volt moves one view file).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LivewireComponentKind {
    V4Sfc,
    V4Mfc,
    V3Class,
    Volt,
}

/// A resolved Livewire component — the kind plus every file that belongs to
/// it. `paths` is what rename consumes: every entry is either a candidate for
/// a `RenameFile` op or (for V3Class) a class file whose `class X extends
/// Component` declaration also needs an in-file `TextEdit`.
///
/// For V4 MFC the first entry is the directory itself; child files follow.
/// Rename emits a `RenameFile` for each in order (directory first so the
/// child paths in subsequent ops are relative to the new dir name on
/// clients that apply operations sequentially).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LivewireComponent {
    pub kind: LivewireComponentKind,
    pub paths: Vec<PathBuf>,
}

/// Resolve a Livewire component tag name (e.g. `admin.user-list` or
/// `pages::dashboard`) to the concrete on-disk component, if any.
///
/// Returns `None` when the name doesn't match anything Livewire would
/// actually discover at runtime. The caller (Phase 3c rename, Phase 3d
/// file-rename, hover, goto-definition) then gives up gracefully.
///
/// Resolution order, mirroring Livewire 4's discovery preference:
///   1. V4 SFC — `⚡{leaf}.blade.php` under each candidate base
///   2. V4 MFC — `⚡{leaf}/` directory with the required `{leaf}.php` child
///   3. Volt — plain `{leaf}.blade.php` with a Volt front-matter signature
///   4. V3 Class — `{class_path}/{Pascal}/{Pascal}.php` (skipped when the
///      name is namespaced — class lookups don't honor `<livewire:pages::...>`)
///
/// V3 projects (per `version`) skip the V4 SFC/MFC and Volt checks and go
/// straight to class-based resolution. Unknown-version projects try all
/// four — better to over-discover than to miss a component.
pub fn resolve_component(
    name: &str,
    config: &LivewireConfig,
    version: LivewireVersion,
) -> Option<LivewireComponent> {
    let (namespace, bare) = split_namespace(name);
    let segments: Vec<&str> = bare.split('.').collect();
    let leaf = *segments.last()?;
    if leaf.is_empty() {
        return None;
    }
    let parents = &segments[..segments.len() - 1];
    let sub = parents_to_path(parents);

    let base_dirs: Vec<&PathBuf> = match namespace {
        Some(ns) => match config.component_namespaces.get(ns) {
            Some(p) => vec![p],
            None => return None,
        },
        None => config.component_locations.iter().collect(),
    };

    let try_v4 = matches!(version, LivewireVersion::V4 | LivewireVersion::Unknown);

    for base in &base_dirs {
        let parent_dir = if sub.as_os_str().is_empty() {
            (*base).clone()
        } else {
            base.join(&sub)
        };

        if try_v4 {
            if let Some(c) = try_v4_sfc(&parent_dir, leaf) {
                return Some(c);
            }
            if let Some(c) = try_v4_mfc(&parent_dir, leaf) {
                return Some(c);
            }
            if let Some(c) = try_volt(&parent_dir, leaf) {
                return Some(c);
            }
        }
    }

    // V3 class-based fallback. Class lookups don't go through namespaces —
    // those are a view-co-located concept. So only the un-namespaced names
    // ever fall through here.
    if namespace.is_none() {
        if let Some(c) = try_v3_class(bare, config) {
            return Some(c);
        }
    }

    None
}

/// Reverse of [`resolve_component`]: given a component file path, return the
/// Livewire component name it backs (`counter`, `admin.user-list`,
/// `pages::dashboard`), or `None` if the path isn't a Livewire component.
///
/// Works by *guess and verify*: derive candidate names from the path under the
/// configured roots (class path, component locations, namespace dirs), then
/// confirm each by running [`resolve_component`] forward and checking it points
/// back at this file. Every shape/convention nuance (v3 class, v4 SFC/MFC,
/// Volt, `⚡` prefixes, kebab-casing) stays in the forward resolver — a wrong
/// guess simply fails verification, so this never returns a bogus name (at
/// worst it returns `None` and the caller shows no lens).
pub fn livewire_name_for_path(
    path: &Path,
    config: &LivewireConfig,
    version: LivewireVersion,
) -> Option<String> {
    let target = crate::route_discovery::normalize_path(path);
    for name in candidate_livewire_names(path, config) {
        if let Some(component) = resolve_component(&name, config, version) {
            if component
                .paths
                .iter()
                .any(|p| crate::route_discovery::normalize_path(p) == target)
            {
                return Some(name);
            }
        }
    }
    None
}

/// Candidate component names for `path`, one per configured root it falls
/// under. Over-generation is safe — [`livewire_name_for_path`] verifies each.
fn candidate_livewire_names(path: &Path, config: &LivewireConfig) -> Vec<String> {
    let mut out = Vec::new();
    let is_blade = path.to_string_lossy().ends_with(".blade.php");
    let is_php = !is_blade && path.extension().and_then(|e| e.to_str()) == Some("php");

    // V3 class: a non-blade `.php` under the class path → kebab-dotted class
    // path relative to the root.
    if is_php {
        if let Ok(rel) = path.strip_prefix(&config.class_path) {
            if let Some(stem) = rel.to_str().and_then(|s| s.strip_suffix(".php")) {
                if let Some(name) = kebab_dotted(stem.split(['/', '\\']), "") {
                    out.push(name);
                }
            }
        }
    }

    // V4 SFC / MFC / Volt under a component location (+ namespaced variants).
    for loc in &config.component_locations {
        if let Ok(rel) = path.strip_prefix(loc) {
            if let Some(name) = name_from_component_rel(rel, is_blade) {
                out.push(name);
            }
        }
    }
    for (ns, dir) in &config.component_namespaces {
        if let Ok(rel) = path.strip_prefix(dir) {
            if let Some(name) = name_from_component_rel(rel, is_blade) {
                out.push(format!("{ns}::{name}"));
            }
        }
    }
    out
}

/// Derive a component name from a path relative to a component location.
///   - file inside a `⚡leaf/` dir (MFC, `.php` or `.blade.php`) → the `⚡leaf`
///     dir supplies the leaf, the trailing file is dropped.
///   - `[⚡]leaf.blade.php` (SFC or Volt) → dir segments + emoji-stripped leaf.
fn name_from_component_rel(rel: &Path, is_blade: bool) -> Option<String> {
    let s = rel.to_str()?;
    let segs: Vec<&str> = s.split(['/', '\\']).collect();
    if segs.is_empty() {
        return None;
    }
    // MFC: the file lives inside a `⚡leaf/` directory.
    if segs.len() >= 2 && naming::has_emoji(segs[segs.len() - 2]) {
        let leaf_dir = segs[segs.len() - 2];
        return kebab_dotted(segs[..segs.len() - 2].iter().copied(), leaf_dir);
    }
    // SFC / Volt: a `.blade.php` file directly under the location tree.
    if is_blade {
        let (last, parents) = segs.split_last()?;
        let leaf = last.strip_suffix(".blade.php").unwrap_or(last);
        return kebab_dotted(parents.iter().copied(), leaf);
    }
    None
}

/// Kebab-case each segment (PascalCase or emoji-prefixed) and dot-join. When
/// `leaf` is empty the last `parents` segment is treated as the leaf (used for
/// the class-path form where the whole relative path is segments).
fn kebab_dotted<'a>(parents: impl Iterator<Item = &'a str>, leaf: &str) -> Option<String> {
    let mut parts: Vec<String> = parents
        .map(|p| naming::pascal_to_kebab(naming::strip_emoji(p)))
        .collect();
    if !leaf.is_empty() {
        parts.push(naming::pascal_to_kebab(naming::strip_emoji(leaf)));
    }
    parts.retain(|p| !p.is_empty());
    (!parts.is_empty()).then(|| parts.join("."))
}

// ---------- format-specific lookups ----------

fn try_v4_sfc(parent_dir: &Path, leaf: &str) -> Option<LivewireComponent> {
    let candidate = parent_dir.join(format!("{}{}.blade.php", naming::LIVEWIRE_EMOJI, leaf));
    if candidate.is_file() {
        return Some(LivewireComponent {
            kind: LivewireComponentKind::V4Sfc,
            paths: vec![candidate],
        });
    }
    None
}

fn try_v4_mfc(parent_dir: &Path, leaf: &str) -> Option<LivewireComponent> {
    let dir = parent_dir.join(format!("{}{}", naming::LIVEWIRE_EMOJI, leaf));
    if !dir.is_dir() {
        return None;
    }
    let class_file = dir.join(format!("{}.php", leaf));
    if !class_file.is_file() {
        // Bare directory without the required class file — not an MFC.
        return None;
    }
    Some(LivewireComponent {
        kind: LivewireComponentKind::V4Mfc,
        paths: mfc_paths(&dir, leaf),
    })
}

fn try_volt(parent_dir: &Path, leaf: &str) -> Option<LivewireComponent> {
    let candidate = parent_dir.join(format!("{}.blade.php", leaf));
    if !candidate.is_file() {
        return None;
    }
    if !blade_contains_volt_signature(&candidate) {
        return None;
    }
    Some(LivewireComponent {
        kind: LivewireComponentKind::Volt,
        paths: vec![candidate],
    })
}

fn try_v3_class(bare: &str, config: &LivewireConfig) -> Option<LivewireComponent> {
    let class_path = config
        .class_path
        .join(naming::dotted_to_class_path(bare))
        .with_extension("php");
    if !class_path.is_file() {
        return None;
    }
    let mut paths = vec![class_path];
    // Companion view file — kebab path under view_path. Optional: a class-
    // based component can return its own view via render(), in which case
    // there's no canonical view file. We include the conventional one when
    // it exists so rename catches it.
    let view_file = config
        .view_path
        .join(bare.replace('.', "/"))
        .with_extension("blade.php");
    if view_file.is_file() {
        paths.push(view_file);
    }
    Some(LivewireComponent {
        kind: LivewireComponentKind::V3Class,
        paths,
    })
}

// ---------- helpers ----------

fn split_namespace(name: &str) -> (Option<&str>, &str) {
    if let Some(pos) = name.find("::") {
        (Some(&name[..pos]), &name[pos + 2..])
    } else {
        (None, name)
    }
}

fn parents_to_path(parents: &[&str]) -> PathBuf {
    let mut p = PathBuf::new();
    for seg in parents {
        p.push(seg);
    }
    p
}

/// Enumerate the files inside an MFC directory in the order rename should
/// emit them: the directory itself first, then each child basename that
/// exists. Mirrors Livewire's `MultiFileParser::parse` expectations — class,
/// view, optional js, optional css, optional global.css.
fn mfc_paths(dir: &Path, leaf: &str) -> Vec<PathBuf> {
    let mut paths = vec![dir.to_path_buf()];
    for ext in MFC_CHILD_EXTENSIONS {
        let child = dir.join(format!("{}.{}", leaf, ext));
        if child.is_file() {
            paths.push(child);
        }
    }
    paths
}

const MFC_CHILD_EXTENSIONS: &[&str] = &["php", "blade.php", "js", "css", "global.css", "test.php"];

/// True when the Blade file's front-matter PHP block carries a Volt
/// signature — either an explicit `Livewire\Volt\Component` import/extends,
/// or a bare functional-API call (`state()`, `action()`, `computed()`,
/// `mount()`, `usesPagination()`, ...). Permissive by design — false
/// positives are harmless (we'd treat a Volt-like file as Volt) while
/// false negatives would silently drop the file from rename coverage.
pub fn blade_contains_volt_signature(blade_path: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(blade_path) else {
        return false;
    };
    source_contains_volt_signature(&content)
}

/// Same Volt-signature check as [`blade_contains_volt_signature`] but on
/// already-read source — lets callers that already hold the file contents avoid
/// a second read.
pub fn source_contains_volt_signature(content: &str) -> bool {
    let window = front_matter_window(content);
    if window.contains("Volt\\Component") || window.contains("volt\\component") {
        return true;
    }
    VOLT_FUNCTIONAL_CALLS
        .iter()
        .any(|needle| window.contains(needle))
}

/// Volt files put their PHP in a front-matter block — usually the first few
/// dozen lines. Scanning only that window keeps the check cheap and avoids
/// matching the same call words inside the Blade body.
fn front_matter_window(content: &str) -> &str {
    const WINDOW_BYTES: usize = 4096;
    let end = WINDOW_BYTES.min(content.len());
    // Snap `end` back to a UTF-8 char boundary so we never slice mid-codepoint.
    let mut adjusted = end;
    while adjusted > 0 && !content.is_char_boundary(adjusted) {
        adjusted -= 1;
    }
    &content[..adjusted]
}

const VOLT_FUNCTIONAL_CALLS: &[&str] = &[
    "state(",
    "action(",
    "computed(",
    "mount(",
    "rendering(",
    "rendered(",
    "usesPagination(",
    "usesFileUploads(",
    "form(",
];

#[cfg(test)]
mod tests;
