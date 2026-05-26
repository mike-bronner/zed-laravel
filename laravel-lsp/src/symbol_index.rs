//! Inverted symbol index — turns `find-references` from O(N files) into
//! O(1) by name, plus the size of the answer.
//!
//! ## What it stores
//!
//! Two parallel maps, kept in sync as files are indexed and changed:
//!
//! - **forward**: `(SymbolKind, name) → Vec<ReferenceLocationData>`
//!   The hot path. `find_references` consults this and returns the
//!   match list directly.
//!
//! - **by_file**: `PathBuf → Vec<SymbolKey>` listing which symbol keys
//!   this file contributed entries to. When a file is edited or
//!   deleted, this lets us yank just that file's entries out of
//!   `forward` without scanning the whole index.
//!
//! Plus a small dirty-files set so we can defer re-parsing edited
//! files until the next `find_references` actually needs them — same
//! pattern as Salsa's lazy invalidation.
//!
//! ## Memory cost
//!
//! On a 60k-file Laravel project: ~600k forward entries × ~80 bytes
//! (SymbolKey + PathBuf + 3 u32) ≈ 50MB. Reasonable for an LSP that's
//! the user's primary navigation tool. Could be cut ~2-3× by interning
//! the symbol names into a shared string pool — left as future work.
//!
//! ## Correctness invariants
//!
//! 1. For every `(key, locs)` in `forward`, every `loc.file_path`
//!    appears as a key in `by_file`, and `key` is in that file's
//!    `by_file` list.
//! 2. `remove_file(p)` followed by `insert_file(p, patterns)` is a
//!    no-op iff `patterns` produces the same set of symbol entries
//!    as the previous insertion for `p`.
//! 3. `take_dirty()` is idempotent on the receiver: calling it twice
//!    in a row returns an empty Vec the second time.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::salsa_impl::{ParsedPatternsData, ReferenceLocationData, SymbolRefData};

/// Discriminant matching the nine symbol kinds find-references handles.
/// `Copy + Hash + Eq` so it composes into `SymbolKey` cheaply.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum SymbolKind {
    View,
    Route,
    Config,
    Translation,
    Env,
    Component,
    Livewire,
    Middleware,
    Binding,
}

/// Composite key for the forward map. Names are heap-allocated Strings
/// (rather than `&str`) because the index outlives any source file the
/// names came from — symbols persist across file edits in the cache.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
struct SymbolKey {
    kind: SymbolKind,
    name: String,
}

/// The inverted index itself. Owned by the actor; never shared (reads
/// and writes both go through the actor's single-threaded queue, so
/// internal Mutexes would be redundant).
#[derive(Default, Debug)]
pub struct SymbolIndex {
    forward: HashMap<SymbolKey, Vec<ReferenceLocationData>>,
    by_file: HashMap<PathBuf, Vec<SymbolKey>>,
    /// Files whose patterns may have changed since their index entries
    /// were last refreshed. Processed lazily in `find_references` so
    /// hot-path edits don't pay re-indexing cost up front.
    dirty: HashSet<PathBuf>,
}

impl SymbolIndex {
    /// Add every pattern entry from `patterns` into the forward map,
    /// and record the resulting keys in `by_file` so we can find them
    /// again for removal.
    ///
    /// Idempotent w.r.t. the dirty set: caller is responsible for
    /// pairing this with `remove_file` if the file was already indexed.
    pub fn insert_file(&mut self, path: &Path, patterns: &ParsedPatternsData) {
        let mut keys: Vec<SymbolKey> = Vec::new();

        // One macro arm per pattern collection on `ParsedPatternsData`.
        // The `$name` field name varies between collections (it's
        // `name` for some, `key` for config/translation, `path` for…
        // actually no, we currently only index the nine listed below
        // because they're what `find-references` classifies).
        macro_rules! ingest {
            ($field:ident, $kind:expr, $name_field:ident) => {
                for p in &patterns.$field {
                    let key = SymbolKey {
                        kind: $kind,
                        name: p.$name_field.clone(),
                    };
                    let loc = ReferenceLocationData {
                        file_path: path.to_path_buf(),
                        line: p.line,
                        column: p.column,
                        end_column: p.end_column,
                    };
                    self.forward.entry(key.clone()).or_default().push(loc);
                    keys.push(key);
                }
            };
        }
        ingest!(views, SymbolKind::View, name);
        ingest!(route_refs, SymbolKind::Route, name);
        ingest!(config_refs, SymbolKind::Config, key);
        ingest!(translation_refs, SymbolKind::Translation, key);
        ingest!(env_refs, SymbolKind::Env, name);
        ingest!(components, SymbolKind::Component, name);
        ingest!(livewire_refs, SymbolKind::Livewire, name);
        ingest!(middleware_refs, SymbolKind::Middleware, name);
        ingest!(binding_refs, SymbolKind::Binding, name);

        if !keys.is_empty() {
            self.by_file.insert(path.to_path_buf(), keys);
        }
    }

