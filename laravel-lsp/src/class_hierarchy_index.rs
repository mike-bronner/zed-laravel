//! Project-wide class-hierarchy + member index.
//!
//! Where `symbol_index` answers "where is symbol X referenced?", this index
//! answers structural questions about the PHP type graph:
//!
//! - the declaration + members of a class/interface/trait/enum, by FQCN
//! - which classes **implement** an interface
//! - which classes **use** a trait
//! - which classes **extend** a given parent (its subclasses)
//!
//! Those four feed the structural code lenses (implementations / usages /
//! overrides / parent) and give cross-file inheritance resolution a single
//! place to consult instead of re-walking files on demand.
//!
//! ## Shape
//!
//! Mirrors `symbol_index`: a forward map (`fqcn → ClassNode`) plus reverse
//! adjacency maps, a `by_file` reverse index for cheap eviction, and a lazy
//! `dirty` set. Owned by the SalsaActor; all access is single-threaded
//! through the actor queue, so no internal locking.
//!
//! ## FQCN resolution
//!
//! The walker stores `extends` / `implements` / `use Trait` targets raw. We
//! resolve each to a fully-qualified name through the file's `use` aliases
//! (and the current namespace for still-bare names) so reverse edges line up
//! across files. Resolution matches `query_chain::use_aliases` conventions;
//! Laravel's global facade aliases (which aren't in `use` scope) are left
//! as-is, same as elsewhere in the codebase.
//!
//! ## Correctness invariants
//!
//! 1. For every reverse-edge entry `(target → [fqcn, …])`, each `fqcn`
//!    appears as a key in `classes` whose node lists `target` among its
//!    extends/implements/trait_uses.
//! 2. `remove_file(p)` evicts only nodes whose `file_path == p`, so a
//!    duplicate FQCN declared elsewhere survives.
//! 3. `take_dirty()` is idempotent: a second call returns an empty Vec.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::laravel_introspector::walker::{self, PhpMethodInfo, PhpPropertyInfo, PhpStructureKind};
use crate::query_chain::use_aliases::{self, UseAliases};

/// A single member (method or property) declaration with its source span.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MemberDecl {
    pub name: String,
    pub is_static: bool,
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

/// One class-like declaration, with inheritance edges resolved to FQCNs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ClassNode {
    pub fqcn: String,
    pub kind: PhpStructureKind,
    pub file_path: PathBuf,
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
    /// Resolved FQCN of the `extends` parent, if any.
    pub extends: Option<String>,
    /// Resolved FQCNs of `implements` interfaces (direct only).
    pub implements: Vec<String>,
    /// Resolved FQCNs of `use`-d traits (direct only).
    pub trait_uses: Vec<String>,
    pub methods: Vec<MemberDecl>,
    pub properties: Vec<MemberDecl>,
}

/// Inverted class-hierarchy index. Owned by the actor; never shared.
#[derive(Default, Debug)]
pub struct ClassHierarchyIndex {
    classes: HashMap<String, ClassNode>,
    /// interface FQCN → classes that implement it (direct).
    implementers: HashMap<String, Vec<String>>,
    /// trait FQCN → classes that `use` it (direct).
    trait_users: HashMap<String, Vec<String>>,
    /// parent FQCN → classes that extend it (direct).
    subclasses: HashMap<String, Vec<String>>,
    /// path → FQCNs that file contributed, for eviction.
    by_file: HashMap<PathBuf, Vec<String>>,
    /// Files whose nodes may have drifted; refreshed lazily by the caller.
    dirty: HashSet<PathBuf>,
}

impl ClassHierarchyIndex {
    /// Insert every node a file contributed into the forward map and the
    /// reverse adjacency maps. Caller pairs this with `remove_file` first
    /// when re-indexing an already-known path (same contract as
    /// `symbol_index`).
    pub fn insert_file(&mut self, path: &Path, nodes: Vec<ClassNode>) {
        if nodes.is_empty() {
            return;
        }
        let mut fqcns = Vec::with_capacity(nodes.len());
        for node in nodes {
            let fqcn = node.fqcn.clone();
            if let Some(parent) = &node.extends {
                self.subclasses
                    .entry(parent.clone())
                    .or_default()
                    .push(fqcn.clone());
            }
            for iface in &node.implements {
                self.implementers
                    .entry(iface.clone())
                    .or_default()
                    .push(fqcn.clone());
            }
            for tr in &node.trait_uses {
                self.trait_users
                    .entry(tr.clone())
                    .or_default()
                    .push(fqcn.clone());
            }
            self.classes.insert(fqcn.clone(), node);
            fqcns.push(fqcn);
        }
        self.by_file.insert(path.to_path_buf(), fqcns);
    }

