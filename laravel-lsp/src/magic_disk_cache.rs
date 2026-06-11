//! Persistent on-disk cache for the *resolved* magic-member index.
//!
//! The pattern disk cache ([`crate::pattern_disk_cache`]) already persists each
//! file's parsed patterns (including the raw `member_access_refs`) and class-
//! hierarchy nodes. What it does NOT persist is the **resolution** of those
//! member accesses into `<declaring_fqcn>#<member>` entries — the
//! `build_magic_member_entries` pass that resolves every `$user->email` against
//! the project's class hierarchy and view-variable index.
//!
//! On a large project that pass is the dominant warm cost (tens of seconds for
//! ~40k member accesses), and it re-ran on *every* startup even when not a
//! single file had changed. This cache stores the resolved entries so a clean
//! reload restores them instantly instead.
//!
//! ## Validity
//!
//! Magic-member resolution is **cross-file**: file A's `$user->email` resolves
//! through the User class hierarchy, which may live in file B. A per-file
//! mtime check alone is therefore insufficient — a change to B can invalidate
//! A's cached entry. The restore handles this granularly (#80): everything is
//! restored as-is, then the files the pattern cache flagged as changed are
//! re-resolved **along with their recorded blast radius** — dependents of
//! their classes and transitive descendants (from the persisted dependency
//! sets), plus Blade files of views they render (from the persisted render
//! sites). An earlier revision used an all-or-nothing guard instead,
//! re-resolving the whole project after any edit-then-restart.
//!
//! The cache is also re-saved (debounced) after live incremental refreshes,
//! so it tracks the in-memory index across a working session instead of
//! going stale at the first edit.
//!
//! ## Format
//!
//! `bincode`-encoded `CacheFile { schema_version, entries }`, in the same
//! XDG project-hash directory the pattern cache uses, as `magic_cache.bin`.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::symbol_index::MagicMemberEntry;
use crate::view_var_index::ViewRender;

/// Bump when `MagicMemberEntry`'s shape (or this file's layout) changes, so a
/// stale cache from an older build is discarded on read rather than
/// mis-deserialized. v2: call-form usages (#77) — a v1 cache holds only
/// property-form entries for unchanged files, which would leave scope /
/// finder references invisible until an unrelated edit; discard wholesale.
/// v3: per-file receiver dependencies + controller view renders (#80) — the
/// incremental save flow needs both alive after a cache-warm restart, where
/// the resolution pass that would otherwise populate them never runs.
const SCHEMA_VERSION: u32 = 3;

const CACHE_FILENAME: &str = "magic_cache.bin";

/// Everything the warm path needs to restore the magic-member system without
/// re-resolving: per-file resolved entries, per-file receiver dependencies
/// (for the incremental save flow's blast-radius lookups), and per-controller
/// view renders (to rebuild the persistent view-variable index).
#[derive(Default, Serialize, Deserialize)]
pub struct MagicCacheData {
    /// One entry per file that contributed resolved magic members or receiver
    /// dependencies: (path, resolved entries, attempted receiver FQCNs).
    pub entries: Vec<(PathBuf, Vec<MagicMemberEntry>, HashSet<String>)>,
    /// One entry per controller with `view()` render sites, for rebuilding
    /// the view-variable index on restore.
    pub view_renders: Vec<(PathBuf, Vec<ViewRender>)>,
}

#[derive(Serialize, Deserialize)]
struct CacheFile {
    schema_version: u32,
    data: MagicCacheData,
}

/// Where the magic cache for `project_root` lives. Mirrors the pattern cache's
/// per-project-hash directory so both live side by side. `None` only if the
/// user's home dir can't be resolved.
fn cache_file_path(project_root: &Path) -> Option<PathBuf> {
    let proj_dirs = ProjectDirs::from("org", "mike-bronner", "laravel-lsp")?;
    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let mut hasher = DefaultHasher::new();
    canonical.hash(&mut hasher);
    let project_hash = format!("{:x}", hasher.finish());
    Some(
        proj_dirs
            .cache_dir()
            .join(project_hash)
            .join(CACHE_FILENAME),
    )
}