    /// Yank every entry this file contributed out of `forward`, using
    /// the reverse map to avoid scanning unrelated keys. Empty-bucket
    /// keys are dropped from `forward` to keep memory bounded as files
    /// churn over time.
    pub fn remove_file(&mut self, path: &Path) {
        let keys = match self.by_file.remove(path) {
            Some(k) => k,
            None => return,
        };
        for key in keys {
            if let Some(entries) = self.forward.get_mut(&key) {
                entries.retain(|loc| loc.file_path.as_path() != path);
                if entries.is_empty() {
                    self.forward.remove(&key);
                }
            }
        }
    }

    /// Mark a path as needing refresh. Cheap (just a HashSet insert);
    /// the actual re-parse work happens later in `take_dirty()`.
    pub fn mark_dirty(&mut self, path: &Path) {
        let was_new = self.dirty.insert(path.to_path_buf());
        // Diagnostic: log every 100 marks so we can see the growth shape
        // without flooding. Phase 3c investigation surfaced 11k+ dirty
        // entries at find_references time; mark_dirty's only known
        // caller is `handle_update_file`, so we want to know how often
        // that's actually firing.
        if was_new && self.dirty.len().is_multiple_of(100) {
            tracing::debug!(
                "symbol_index.dirty grew to {} (last added: {})",
                self.dirty.len(),
                path.display()
            );
        }
    }

    /// Drain and return the dirty set so the caller can re-index each
    /// path. Returned in unspecified order. After this call the set is
    /// empty.
    ///
    /// We return paths instead of doing the work inline because the
    /// re-indexing needs `handle_get_patterns` on the actor, which
    /// would conflict with `&mut self.symbol_index` borrow rules. The
    /// caller can sequence them at the actor level.
    pub fn take_dirty(&mut self) -> Vec<PathBuf> {
        self.dirty.drain().collect()
    }

    /// Drop everything. Used when we want to rebuild from scratch
    /// (typically right after warming finishes).
    pub fn clear(&mut self) {
        self.forward.clear();
        self.by_file.clear();
        self.dirty.clear();
    }

    /// Look up every occurrence of a symbol. `find-references` calls
    /// this exactly once per query. Returns a cloned Vec because the
    /// result crosses an async boundary.
    pub fn find(&self, symbol: &SymbolRefData) -> Vec<ReferenceLocationData> {
        let key = match symbol_to_key(symbol) {
            Some(k) => k,
            None => return Vec::new(),
        };
        self.forward.get(&key).cloned().unwrap_or_default()
    }

    /// Total entry count across all symbols. Useful for logging.
    pub fn entry_count(&self) -> usize {
        self.forward.values().map(|v| v.len()).sum()
    }

    /// Distinct files that have contributed entries.
    pub fn indexed_file_count(&self) -> usize {
        self.by_file.len()
    }

    /// Distinct symbol keys (unique `(kind, name)` pairs).
    pub fn distinct_symbol_count(&self) -> usize {
        self.forward.len()
    }
}

/// Translate the cross-async-boundary `SymbolRefData` variants into the
/// (kind, name) shape our forward map uses. Returns `None` for symbol
/// shapes we don't index (none today, but the option keeps future
/// additions backward-compatible).
fn symbol_to_key(symbol: &SymbolRefData) -> Option<SymbolKey> {
    let (kind, name) = match symbol {
        SymbolRefData::View(n) => (SymbolKind::View, n),
        SymbolRefData::Route(n) => (SymbolKind::Route, n),
        SymbolRefData::Config(n) => (SymbolKind::Config, n),
        SymbolRefData::Translation(n) => (SymbolKind::Translation, n),
        SymbolRefData::Env(n) => (SymbolKind::Env, n),
        SymbolRefData::Component(n) => (SymbolKind::Component, n),
        SymbolRefData::Livewire(n) => (SymbolKind::Livewire, n),
        SymbolRefData::Middleware(n) => (SymbolKind::Middleware, n),
        SymbolRefData::Binding(n) => (SymbolKind::Binding, n),
    };
    Some(SymbolKey {
        kind,
        name: name.clone(),
    })
}

#[cfg(test)]
mod tests;