    /// Evict every node this file owns, plus its reverse edges. A node whose
    /// `file_path` no longer matches `path` (a duplicate FQCN now owned by a
    /// different file) is left untouched.
    pub fn remove_file(&mut self, path: &Path) {
        let fqcns = match self.by_file.remove(path) {
            Some(f) => f,
            None => return,
        };
        for fqcn in fqcns {
            let owned = self
                .classes
                .get(&fqcn)
                .map(|n| n.file_path == path)
                .unwrap_or(false);
            if !owned {
                continue;
            }
            if let Some(node) = self.classes.remove(&fqcn) {
                if let Some(parent) = &node.extends {
                    remove_edge(&mut self.subclasses, parent, &fqcn);
                }
                for iface in &node.implements {
                    remove_edge(&mut self.implementers, iface, &fqcn);
                }
                for tr in &node.trait_uses {
                    remove_edge(&mut self.trait_users, tr, &fqcn);
                }
            }
        }
    }

    /// Mark a path as needing refresh (cheap; work deferred to `take_dirty`).
    pub fn mark_dirty(&mut self, path: &Path) {
        self.dirty.insert(path.to_path_buf());
    }

    /// Drain and return the dirty set so the caller can re-index each path.
    pub fn take_dirty(&mut self) -> Vec<PathBuf> {
        self.dirty.drain().collect()
    }

    /// Drop everything — used to rebuild from scratch after warming.
    pub fn clear(&mut self) {
        self.classes.clear();
        self.implementers.clear();
        self.trait_users.clear();
        self.subclasses.clear();
        self.by_file.clear();
        self.dirty.clear();
    }

    /// The declaration node for a class FQCN, if indexed.
    pub fn get(&self, fqcn: &str) -> Option<&ClassNode> {
        self.classes.get(fqcn)
    }

    /// Snapshot the `fqcn → declaring file` mapping. The magic-member index
    /// build (M4) runs in a parallel pass that can't borrow the actor-owned
    /// index, so it takes this cheap owned copy (paths only, not full nodes)
    /// to drive receiver resolution.
    pub fn fqcn_file_map(&self) -> std::collections::HashMap<String, PathBuf> {
        self.classes
            .iter()
            .map(|(fqcn, node)| (fqcn.clone(), node.file_path.clone()))
            .collect()
    }

    /// Group every indexed class by its declaring file. Used to persist the
    /// hierarchy alongside the pattern cache so it survives a warm restart
    /// (the index is otherwise only populated by fresh parses).
    pub fn nodes_by_file(&self) -> std::collections::HashMap<PathBuf, Vec<ClassNode>> {
        let mut map: std::collections::HashMap<PathBuf, Vec<ClassNode>> =
            std::collections::HashMap::new();
        for node in self.classes.values() {
            map.entry(node.file_path.clone())
                .or_default()
                .push(node.clone());
        }
        map
    }

