//! Prove that a class member is *read* by framework code — either through the
//! class's own inheritance chain, or by a vendor package consuming the class
//! through an interface it implements.
//!
//! ## Why this exists
//!
//! The unused-symbol diagnostic flags a code-lensed member with zero project
//! references as "possibly dead code". That's wrong for **framework-read
//! configuration properties**, which come in two structural flavors:
//!
//! 1. **Chain reads**: a model's `public $timestamps = false;` is never
//!    referenced by *app* code, but `Illuminate\Database\Eloquent\Concerns\
//!    HasTimestamps` — a trait in the model's own chain — reads
//!    `$this->timestamps`.
//! 2. **Consumer reads**: a queued job's `public $tries = 1;` is read by
//!    `Illuminate\Queue\Queue::getJobTries()` as `$job->tries ?? $job->tries()`
//!    — a duck-typed read on an object the framework was *handed*. Nothing in
//!    the job's chain (`Queueable`, `InteractsWithQueue`, …) ever touches
//!    `$tries`; the read lives in the package that consumes the
//!    `ShouldQueue` contract the job implements.
//!
//! Rather than maintain a hand-written allowlist of these properties (a
//! heuristic that drifts with each Laravel release), this module *proves* the
//! read deterministically:
//!
//! - **Chain walk**: resolve the class's `extends` parents and `use`d traits
//!   (app **or** vendor) to files and check whether any of them reads
//!   `$this->member` (or `self::$member` / `static::$member`).
//! - **Consumer scan**: if the chain proves nothing, take every interface the
//!   chain `implements`, resolve each to its `vendor/<vendor>/<package>/`
//!   root, and scan that package's PHP files for a property access
//!   `->member` on *any* receiver. The contract and its consumer normally
//!   share a package (`ShouldQueue` and `Queue` both live in
//!   `laravel/framework`), so the scan is bounded to one package tree.
//!
//! ## Scope — we do NOT index all of `vendor/`
//!
//! The chain walk only resolves the files actually *in the chain* — for a real
//! Eloquent model that's `Model` plus its concern traits, roughly two dozen
//! small files. The consumer scan walks at most the packages owning the
//! chain's interfaces (typically just `laravel/framework`), with a cheap
//! `->member` substring prefilter so tree-sitter only parses candidate files,
//! and memoizes the per-`(package, member)` verdict for the process lifetime
//! (vendor trees don't change mid-session; a `composer update` warrants a
//! server restart anyway). The full `vendor/` tree (often 10k+ files across
//! dozens of packages) is never walked.
//!
//! ## Concurrency
//!
//! [`member_read_in_chain`] is **synchronous**: it does filesystem IO and
//! tree-sitter parsing inline. Call it from `spawn_blocking`, never from
//! inside the Salsa actor — a slow walk there would serialize with every other
//! in-flight LSP request. The memo cache behind the consumer scan is a
//! `Mutex`ed map; contention is negligible (the lock is held only for a
//! lookup/insert, never across IO).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use walkdir::WalkDir;

use tree_sitter::Node;

use crate::class_hierarchy_index::classes_in_file;
use crate::class_locator::find_php_class_file_in_app_or_vendor;
use crate::parser::parse_php;

/// Safety bound on how many classes a single chain walk will resolve+parse. A
/// real Eloquent model's chain (`Model` + its concern traits) is ~25 classes;
/// 256 is generous headroom while still capping a pathological hierarchy (or a
/// resolution cycle the visited-set somehow doesn't catch).
const MAX_CHAIN_VISITS: usize = 256;

/// Safety bound on how many files a single package consumer scan will visit.
/// `laravel/framework` — by far the largest package this scan ever meets — is
/// ~3k PHP files; 8192 is generous headroom while still capping a pathological
/// vendor tree (a package that bundles fixtures, generated code, etc.).
const MAX_PACKAGE_FILES: usize = 8192;

