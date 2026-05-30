//! Persistent on-disk cache for parsed file patterns.
//!
//! Without this, every Zed startup re-parses the entire project — even
//! when not a single file has changed. On a 40k-file Laravel project
//! that's a ~7-second tax on every editor reopen. With it: the first
//! startup parses everything and writes the cache to disk; subsequent
//! startups stat every project file (fast — pure metadata, no reads),
//! restore unchanged entries from the cache, and only re-parse the
//! files whose mtime has actually changed.
//!
//! ## Format
//!
//! `bincode`-encoded `CacheFile { schema_version, entries }`. Binary
//! because the load runs on the critical path of LSP startup and JSON
//! decode of 40k entries is ~10× slower than bincode.
//!
//! ## Invalidation
//!
//! Per-file mtime. Each entry stores the mtime at parse time; on load
//! we stat the path and only restore the entry if the mtime is byte-
//! identical (both `secs` and `nanos`). Anything else — file edited,
//! file deleted, vendor reinstalled, schema version bumped — falls
//! through to a fresh parse, which is exactly what warming already
//! handles.
//!
//! ## Location
//!
//! Same XDG-compliant project-hash directory the existing `cache.json`
//! lives in, alongside it as `pattern_cache.bin`. One cache per project
//! root path.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{Context, Result};
use dashmap::DashMap;
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::salsa_impl::ParsedPatternsData;

/// Bump this when the cache format changes — old caches are discarded
/// on read instead of risking deserialization into a struct that no
/// longer matches.
///
/// History:
///   v1 — initial pattern cache.
///   v2 — @livewire('name') directive form added to `livewire_refs`
///        so directive-form references are classified and indexed the
///        same as `<livewire:name>` tag form. Old caches lacked these
///        entries, breaking goto/hover/rename on the directive form.
const SCHEMA_VERSION: u32 = 2;

const CACHE_FILENAME: &str = "pattern_cache.bin";

/// On-disk envelope. The version field is checked before we try to
/// decode the entries map, so a stale cache from an older build just
/// gets dropped instead of crashing the LSP.
#[derive(Serialize, Deserialize)]
struct CacheFile {
    schema_version: u32,
    entries: HashMap<PathBuf, CachedEntry>,
}

/// One file's worth of cached patterns plus the mtime we observed when
/// the patterns were parsed. Both `secs` and `nanos` are stored
/// independently because we need byte-exact comparison: APFS gives us
/// nanosecond precision and we don't want a `touch` (which preserves
/// the second but changes the nanos) to slip past as "unchanged."
#[derive(Serialize, Deserialize)]
struct CachedEntry {
    mtime_secs: u64,
    mtime_nanos: u32,
    patterns: ParsedPatternsData,
}

/// Where the cache file for `project_root` lives on disk. Returns `None`
/// only if the user's home directory can't be resolved — every modern
/// OS we support has one, so this is effectively infallible in practice.
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
/// Returns `None` if the time predates the epoch — that shouldn't happen
/// for real files but we'd rather drop the entry than panic.
fn split_mtime(time: SystemTime) -> Option<(u64, u32)> {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .map(|d| (d.as_secs(), d.subsec_nanos()))
}

/// Read the file's mtime as `(secs, nanos)`. `None` if the file doesn't
/// exist or isn't reachable — caller treats both as "no cache for this
/// path."
fn read_mtime(path: &Path) -> Option<(u64, u32)> {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(split_mtime)
}

