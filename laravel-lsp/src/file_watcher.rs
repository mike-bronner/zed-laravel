//! Server-initiated file watching via LSP `workspace/didChangeWatchedFiles`.
//!
//! Without this, external file changes (a `git pull`, a formatter run
//! outside Zed, another editor saving) leave our in-memory pattern
//! cache stale until the user restarts the LSP — and stale cache means
//! wrong find-references results, which is the single failure mode
//! users are least forgiving of.
//!
//! Design choices, in case they need revisiting later:
//!
//! 1. **Glob-scoped, not project-wide.** We only ask the client to
//!    notify us about files under the four Laravel directories we
//!    actually index: `app/Http/Controllers`, the configured view
//!    paths, the Livewire path (if any), and `routes/`. A
//!    `composer install` that rewrites thousands of files in
//!    `vendor/` produces zero notifications.
//!
//! 2. **Server-initiated dynamic registration.** Zed's view paths and
//!    Livewire path depend on the project's `config/view.php` and
//!    `config/livewire.php`, which we can't see during `initialize`
//!    (we haven't read them yet). So we declare the capability statically
//!    in `initialize` and send the actual `workspace/didChangeWatchedFiles`
//!    registration later, from `initialized` once the config is loaded.
//!
//! 3. **Lazy re-parse, eager invalidation.** When an event arrives, we
//!    remove the entry from `pattern_cache` and bump the Salsa file
//!    version. We do NOT spawn a re-parse — the next query that touches
//!    the file pays the parse cost lazily. Spreads work across user-
//!    driven queries instead of bunching it.
//!
//! 4. **No debounce.** Per-event work is ~50µs (a DashMap remove + an
//!    actor message). Even a 2000-file burst from `git checkout` drains
//!    in <100ms — invisible. If we ever add eager re-parsing, a quiet-
//!    period debouncer is the right addition; without it, the work is
//!    too cheap to coalesce.
//!
//! 5. **Open-document precedence.** If a file is currently open in the
//!    editor, its in-memory authoritative content is the editor buffer,
//!    NOT what's on disk. `textDocument/didChange` already handles those
//!    updates and pushes buffer text into Salsa. We skip
//!    watched-file events for open paths to avoid clobbering the
//!    buffer with disk content the user hasn't seen yet (race during
//!    external edit while file is open in Zed).

use std::path::{Path, PathBuf};

use lsp_types::{
    DidChangeWatchedFilesRegistrationOptions, FileSystemWatcher, GlobPattern, Registration,
    WatchKind,
};

/// One registration ID for our single file-watcher registration. Letting
/// the server send a future `client/unregisterCapability` with the same
/// ID would tear it down cleanly — we don't do that today, but the ID
/// is here in case we ever support config reloads that change the
/// watched globs.
pub const REGISTRATION_ID: &str = "laravel-lsp/file-watcher";

/// LSP method this registration is for.
pub const METHOD: &str = "workspace/didChangeWatchedFiles";

/// Build the glob patterns to watch for a given project. Globs are
/// absolute paths — Zed handles both absolute and relative globs, but
/// absolute removes any ambiguity about which workspace folder a
/// pattern is rooted in.
///
/// The set of globs covers exactly the directories that
/// `SalsaActor::handle_register_project_files` enumerates, so the
/// pattern_cache only ever holds entries for files we're also
/// watching.
pub fn build_watchers(
    root: &Path,
    view_paths: &[PathBuf],
    livewire_path: Option<&Path>,
) -> Vec<FileSystemWatcher> {
    // Watch creates, changes, and deletes — all three matter for
    // keeping the pattern cache aligned with disk. The LSP spec's
    // default if `kind` is omitted is also "all three (7)", so we
    // could leave it None; we pass it explicitly for clarity.
    let kind = Some(WatchKind::Create | WatchKind::Change | WatchKind::Delete);

    let mut watchers = Vec::with_capacity(4 + view_paths.len());

    // Controllers — current default path. If a project moves them, we
    // miss those changes until a future improvement makes this glob
    // configurable. Acceptable for v1.
    watchers.push(FileSystemWatcher {
        glob_pattern: GlobPattern::String(format!(
            "{}/app/Http/Controllers/**/*.php",
            root.display()
        )),
        kind,
    });

    // Routes.
    watchers.push(FileSystemWatcher {
        glob_pattern: GlobPattern::String(format!("{}/routes/**/*.php", root.display())),
        kind,
    });

    // Migrations — feed the migration index (goto-definition for columns and
    // tables). New/renamed/edited migrations change column definitions.
    watchers.push(FileSystemWatcher {
        glob_pattern: GlobPattern::String(format!(
            "{}/database/migrations/**/*.php",
            root.display()
        )),
        kind,
    });

    // View paths. We watch `.blade.php` first as the primary case, then
    // bare `.php` for the rare anonymous-component-in-PHP-only style.
    // Some projects configure multiple view paths (e.g., themed apps);
    // we register one pair per configured path.
    for view_path in view_paths {
        watchers.push(FileSystemWatcher {
            glob_pattern: GlobPattern::String(format!("{}/**/*.blade.php", view_path.display())),
            kind,
        });
        watchers.push(FileSystemWatcher {
            glob_pattern: GlobPattern::String(format!("{}/**/*.php", view_path.display())),
            kind,
        });
    }

    // Livewire path, when the project uses it. v3 vs v2 differ in
    // location; the config layer already resolved which one applies.
    if let Some(lw) = livewire_path {
        watchers.push(FileSystemWatcher {
            glob_pattern: GlobPattern::String(format!("{}/**/*.php", lw.display())),
            kind,
        });
    }

    // Vendor packages. We index everything composer-installed (see
    // SalsaActor::vendor_files) — the watcher needs matching globs so
    // changes from `composer install`, `composer update`, or a local
    // package symlink edit invalidate the right entries. Two globs
    // cover PHP source and Blade views; the `.json.php` data-file
    // skip lives in the warming filter, not at the watcher layer.
    watchers.push(FileSystemWatcher {
        glob_pattern: GlobPattern::String(format!("{}/vendor/**/*.php", root.display())),
        kind,
    });
    watchers.push(FileSystemWatcher {
        glob_pattern: GlobPattern::String(format!("{}/vendor/**/*.blade.php", root.display())),
        kind,
    });

    watchers
}

/// Build the full registration payload for `client/registerCapability`.
/// One registration covers all globs in a single batch — Zed processes
/// them as one watcher set rather than N independent watchers.
pub fn build_registration(
    root: &Path,
    view_paths: &[PathBuf],
    livewire_path: Option<&Path>,
) -> Registration {
    let watchers = build_watchers(root, view_paths, livewire_path);
    let opts = DidChangeWatchedFilesRegistrationOptions { watchers };
    Registration {
        id: REGISTRATION_ID.to_string(),
        method: METHOD.to_string(),
        register_options: serde_json::to_value(opts).ok(),
    }
}

#[cfg(test)]
mod tests;