/// Whether `member` is read as `$this->member` — or `self::$member` /
/// `static::$member` / `Foo::$member` — in `fqcn`'s own source or in any class
/// or trait reachable by walking its `extends` parents and `use`d traits; or,
/// failing that, whether a vendor package owning an interface the chain
/// `implements` reads `->member` on any receiver (a duck-typed consumer read —
/// see the module docs for the `$tries` example).
///
/// Each FQCN (app or vendor) is resolved to a file via
/// [`find_php_class_file_in_app_or_vendor`], parsed, and scanned; its parent and
/// traits are then enqueued. The walk is cycle-safe (each FQCN visited once) and
/// bounded by [`MAX_CHAIN_VISITS`]. Interfaces collected along the way feed the
/// consumer scan, which runs only if the chain walk proves nothing.
///
/// Returns `false` for inputs that don't name a resolvable class — an empty
/// FQCN, or a synthetic key like `volt::/path/to/file` (which carries `::`) —
/// without touching the filesystem.
///
/// See the module docs for the concurrency contract (call from
/// `spawn_blocking`).
pub fn member_read_in_chain(root: &Path, fqcn: &str, member: &str) -> bool {
    // Synthetic component keys (`volt::<path>`) and empty names don't name a
    // class. Bail before the resolver does a fruitless `vendor/` basename walk.
    if fqcn.trim().is_empty() || fqcn.contains("::") || member.is_empty() {
        return false;
    }

    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: Vec<String> = vec![normalize(fqcn)];
    // Interfaces implemented anywhere in the chain (the class itself or a
    // vendor parent), for the consumer scan after the chain walk comes up dry.
    let mut interfaces: HashSet<String> = HashSet::new();

    while let Some(current) = queue.pop() {
        if visited.len() >= MAX_CHAIN_VISITS {
            break;
        }
        if current.is_empty() || !visited.insert(current.clone()) {
            continue;
        }
        let Some(path) = find_php_class_file_in_app_or_vendor(&current, root) else {
            continue;
        };
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if source_reads_member(&content, member) {
            return true;
        }
        // Enqueue parents + traits (already resolved to FQCNs by the hierarchy
        // extractor). PSR-4 puts one class per file, so taking every node the
        // file declares is correct in practice and harmless otherwise (the
        // visited-set dedups, the bound caps fan-out).
        for node in classes_in_file(&path, &content) {
            if let Some(parent) = node.extends {
                queue.push(normalize(&parent));
            }
            for tr in node.trait_uses {
                queue.push(normalize(&tr));
            }
            for iface in node.implements {
                interfaces.insert(normalize(&iface));
            }
        }
    }

    member_read_by_interface_consumers(root, &interfaces, member)
}

/// Whether any vendor package owning one of `interfaces` reads `->member` on
/// any receiver. This is the duck-typed half of the proof: the framework reads
/// `$job->tries` in `Illuminate\Queue\Queue`, which is in NO job's inheritance
/// chain — but it IS in the package that owns the `ShouldQueue` contract the
/// job implements.
///
/// Interfaces that resolve outside `vendor/` (app-side contracts) are skipped:
/// app-side consumer reads are ordinary project references the index already
/// counts. Each package is scanned at most once per (package, member) — see
/// [`package_reads_member`] for the memoization.
fn member_read_by_interface_consumers(
    root: &Path,
    interfaces: &HashSet<String>,
    member: &str,
) -> bool {
    let mut packages: HashSet<PathBuf> = HashSet::new();
    for iface in interfaces {
        let Some(path) = find_php_class_file_in_app_or_vendor(iface, root) else {
            continue;
        };
        if let Some(pkg) = vendor_package_root(root, &path) {
            packages.insert(pkg);
        }
    }
    packages.iter().any(|pkg| package_reads_member(pkg, member))
}

/// `<root>/vendor/<vendor>/<package>` for a file under it, or `None` for any
/// path not inside the project's `vendor/` tree.
fn vendor_package_root(root: &Path, file: &Path) -> Option<PathBuf> {
    let rel = file.strip_prefix(root).ok()?;
    let mut comps = rel.components();
    if comps.next()?.as_os_str() != "vendor" {
        return None;
    }
    let vendor = comps.next()?;
    let package = comps.next()?;
    // A file directly under vendor/<v>/ has no package segment to own it.
    comps.next()?;
    Some(root.join("vendor").join(vendor).join(package))
}

