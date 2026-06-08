//! Eager, project-wide index of Artisan commands — the resolution half of
//! issue #62. Where [`crate::command_signature`] reads a single `Command` class
//! and [`crate::command_call_locator`] finds the *references* to a command in
//! ordinary PHP, this module ties them together: it scans the whole project
//! (including `vendor/`) for `Command` subclasses, extracts each one's
//! `$signature`, and keys them by command name so goto-definition and hover can
//! resolve a call-site string (`Artisan::call('emails:send')`) to the class
//! that declares it.
//!
//! Built once at init and refreshed when a relevant `Command` file changes
//! (mirrors [`crate::migration_index`]). The walk is regex-gated — a file is
//! only fully parsed when it actually `extends ...Command`, so the cost of
//! scanning a large `vendor/` tree stays bounded to real command classes.
//!
//! ## Priority (AC: app overrides framework/package)
//!
//! Two classes can declare the same command name (a package ships
//! `queue:work`, an app overrides it). The index keeps the highest-priority
//! declaration, matching the convention in `CLAUDE.md`
//! (*Framework=0, Package=1, App=2 — higher wins*):
//!
//! | Source | Priority | Detected by |
//! |--------|----------|-------------|
//! | App    | 2 | not under `vendor/` |
//! | Package| 1 | under `vendor/` (non-framework) |
//! | Framework | 0 | under `vendor/laravel/framework/` |
//!
//! On a tie (same name, same priority) the first one walked wins — stable and
//! good enough; ambiguity across two packages is vanishingly rare.
//!
//! ## Position convention
//!
//! 0-based throughout; `start_column`/`end_column` bracket the signature
//! string's *content* (inside the quotes), matching the rest of the stack — see
//! `CLAUDE.md`. Goto lands on the `$signature` declaration, which is where the
//! command is actually defined.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lazy_static::lazy_static;
use regex::Regex;
use walkdir::WalkDir;

use crate::command_signature::{extends_console_command, extract_command_signature};

/// Source tier of a command declaration. Higher wins when two classes declare
/// the same command name (`App` overrides a `Package` which overrides the
/// `Framework`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CommandPriority {
    /// Shipped by `laravel/framework` itself (`vendor/laravel/framework/`).
    Framework = 0,
    /// Shipped by any other `vendor/` package.
    Package = 1,
    /// Defined in the project's own source (not under `vendor/`).
    App = 2,
}

/// One Artisan command discovered on a `Command` subclass, resolved to its
/// declaration site. `class_name` powers the hover summary; the position fields
/// point at the `$signature` string content so goto lands on the declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandEntry {
    /// The resolvable command name (`emails:send`).
    pub name: String,
    /// The declaring class's short name (`SendEmails`), for hover summaries.
    pub class_name: String,
    /// The full signature string content as written (arguments/options kept).
    pub raw_signature: String,
    /// File that declares the command.
    pub file: PathBuf,
    /// 0-based row of the `$signature` string content.
    pub line: u32,
    /// 0-based column of the first content character (after the opening quote).
    pub start_column: u32,
    /// 0-based column one past the last content character (before the quote).
    pub end_column: u32,
    /// Source tier — see [`CommandPriority`].
    pub priority: CommandPriority,
}

/// Resolved Artisan commands across the project and `vendor/`, keyed by command
/// name with app-over-package-over-framework priority already applied.
#[derive(Debug, Clone, Default)]
pub struct CommandIndex {
    commands: HashMap<String, CommandEntry>,
}

impl CommandIndex {
    /// The declaring class for `name`, if any command provides it.
    pub fn resolve(&self, name: &str) -> Option<&CommandEntry> {
        self.commands.get(name)
    }

    /// Number of distinct command names indexed.
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

/// Classify a command file's source tier from its path.
pub fn classify_priority(path: &Path) -> CommandPriority {
    let s = path.to_string_lossy().replace('\\', "/");
    if s.contains("/vendor/laravel/framework/") || s.contains("vendor/laravel/framework/") {
        CommandPriority::Framework
    } else if s.contains("/vendor/") || s.starts_with("vendor/") {
        CommandPriority::Package
    } else {
        CommandPriority::App
    }
}

/// The declaring class's short name, e.g. `SendEmails` from
/// `class SendEmails extends Command`. Returns `None` when no class declaration
/// is found (the caller then skips the file — no class, no goto target).
pub fn class_name_from_content(content: &str) -> Option<String> {
    lazy_static! {
        static ref CLASS_RE: Regex = Regex::new(r"\bclass\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    }
    CLASS_RE
        .captures(content)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

/// Directory names never worth descending into when hunting for command
/// classes — build output and VCS metadata, none of which hold PHP source.
const SKIP_DIRS: &[&str] = &["node_modules", ".git", "storage", "public"];

/// Build the index by walking every `*.php` under `<root>` (project + vendor),
/// keeping the highest-priority declaration per command name. Non-PHP files,
/// build/VCS directories, and files that don't `extends ...Command` are skipped
/// cheaply so a large `vendor/` tree doesn't dominate the walk.
pub fn build_command_index(root: &Path) -> CommandIndex {
    let mut index = CommandIndex::default();
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| {
            !(e.file_type().is_dir()
                && e.file_name()
                    .to_str()
                    .is_some_and(|n| SKIP_DIRS.contains(&n)))
        })
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "php") {
            if let Ok(content) = std::fs::read_to_string(path) {
                index_command_file(&mut index, path, &content);
            }
        }
    }
    index
}

/// Index a single PHP file's command declaration, if it has one. Exposed for
/// unit tests; [`build_command_index`] is the production entry.
///
/// The new entry replaces an existing one for the same command name only when
/// it ranks strictly higher (App > Package > Framework) — so an app command
/// wins over a package/framework command of the same name, and a same-tier
/// duplicate leaves the first-walked winner in place.
pub fn index_command_file(index: &mut CommandIndex, path: &Path, content: &str) {
    // Cheap gate: skip anything that isn't a Command subclass before the
    // heavier signature/class-name extraction runs.
    if !extends_console_command(content) {
        return;
    }
    let Some(sig) = extract_command_signature(content) else {
        return;
    };
    let Some(class_name) = class_name_from_content(content) else {
        return;
    };

    let priority = classify_priority(path);
    let entry = CommandEntry {
        name: sig.name.clone(),
        class_name,
        raw_signature: sig.raw_signature,
        file: path.to_path_buf(),
        line: sig.line,
        start_column: sig.start_column,
        end_column: sig.end_column,
        priority,
    };

    match index.commands.get(&sig.name) {
        Some(existing) if existing.priority >= entry.priority => {}
        _ => {
            index.commands.insert(sig.name, entry);
        }
    }
}

#[cfg(test)]
mod tests;