    /// Classes that directly implement `fqcn` (an interface).
    pub fn implementers_of(&self, fqcn: &str) -> &[String] {
        self.implementers
            .get(fqcn)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Classes that directly `use` `fqcn` (a trait).
    pub fn trait_users_of(&self, fqcn: &str) -> &[String] {
        self.trait_users.get(fqcn).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Classes that directly extend `fqcn`.
    pub fn subclasses_of(&self, fqcn: &str) -> &[String] {
        self.subclasses.get(fqcn).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The resolved FQCN this class extends, if any.
    pub fn parent_of(&self, fqcn: &str) -> Option<&str> {
        self.classes.get(fqcn).and_then(|n| n.extends.as_deref())
    }

    pub fn class_count(&self) -> usize {
        self.classes.len()
    }

    pub fn indexed_file_count(&self) -> usize {
        self.by_file.len()
    }

    /// Total reverse-edge count (implements + trait-use + extends). For logs.
    pub fn edge_count(&self) -> usize {
        self.implementers.values().map(Vec::len).sum::<usize>()
            + self.trait_users.values().map(Vec::len).sum::<usize>()
            + self.subclasses.values().map(Vec::len).sum::<usize>()
    }
}

fn remove_edge(map: &mut HashMap<String, Vec<String>>, key: &str, value: &str) {
    if let Some(v) = map.get_mut(key) {
        v.retain(|x| x != value);
        if v.is_empty() {
            map.remove(key);
        }
    }
}

/// Parse a PHP file once and extract every class-like declaration as a
/// `ClassNode` with inheritance edges resolved to FQCNs. Returns empty on
/// parse failure or a file with no declarations.
pub fn classes_in_file(path: &Path, content: &str) -> Vec<ClassNode> {
    let Ok(tree) = crate::parser::parse_php(content) else {
        return Vec::new();
    };
    classes_from_tree(path, &tree, content)
}

/// Extract class nodes from an already-parsed PHP tree, so a caller that has
/// already parsed the file (e.g. cache warming) shares the one parse instead
/// of re-parsing just for hierarchy data.
pub fn classes_from_tree(path: &Path, tree: &tree_sitter::Tree, content: &str) -> Vec<ClassNode> {
    let aliases = use_aliases::extract_use_aliases(tree, content);
    let structure = walker::extract_php_structure_from_tree(tree, content.as_bytes());
    let namespace = structure.namespace.as_deref();

    structure
        .structures
        .iter()
        .map(|s| ClassNode {
            fqcn: qualify(&s.name, namespace),
            kind: s.kind,
            file_path: path.to_path_buf(),
            start_line: s.start_line,
            start_column: s.start_column,
            end_line: s.end_line,
            end_column: s.end_column,
            extends: s
                .extends_raw
                .as_deref()
                .map(|raw| resolve_fqcn(raw, &aliases, namespace)),
            implements: s
                .implements_raw
                .iter()
                .map(|raw| resolve_fqcn(raw, &aliases, namespace))
                .collect(),
            trait_uses: s
                .trait_uses
                .iter()
                .map(|raw| resolve_fqcn(raw, &aliases, namespace))
                .collect(),
            methods: s.methods.iter().map(member_from_method).collect(),
            properties: s.properties.iter().map(member_from_property).collect(),
        })
        .collect()
}

/// Qualify a declared class name with its file namespace.
fn qualify(name: &str, namespace: Option<&str>) -> String {
    match namespace {
        Some(ns) if !ns.is_empty() => format!("{ns}\\{name}"),
        _ => name.to_string(),
    }
}

/// Resolve a raw `extends`/`implements`/`use` target to an FQCN: alias
/// expansion first, then current-namespace qualification for still-bare
/// names. Explicitly-rooted names (`\Foo`) are taken as absolute.
fn resolve_fqcn(raw: &str, aliases: &UseAliases, namespace: Option<&str>) -> String {
    let was_absolute = raw.trim_start().starts_with('\\');
    let resolved = use_aliases::resolve_class_name(raw, aliases);
    if !was_absolute && !resolved.contains('\\') {
        if let Some(ns) = namespace {
            if !ns.is_empty() {
                return format!("{ns}\\{resolved}");
            }
        }
    }
    resolved
}

fn member_from_method(m: &PhpMethodInfo) -> MemberDecl {
    MemberDecl {
        name: m.name.clone(),
        is_static: m.is_static,
        start_line: m.start_line,
        start_column: m.start_column,
        end_line: m.end_line,
        end_column: m.end_column,
    }
}

fn member_from_property(p: &PhpPropertyInfo) -> MemberDecl {
    MemberDecl {
        name: p.name.clone(),
        is_static: p.is_static,
        start_line: p.start_line,
        start_column: p.start_column,
        end_line: p.end_line,
        end_column: p.end_column,
    }
}

#[cfg(test)]
mod tests;