/// Load the resolved magic-member data previously saved for `project_root`.
///
/// Returns `None` — meaning "no usable cache, resolve fresh" — for any of:
/// missing file, decode failure, or schema-version mismatch. The caller is
/// responsible for deciding the cache is still *valid* for the current project
/// state (see the module-level validity note); this just reads it back.
pub fn load(project_root: &Path) -> Option<MagicCacheData> {
    let path = cache_file_path(project_root)?;
    let bytes = std::fs::read(&path).ok()?;
    let (cache, _): (CacheFile, _) =
        bincode::serde::decode_from_slice(&bytes, bincode::config::standard()).ok()?;
    if cache.schema_version != SCHEMA_VERSION {
        return None;
    }
    Some(cache.data)
}

/// Persist the resolved magic-member `data` for `project_root`. Written via
/// a temp-file rename so a crash mid-write leaves the previous cache intact.
/// Returns the number of entry files written. Errors are advisory — failing
/// to persist doesn't affect the in-memory index.
pub fn save(project_root: &Path, data: &MagicCacheData) -> Result<usize> {
    let cache_path =
        cache_file_path(project_root).context("could not resolve cache directory for project")?;
    let total = data.entries.len();
    let cache = CacheFile {
        schema_version: SCHEMA_VERSION,
        data: MagicCacheData {
            entries: data.entries.clone(),
            view_renders: data.view_renders.clone(),
        },
    };
    let encoded = bincode::serde::encode_to_vec(&cache, bincode::config::standard())
        .context("bincode encode failed")?;
    if let Some(parent) = cache_path.parent() {
        std::fs::create_dir_all(parent).context("could not create cache directory")?;
    }
    let tmp = cache_path.with_extension("bin.tmp");
    std::fs::write(&tmp, &encoded).context("write tmp magic cache failed")?;
    std::fs::rename(&tmp, &cache_path).context("rename tmp magic cache failed")?;
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip the v3 schema — entries with deps, dep-only files, and
    /// view renders all survive save → load.
    #[test]
    fn v3_cache_round_trips_entries_deps_and_renders() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();

        let entry = MagicMemberEntry {
            fqcn: "App\\Models\\User".to_string(),
            member: "email".to_string(),
            line: 10,
            column: 4,
            end_column: 12,
        };
        let mut deps = std::collections::HashSet::new();
        deps.insert("App\\Models\\User".to_string());
        let mut dep_only = std::collections::HashSet::new();
        dep_only.insert("App\\Models\\Invoice".to_string());
        let mut vars = std::collections::HashMap::new();
        vars.insert("user".to_string(), "App\\Models\\User".to_string());

        let data = MagicCacheData {
            entries: vec![
                (
                    PathBuf::from("/proj/app/Http/Controllers/A.php"),
                    vec![entry.clone()],
                    deps.clone(),
                ),
                // Dep-only file: failed classifications recorded, no entries.
                (
                    PathBuf::from("/proj/app/Services/B.php"),
                    Vec::new(),
                    dep_only,
                ),
            ],
            view_renders: vec![(
                PathBuf::from("/proj/app/Http/Controllers/A.php"),
                vec![ViewRender {
                    view_name: "users.show".to_string(),
                    vars,
                }],
            )],
        };

        assert_eq!(save(root, &data).unwrap(), 2);
        let loaded = load(root).expect("cache must load back");
        assert_eq!(loaded.entries.len(), 2);
        let a = loaded
            .entries
            .iter()
            .find(|(p, _, _)| p.ends_with("A.php"))
            .unwrap();
        assert_eq!(a.1, vec![entry]);
        assert!(a.2.contains("App\\Models\\User"));
        let b = loaded
            .entries
            .iter()
            .find(|(p, _, _)| p.ends_with("B.php"))
            .unwrap();
        assert!(b.1.is_empty());
        assert!(b.2.contains("App\\Models\\Invoice"));
        assert_eq!(loaded.view_renders.len(), 1);
        assert_eq!(loaded.view_renders[0].1[0].view_name, "users.show");
    }
}
