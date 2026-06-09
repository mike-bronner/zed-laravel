//! Persistent on-disk cache for the Artisan command index.
//!
//! [`crate::command_index::build_command_index`] walks the *entire* project
//! and `vendor/` tree, reading every `*.php` file to find the handful that
//! `extends ...Command`. On a large Laravel install that's a real chunk of
//! I/O paid on every LSP cold start — even when nothing has changed since the
//! last run. This cache removes that tax: the first build writes the resolved
//! index to disk, and subsequent startups restore it instantly so
//! goto-definition and hover on command strings work without waiting for the
//! walk.
//!
//! It's a cold-start accelerator, not the source of truth. The full
//! [`crate::command_index::build_command_index`] still runs after the restore
//! (`rebuild_command_index` in `main.rs`) to pick up anything that changed
//! while the LSP was off — added/removed command classes included — and then
//! re-saves the refreshed index. The file watcher keeps the index (and this
//! cache) current while the server is running. This mirrors the design of
//! [`crate::pattern_disk_cache`].
//!
//! ## Format
//!
//! `bincode`-encoded `CacheFile { schema_version, entries }`, where each entry
//! is one command declaration plus the mtime of the file that declares it.
//! Binary because the load is on the startup critical path.
//!
//! ## Invalidation
//!
//! Per-file mtime, identical to [`crate::pattern_disk_cache`]. Each entry
//! stores the mtime observed when the command was indexed; on load we stat the
//! declaring file and only restore the entry if the mtime is byte-identical
//! (both `secs` and `nanos`). A changed, moved, or deleted command file drops
//! its entry — the full rebuild that follows the restore then re-discovers the
//! correct state. A schema-version bump discards the whole cache.
//!
//! ## Location
//!
//! The same XDG-compliant project-hash directory the pattern cache lives in,
//! alongside it as `command_cache.bin`. One cache per project root path.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::command_index::{CommandEntry, CommandIndex};

/// Bump this when the cached structures change — old caches are discarded on
/// read instead of risking deserialization into a struct that no longer
/// matches.
///
/// History:
///   v1 — initial command index cache.
const SCHEMA_VERSION: u32 = 1;

const CACHE_FILENAME: &str = "command_cache.bin";

/// On-disk envelope. The version field is checked before we try to decode the
/// entries, so a stale cache from an older build is dropped instead of
/// crashing the LSP.
#[derive(Serialize, Deserialize)]
struct CacheFile {
    schema_version: u32,
    entries: Vec<CachedEntry>,
}

/// One resolved command declaration plus the mtime we observed for its
/// declaring file when it was indexed. Both `secs` and `nanos` are stored
/// independently for byte-exact comparison — a `touch` that preserves the
/// second but bumps the nanos must still count as "changed."
#[derive(Serialize, Deserialize)]
struct CachedEntry {
    mtime_secs: u64,
    mtime_nanos: u32,
    entry: CommandEntry,
}

/// Where the cache file for `project_root` lives on disk. Returns `None` only
/// if the user's home directory can't be resolved — effectively infallible in
/// practice. Hashes the canonical root path so the location matches
/// [`crate::pattern_disk_cache`]'s per-project directory.
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

/// Decompose a `SystemTime` into `(secs, nanos)` relative to UNIX_EPOCH.
/// `None` if the time predates the epoch — shouldn't happen for real files,
/// but we drop the entry rather than panic.
fn split_mtime(time: SystemTime) -> Option<(u64, u32)> {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .map(|d| (d.as_secs(), d.subsec_nanos()))
}

/// Read the file's mtime as `(secs, nanos)`. `None` if the file doesn't exist
/// or isn't reachable — caller treats both as "no cache for this path."
fn read_mtime(path: &Path) -> Option<(u64, u32)> {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(split_mtime)
}

/// Restore the cached command index for `project_root`, validating every entry
/// against its declaring file's current mtime. Returns the rebuilt index, or
/// `None` when there's no cache, it's unreadable, or the schema doesn't match —
/// in every case the caller falls back to a full
/// [`crate::command_index::build_command_index`].
///
/// Stale entries (changed, moved, or deleted files) are silently skipped; the
/// returned index contains only the entries that still match disk, with the
/// App > Package > Framework priority merge re-applied via
/// [`CommandIndex::insert_entry`].
pub fn load_index(project_root: &Path) -> Option<CommandIndex> {
    let path = cache_file_path(project_root)?;
    let bytes = std::fs::read(&path).ok()?;

    let cache: CacheFile =
        match bincode::serde::decode_from_slice(&bytes, bincode::config::standard()) {
            Ok((c, _)) => c,
            Err(e) => {
                tracing::debug!("command_disk_cache: decode failed, ignoring: {}", e);
                return None;
            }
        };

    if cache.schema_version != SCHEMA_VERSION {
        tracing::info!(
            "command_disk_cache: schema mismatch (disk={}, current={}), ignoring",
            cache.schema_version,
            SCHEMA_VERSION
        );
        return None;
    }

    let mut index = CommandIndex::default();
    for cached in cache.entries {
        // Only restore the entry if its declaring file is still present and
        // unchanged. Anything else falls through to the full rebuild.
        match read_mtime(&cached.entry.file) {
            Some((s, n)) if s == cached.mtime_secs && n == cached.mtime_nanos => {
                index.insert_entry(cached.entry);
            }
            _ => {}
        }
    }
    Some(index)
}

/// Persist `index` to disk, stamping each command with its declaring file's
/// CURRENT mtime. Called after every successful build/rebuild. Safe to run on
/// the blocking pool — it's sync I/O.
///
/// Returns the number of entries written, or an error if the cache directory
/// or file couldn't be written. Errors are advisory — failing to persist
/// doesn't affect the live in-memory index.
pub fn save_index(index: &CommandIndex, project_root: &Path) -> Result<usize> {
    let cache_path =
        cache_file_path(project_root).context("could not resolve cache directory for project")?;

    let mut entries: Vec<CachedEntry> = Vec::with_capacity(index.len());
    for entry in index.entries() {
        // Stat the declaring file at save time so the stamped mtime reflects
        // what's on disk RIGHT NOW. A file modified since indexing simply
        // fails the next load's mtime check and gets re-parsed.
        let Some((secs, nanos)) = read_mtime(&entry.file) else {
            // File vanished between build and save — skip it; load_index would
            // drop a dangling entry anyway.
            continue;
        };
        entries.push(CachedEntry {
            mtime_secs: secs,
            mtime_nanos: nanos,
            entry: entry.clone(),
        });
    }

    let total = entries.len();
    let cache = CacheFile {
        schema_version: SCHEMA_VERSION,
        entries,
    };

    let encoded = bincode::serde::encode_to_vec(&cache, bincode::config::standard())
        .context("bincode encode failed")?;

    if let Some(parent) = cache_path.parent() {
        std::fs::create_dir_all(parent).context("could not create cache directory")?;
    }
    // Write to a temp file then rename — atomic on POSIX, so a crash mid-save
    // leaves the previous cache intact rather than a truncated one.
    let tmp = cache_path.with_extension("bin.tmp");
    std::fs::write(&tmp, &encoded).context("write tmp cache file failed")?;
    std::fs::rename(&tmp, &cache_path).context("rename tmp cache file failed")?;

    Ok(total)
}

#[cfg(test)]
mod tests;
