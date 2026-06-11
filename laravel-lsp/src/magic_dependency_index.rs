//! Reverse dependency index for the magic-member system: which files
//! resolved member-access receivers against which classes.
//!
//! This is what makes the save-time refresh *incremental* (#80). When a
//! saved file changes a class's surface, the old behavior re-resolved the
//! entire project; this index answers "which files actually reference that
//! class?" so only the genuine blast radius re-resolves.
//!
//! **Populated from attempts, not successes.** The resolvers record every
//! receiver FQCN they *try* to classify against — including lookups where
//! the member doesn't (yet) exist on the class. That asymmetry is the
//! point: if `$user->avatar` fails today because `User` has no `avatar`,
//! the file still depends on `User` — adding the member tomorrow must
//! re-resolve this file or the new reference stays invisible until a full
//! rebuild. Receiver FQCNs come from use-statements and type-hints, so
//! they're recordable even when the class isn't indexed at all.
//!
//! Mirrors `symbol_index` ownership: actor-owned, no internal locking,
//! all access serialized through the actor queue.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Default, Debug)]
pub struct MagicDependencyIndex {
    /// fqcn → files that resolved a receiver against it.
    dependents: HashMap<String, HashSet<PathBuf>>,
    /// file → FQCNs it referenced (for eviction on re-index).
    by_file: HashMap<PathBuf, HashSet<String>>,
}

impl MagicDependencyIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace `path`'s recorded dependencies with `fqcns`
    /// (evict-then-insert, same contract as the other actor indexes).
    pub fn replace_file(&mut self, path: &Path, fqcns: HashSet<String>) {
        self.remove_file(path);
        if fqcns.is_empty() {
            return;
        }
        for fqcn in &fqcns {
            self.dependents
                .entry(fqcn.clone())
                .or_default()
                .insert(path.to_path_buf());
        }
        self.by_file.insert(path.to_path_buf(), fqcns);
    }

    /// Drop `path`'s contribution entirely (file deleted or re-indexing).
    pub fn remove_file(&mut self, path: &Path) {
        let Some(fqcns) = self.by_file.remove(path) else {
            return;
        };
        for fqcn in fqcns {
            if let Some(files) = self.dependents.get_mut(&fqcn) {
                files.remove(path);
                if files.is_empty() {
                    self.dependents.remove(&fqcn);
                }
            }
        }
    }

    /// Union of files that reference any of `fqcns`. The save flow feeds
    /// this the surface-changed classes plus their transitive descendants
    /// and re-resolves exactly the returned set.
    pub fn dependents_of<'a, I>(&self, fqcns: I) -> HashSet<PathBuf>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let mut out = HashSet::new();
        for fqcn in fqcns {
            if let Some(files) = self.dependents.get(fqcn) {
                out.extend(files.iter().cloned());
            }
        }
        out
    }

    /// Drop everything — paired with the full-rebuild path, which
    /// repopulates from scratch.
    pub fn clear(&mut self) {
        self.dependents.clear();
        self.by_file.clear();
    }

    /// Number of files with recorded dependencies. For logs.
    pub fn file_count(&self) -> usize {
        self.by_file.len()
    }

    /// Number of distinct FQCNs referenced. For logs.
    pub fn class_count(&self) -> usize {
        self.dependents.len()
    }
}

#[cfg(test)]
mod tests;