/// Process-lifetime memo of consumer-scan verdicts, keyed by
/// `(package root, member)`. Vendor packages don't change mid-session, and the
/// diagnostics pass re-proves the same handful of members on every debounced
/// edit — without this, each keystroke batch would re-walk `laravel/framework`.
static PACKAGE_MEMBER_READS: LazyLock<Mutex<HashMap<(PathBuf, String), bool>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Whether any PHP file in the package tree reads `->member` on any receiver.
/// Memoized in [`PACKAGE_MEMBER_READS`]. Files are prefiltered with a cheap
/// `->member` substring check so tree-sitter only parses candidates; the walk
/// is bounded by [`MAX_PACKAGE_FILES`].
fn package_reads_member(package_root: &Path, member: &str) -> bool {
    let key = (package_root.to_path_buf(), member.to_string());
    if let Some(&hit) = PACKAGE_MEMBER_READS.lock().unwrap().get(&key) {
        return hit;
    }

    let needle = format!("->{member}");
    let mut seen = 0usize;
    let mut found = false;
    let walker = WalkDir::new(package_root).into_iter().filter_entry(|e| {
        let name = e.file_name().to_string_lossy();
        !matches!(name.as_ref(), "node_modules" | ".git")
    });
    for entry in walker.filter_map(|e| e.ok()) {
        if !entry.file_type().is_file()
            || entry.path().extension().and_then(|e| e.to_str()) != Some("php")
        {
            continue;
        }
        seen += 1;
        if seen > MAX_PACKAGE_FILES {
            break;
        }
        let Ok(content) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        if !content.contains(&needle) {
            continue;
        }
        if source_reads_member_any_receiver(&content, member) {
            found = true;
            break;
        }
    }

    PACKAGE_MEMBER_READS.lock().unwrap().insert(key, found);
    found
}

/// Strip a leading `\` and surrounding whitespace so FQCNs compare/dedup
/// consistently (`\Illuminate\…\Model` and `Illuminate\…\Model` are the same).
fn normalize(fqcn: &str) -> String {
    fqcn.trim().trim_start_matches('\\').to_string()
}

/// Whether `source` contains any read of `member` on `$this` (instance) or via a
/// scope (`self::$member` / `static::$member` / `Foo::$member`).
fn source_reads_member(source: &str, member: &str) -> bool {
    let Ok(tree) = parse_php(source) else {
        return false;
    };
    let bytes = source.as_bytes();
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if node_reads_member(n, bytes, member) {
            return true;
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    false
}

/// Whether `source` contains a property access `->member` (or `?->member`) on
/// ANY receiver — `$job->tries`, `$this->tries`, `$x?->tries`. Used by the
/// consumer scan, where the framework reads the property off an object it was
/// handed, so the receiver variable can be anything. Method calls
/// (`$job->tries()`) are a different tree-sitter node kind
/// (`member_call_expression`) and intentionally do NOT match: the diagnostic
/// only lenses properties, so only property reads count as proof.
fn source_reads_member_any_receiver(source: &str, member: &str) -> bool {
    let Ok(tree) = parse_php(source) else {
        return false;
    };
    let bytes = source.as_bytes();
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if matches!(
            n.kind(),
            "member_access_expression" | "nullsafe_member_access_expression"
        ) {
            let matched = n
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(bytes).ok())
                .map(|t| t.trim_start_matches('$') == member)
                .unwrap_or(false);
            if matched {
                return true;
            }
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    false
}

/// One node: is it a `$this->member` access or a `::$member` scoped access whose
/// member name equals `member`?
fn node_reads_member(node: Node, bytes: &[u8], member: &str) -> bool {
    match node.kind() {
        "member_access_expression" | "nullsafe_member_access_expression" => {
            let Some(object) = node.child_by_field_name("object") else {
                return false;
            };
            if object.kind() != "variable_name" || object.utf8_text(bytes).ok() != Some("$this") {
                return false;
            }
            node.child_by_field_name("name")
                .and_then(|n| n.utf8_text(bytes).ok())
                .map(|t| t.trim_start_matches('$') == member)
                .unwrap_or(false)
        }
        // `self::$x` / `static::$x` / `Foo::$x`: the scope is a name/relative
        // scope; the property part is a `variable_name`. Match on the variable.
        "scoped_property_access_expression" => {
            let mut c = node.walk();
            let found = node.children(&mut c).any(|ch| {
                ch.kind() == "variable_name"
                    && ch
                        .utf8_text(bytes)
                        .ok()
                        .map(|t| t.trim_start_matches('$') == member)
                        .unwrap_or(false)
            });
            found
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests;
