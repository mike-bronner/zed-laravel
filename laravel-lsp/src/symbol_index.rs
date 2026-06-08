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
    /// Eloquent magic member / plain class member (M4). The `name` is the
    /// composite `<declaring_fqcn>#<member>` produced by the M3 resolver, so
    /// usages of an inherited or trait-shared member all share one key.
    MagicMember,
}

/// A resolved magic-member occurrence ready to ingest: the inheritance-
/// resolved declaring class, the member name, and the usage site's position.
/// Built by the actor from a file's `member_access_refs` after M3 resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MagicMemberEntry {
    pub fqcn: String,
    pub member: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
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
            // Append rather than overwrite so a file's literal-symbol keys and
            // its magic-member keys (added via `insert_magic_members`) coexist
            // in `by_file` regardless of call order. Safe under the
            // remove-before-reinsert contract — `by_file[path]` is empty when
            // a fresh insert runs.
            self.by_file
                .entry(path.to_path_buf())
                .or_default()
                .extend(keys);
        }
    }

    /// Add resolved magic-member entries (M4) into the forward map under
    /// `SymbolKind::MagicMember` keys, recording them in `by_file` for
    /// eviction. The entries are produced by the actor running the M3 resolver
    /// over a file's captured `member_access_refs`; this method is the dumb
    /// store — all resolution/classification has already happened.
    ///
    /// Call alongside `insert_file` for the same path (order-independent).
    pub fn insert_magic_members(&mut self, path: &Path, entries: &[MagicMemberEntry]) {
        if entries.is_empty() {
            return;
        }
        let mut keys: Vec<SymbolKey> = Vec::with_capacity(entries.len());
        for e in entries {
            let key = SymbolKey {
                kind: SymbolKind::MagicMember,
                name: magic_member_key_name(&e.fqcn, &e.member),
            };
            let loc = ReferenceLocationData {
                file_path: path.to_path_buf(),
                line: e.line,
                column: e.column,
                end_column: e.end_column,
            };
            self.forward.entry(key.clone()).or_default().push(loc);
            keys.push(key);
        }
        self.by_file
            .entry(path.to_path_buf())
            .or_default()
            .extend(keys);
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

    /// Evict only this file's *literal-symbol* keys (views, routes, config,
    /// …), leaving its magic-member entries untouched.
    ///
    /// The lazy dirty-drain re-parses a file and re-inserts its literals on
    /// demand via [`insert_file`], but magic members are resolved only by the
    /// warm/save passes ([`insert_magic_members`]). So a plain [`remove_file`]
    /// here would drop the file's magic entries with nothing to restore them
    /// until the next save — which silently zeroes magic-member find-references
    /// and code-lens counts the moment an indexed file is edited or reopened.
    /// The drain uses this instead so magic survives. Real file *deletes* still
    /// use [`remove_file`] to drop everything.
    pub fn remove_literal_entries(&mut self, path: &Path) {
        let Some(keys) = self.by_file.remove(path) else {
            return;
        };
        let mut retained: Vec<SymbolKey> = Vec::new();
        for key in keys {
            if key.kind == SymbolKind::MagicMember {
                retained.push(key);
                continue;
            }
            if let Some(entries) = self.forward.get_mut(&key) {
                entries.retain(|loc| loc.file_path.as_path() != path);
                if entries.is_empty() {
                    self.forward.remove(&key);
                }
            }
        }
        if !retained.is_empty() {
            self.by_file.insert(path.to_path_buf(), retained);
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

    /// Reverse lookup: every reference sharing the magic-member symbol whose
    /// occurrence covers `(line, column)` in `path`.
    ///
    /// The index is already a position→symbol map — each resolved member access
    /// was stored with its usage position. So find-references invoked *at a
    /// usage site* (`$post->status`, `$this->status`, …) doesn't need to
    /// re-resolve the receiver: we just find which symbol owns the clicked
    /// position and return its whole bucket. This is the only path that works
    /// for Blade, where re-parsing the template as PHP can't locate the node.
    ///
    /// A union-typed receiver (`$post` inferred as two classes) can produce two
    /// symbols at the same position; we return the union of their references,
    /// de-duplicated by `(file, line, column)`.
    pub fn references_at(
        &self,
        path: &Path,
        line: u32,
        column: u32,
    ) -> Vec<ReferenceLocationData> {
        let Some(keys) = self.by_file.get(path) else {
            return Vec::new();
        };
        let mut out: Vec<ReferenceLocationData> = Vec::new();
        let mut seen_keys: HashSet<&SymbolKey> = HashSet::new();
        let mut seen_locs: HashSet<(PathBuf, u32, u32)> = HashSet::new();
        for key in keys {
            if key.kind != SymbolKind::MagicMember || !seen_keys.insert(key) {
                continue;
            }
            let Some(locs) = self.forward.get(key) else {
                continue;
            };
            let covers = locs.iter().any(|l| {
                l.file_path.as_path() == path
                    && l.line == line
                    && column >= l.column
                    && column <= l.end_column
            });
            if !covers {
                continue;
            }
            for l in locs {
                if seen_locs.insert((l.file_path.clone(), l.line, l.column)) {
                    out.push(l.clone());
                }
            }
        }
        out
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
        SymbolRefData::View(n) => (SymbolKind::View, n.clone()),
        SymbolRefData::Route(n) => (SymbolKind::Route, n.clone()),
        SymbolRefData::Config(n) => (SymbolKind::Config, n.clone()),
        SymbolRefData::Translation(n) => (SymbolKind::Translation, n.clone()),
        SymbolRefData::Env(n) => (SymbolKind::Env, n.clone()),
        SymbolRefData::Component(n) => (SymbolKind::Component, n.clone()),
        SymbolRefData::Livewire(n) => (SymbolKind::Livewire, n.clone()),
        SymbolRefData::Middleware(n) => (SymbolKind::Middleware, n.clone()),
        SymbolRefData::Binding(n) => (SymbolKind::Binding, n.clone()),
        SymbolRefData::MagicMember { fqcn, member } => {
            (SymbolKind::MagicMember, magic_member_key_name(fqcn, member))
        }
    };
    Some(SymbolKey { kind, name })
}

/// Composite name for a magic-member key: `<declaring_fqcn>#<member>`. Used by
/// both the index population (M4) and `symbol_to_key` so they agree on the
/// exact string. `#` can't appear in a PHP FQCN or member name, so it's an
/// unambiguous separator.
pub fn magic_member_key_name(fqcn: &str, member: &str) -> String {
    format!("{fqcn}#{member}")
}

#[cfg(test)]
mod tests;