/// Load the on-disk cache, validate every entry against its current
/// file mtime, and insert the valid ones into the shared `pattern_cache`.
///
/// Returns `(restored, dropped)` — restored is what's now in the live
/// cache and won't be re-parsed during warming; dropped is what was on
/// disk but failed the mtime check (stale, missing, or schema mismatch).
///
/// Errors silently degrade to "no cache available": the calling warming
/// flow handles that fine — it just parses everything from scratch.
pub fn load_into(
    pattern_cache: &Arc<DashMap<PathBuf, (i32, Arc<ParsedPatternsData>)>>,
    project_root: &Path,
) -> (usize, usize) {
    let Some(path) = cache_file_path(project_root) else {
        return (0, 0);
    };
    let Ok(bytes) = std::fs::read(&path) else {
        // No cache file yet — first-ever startup for this project.
        return (0, 0);
    };

    let cache: CacheFile =
        match bincode::serde::decode_from_slice(&bytes, bincode::config::standard()) {
            Ok((c, _)) => c,
            Err(e) => {
                tracing::debug!("pattern_disk_cache: decode failed, ignoring: {}", e);
                return (0, 0);
            }
        };

    if cache.schema_version != SCHEMA_VERSION {
        tracing::info!(
            "pattern_disk_cache: schema mismatch (disk={}, current={}), ignoring",
            cache.schema_version,
            SCHEMA_VERSION
        );
        return (0, 0);
    }

    let mut restored = 0usize;
    let mut dropped = 0usize;
    for (path, entry) in cache.entries {
        // Stat the file. If it's gone, or its mtime differs from the
        // cached value, drop the entry — warming will re-parse it.
        match read_mtime(&path) {
            Some((s, n)) if s == entry.mtime_secs && n == entry.mtime_nanos => {
                // Fresh: rebuild the position index (we skipped persisting
                // it because it duplicates the Vec data) and insert.
                let mut patterns = entry.patterns;
                patterns.build_position_index();
                pattern_cache.insert(path, (0, Arc::new(patterns)));
                restored += 1;
            }
            _ => {
                dropped += 1;
            }
        }
    }
    (restored, dropped)
}

/// Persist every entry currently in `pattern_cache` to disk, stamped
/// with each file's CURRENT mtime. Called at the end of warming; safe
/// to run on the tokio blocking pool because the work is sync I/O.
///
/// Returns the number of entries written, or an error if we couldn't
/// touch the cache directory or write the file. Errors are advisory —
/// failing to persist doesn't break the in-memory cache.
pub fn save_from(
    pattern_cache: &Arc<DashMap<PathBuf, (i32, Arc<ParsedPatternsData>)>>,
    project_root: &Path,
) -> Result<usize> {
    let cache_path =
        cache_file_path(project_root).context("could not resolve cache directory for project")?;

    let mut entries: HashMap<PathBuf, CachedEntry> = HashMap::with_capacity(pattern_cache.len());

    // Walk the live DashMap and copy out the data we need. We stat each
    // path at save time (rather than trust an mtime we computed earlier)
    // so the cache reflects what's on disk RIGHT NOW. A file that's been
    // modified since parsing won't get a stale mtime stamped against
    // potentially-stale parsed data — the next load will see the actual
    // mtime mismatch and re-parse.
    for entry in pattern_cache.iter() {
        let path = entry.key();
        let (_, ref patterns) = *entry.value();
        let Some((secs, nanos)) = read_mtime(path) else {
            // File vanished between parse and save — skip it. Saving a
            // dangling entry would just waste space; load_into would
            // drop it anyway.
            continue;
        };
        entries.insert(
            path.clone(),
            CachedEntry {
                mtime_secs: secs,
                mtime_nanos: nanos,
                // ParsedPatternsData: Clone is cheap (Arc bumps).
                patterns: (**patterns).clone(),
            },
        );
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
    // Write to a temp file then rename — atomic on POSIX, so a crash
    // mid-save leaves the previous cache intact rather than a truncated
    // one we'd fail to decode on next load.
    let tmp = cache_path.with_extension("bin.tmp");
    std::fs::write(&tmp, &encoded).context("write tmp cache file failed")?;
    std::fs::rename(&tmp, &cache_path).context("rename tmp cache file failed")?;

    Ok(total)
}

#[cfg(test)]
mod tests;
