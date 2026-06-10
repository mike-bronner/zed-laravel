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
//! through the User class hierarchy, which may live in file B. So a per-file
//! mtime check isn't sufficient — a change to B can invalidate A's cached
//! entry. The caller therefore only restores this cache when the project is
//! unchanged since the last save (the pattern cache validated every file, i.e.
//! zero files needed re-parsing). Any change → the caller rebuilds from
//! scratch. That keeps the cache correct by construction while making the
//! common reload-without-edits case instant.
//!
//! ## Format
//!
//! `bincode`-encoded `CacheFile { schema_version, entries }`, in the same
//! XDG project-hash directory the pattern cache uses, as `magic_cache.bin`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::symbol_index::MagicMemberEntry;

/// Bump when `MagicMemberEntry`'s shape (or this file's layout) changes, so a
/// stale cache from an older build is discarded on read rather than
/// mis-deserialized. v2: call-form usages (#77) — a v1 cache holds only
/// property-form entries for unchanged files, which would leave scope /
/// finder references invisible until an unrelated edit; discard wholesale.
const SCHEMA_VERSION: u32 = 2;

const CACHE_FILENAME: &str = "magic_cache.bin";

#[derive(Serialize, Deserialize)]
struct CacheFile {
    schema_version: u32,
    /// One entry per file that contributed resolved magic members, paired with
    /// that file's full resolved entry list.
    entries: Vec<(PathBuf, Vec<MagicMemberEntry>)>,
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

/// Load the resolved magic-member entries previously saved for `project_root`.
///
/// Returns `None` — meaning "no usable cache, resolve fresh" — for any of:
/// missing file, decode failure, or schema-version mismatch. The caller is
/// responsible for deciding the cache is still *valid* for the current project
/// state (see the module-level validity note); this just reads it back.
pub fn load(project_root: &Path) -> Option<Vec<(PathBuf, Vec<MagicMemberEntry>)>> {
    let path = cache_file_path(project_root)?;
    let bytes = std::fs::read(&path).ok()?;
    let (cache, _): (CacheFile, _) =
        bincode::serde::decode_from_slice(&bytes, bincode::config::standard()).ok()?;
    if cache.schema_version != SCHEMA_VERSION {
        return None;
    }
    Some(cache.entries)
}

/// Persist the resolved magic-member `entries` for `project_root`. Written via
/// a temp-file rename so a crash mid-write leaves the previous cache intact.
/// Returns the number of files written. Errors are advisory — failing to
/// persist doesn't affect the in-memory index.
pub fn save(project_root: &Path, entries: &[(PathBuf, Vec<MagicMemberEntry>)]) -> Result<usize> {
    let cache_path =
        cache_file_path(project_root).context("could not resolve cache directory for project")?;
    let total = entries.len();
    let cache = CacheFile {
        schema_version: SCHEMA_VERSION,
        entries: entries.to_vec(),
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
