//! End-to-end integration tests for the rename engines.
//!
//! These tests sit one level above the per-module unit tests in
//! `src/<module>/tests.rs`: they exercise the locator + EditTarget +
//! WorkspaceEdit composition flow as a single unit, using real
//! tempdir-backed fixture files instead of in-memory strings. The
//! goal is to catch regressions in the orchestration logic the
//! rename handler in `main.rs` performs — locator output flowing into
//! `EditTarget`, `EditTarget`s + `FileRename`s flowing into
//! `build_rename_workspace_edit`, and the resulting `WorkspaceEdit`
//! shape matching what an LSP client expects.
//!
//! What's NOT tested here:
//!
//! - The async `rename` LSP handler itself — that lives on `Backend`
//!   and depends on the Salsa actor + cached config + tokio runtime,
//!   none of which are economical to fixture for a synchronous unit
//!   test. The handler's logic is a thin sequence of these public
//!   helpers; if each composes correctly here, the handler works.
//! - Salsa-side `find_references` — covered by the symbol_index
//!   tests and the per-pattern parser tests.
//! - The conventional Livewire-resolver paths (V4 SFC, V4 MFC, V3
//!   Class, Volt) — heavily covered by `livewire_declaration_locator`
//!   and `livewire_component_resolution` unit tests. The one Livewire
//!   case below is the **view-only fallback** that those tests do NOT
//!   cover: components registered via `Livewire::component()` whose
//!   view lives at a non-conventional path (Jetstream-style) and have
//!   no backing class file.

use laravel_lsp::rename::{build_rename_edit, build_rename_workspace_edit, EditTarget, FileRename};
use laravel_lsp::salsa_impl::LaravelConfigData;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use tower_lsp::lsp_types::{DocumentChangeOperation, DocumentChanges, OneOf, ResourceOp};

// ----- fixtures -----------------------------------------------------------

/// Create a minimal `LaravelConfigData` pointing at the temp project
/// root with conventional Laravel paths. Constructed by hand to avoid
/// touching the config-discovery code, which has its own tests and
/// would drag composer.json + package detection into every rename test.
fn fake_config(root: &Path) -> LaravelConfigData {
    LaravelConfigData {
        root: root.to_path_buf(),
        view_paths: vec![root.join("resources/views")],
        component_paths: vec![("".to_string(), root.join("resources/views/components"))],
        livewire_path: Some(root.join("resources/views/livewire")),
        has_livewire: true,
        view_namespaces: HashMap::new(),
        component_namespaces: HashMap::new(),
        anonymous_component_paths: HashMap::new(),
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
        class_component_files: HashMap::new(),
    }
}

/// Write `content` to `path`, creating parent directories as needed.
/// Mirrors the helper in `livewire_component_resolution.rs` so test
/// shapes stay consistent across files.
fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

// ============================================================================
// View rename — file move + call-site rewrites
// ============================================================================

#[test]
fn view_rename_locates_file_and_computes_target_for_dotted_name() {
    use laravel_lsp::view_declaration_locator::{
        compute_target_path, is_under_vendor, locate_view_file,
    };

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let view_file = root.join("resources/views/users/profile.blade.php");
    write(&view_file, "<h1>profile</h1>");

    let config = fake_config(root);

    let current = locate_view_file("users.profile", &config).expect("view should be located");
    assert_eq!(current, view_file);
    assert!(!is_under_vendor(&current, root));

    let target = compute_target_path("users.profile", "users.account", &current, &config)
        .expect("target path should be computable");
    assert_eq!(target, root.join("resources/views/users/account.blade.php"));
}

#[test]
fn view_rename_refuses_vendor_view() {
    use laravel_lsp::view_declaration_locator::is_under_vendor;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let vendor_view = root.join("vendor/some-package/resources/views/widget.blade.php");
    write(&vendor_view, "{{ $stuff }}");

    // The rename handler refuses if `is_under_vendor` returns true,
    // BEFORE computing any edits.
    assert!(is_under_vendor(&vendor_view, root));
}

// ============================================================================
// Blade component rename — class-backed flavour (file moves + class decl)
// ============================================================================

#[test]
fn component_rename_class_based_locates_both_files() {
    use laravel_lsp::component_declaration_locator::{
        compute_blade_target_path, conventional_class_file_path, locate_component,
    };

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let view_file = root.join("resources/views/components/button.blade.php");
    write(&view_file, "<button>{{ $slot }}</button>");
    let class_file = root.join("app/View/Components/Button.php");
    write(
        &class_file,
        "<?php namespace App\\View\\Components;\n\nclass Button extends Component {}",
    );

    let config = fake_config(root);

    let files = locate_component("button", &config).expect("component should be located");
    assert_eq!(files.blade_file.as_deref(), Some(view_file.as_path()));
    assert_eq!(files.class_file.as_deref(), Some(class_file.as_path()));

    let new_blade = compute_blade_target_path(
        "button",
        "alert-button",
        files.blade_file.as_ref().unwrap(),
        &config,
    )
    .expect("blade target should compute");
    assert_eq!(
        new_blade,
        root.join("resources/views/components/alert-button.blade.php")
    );
    let new_class = conventional_class_file_path("alert-button", &config);
    assert_eq!(new_class, root.join("app/View/Components/AlertButton.php"));
}

#[test]
fn component_rename_anonymous_has_no_class_file() {
    use laravel_lsp::component_declaration_locator::locate_component;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let view_file = root.join("resources/views/components/card.blade.php");
    write(&view_file, "<div class=card>{{ $slot }}</div>");
    // No backing class.

    let config = fake_config(root);
    let files = locate_component("card", &config).expect("anonymous component locates");
    assert_eq!(files.blade_file.as_deref(), Some(view_file.as_path()));
    assert!(
        files.class_file.is_none(),
        "anonymous components have no class file"
    );
}

// ============================================================================
// Livewire — VIEW-ONLY fallback (no class file)
// ============================================================================
//
// The rename handler's primary Livewire path goes through
// `livewire_declaration_locator::locate(name, livewire_config, version)`,
// which is well-covered by 22 unit tests + the LiveWire resolver tests.
//
// The case below is the FALLBACK path that triggers when `locate()` returns
// None: a Livewire component whose view exists at a non-conventional path,
// typically one of:
//   - Published Jetstream views (`resources/views/api/api-token-manager.blade.php`)
//   - Components registered explicitly via `Livewire::component('foo', ...)`
//     pointing at a view outside the configured `view_path`
//
// The rename handler resolves these via `LaravelConfigData::resolve_view_path`
// (the same resolver used for plain `view('x')` calls) and emits a single
// file-rename edit. Vendor-located fallback views still refuse.

#[test]
fn livewire_view_only_resolves_via_laravel_view_path() {
    use laravel_lsp::view_declaration_locator::{compute_target_path, is_under_vendor};

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    // Jetstream-style: view lives at resources/views/api/api-token-manager.blade.php
    // — NOT under resources/views/livewire/ (where the conventional Livewire
    // resolver looks). No class file exists.
    let jetstream_view = root.join("resources/views/api/api-token-manager.blade.php");
    write(&jetstream_view, "<div>token manager</div>");

    let config = fake_config(root);

    // resolve_view_path emits candidate paths; the rename handler picks
    // the first one that exists. For dotted name "api.api-token-manager"
    // the candidate is resources/views/api/api-token-manager.blade.php.
    let candidates = config.resolve_view_path("api.api-token-manager");
    let found = candidates
        .into_iter()
        .find(|p| p.exists())
        .expect("view-only Livewire component view should resolve via view-path");
    assert_eq!(found, jetstream_view);

    // Not under vendor/ → rename proceeds.
    assert!(!is_under_vendor(&found, root));

    // The handler reuses View's `compute_target_path` for the file move.
    let target = compute_target_path(
        "api.api-token-manager",
        "api.token-manager",
        &found,
        &config,
    )
    .expect("target view path should compute");
    assert_eq!(
        target,
        root.join("resources/views/api/token-manager.blade.php")
    );
}

#[test]
fn livewire_view_only_refuses_vendor_published_view() {
    use laravel_lsp::view_declaration_locator::is_under_vendor;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    // A vendor package's resources/views — the view-only fallback would
    // refuse rather than break the package's registered name.
    let vendor_view = root.join("vendor/jetstream/resources/views/api-token-manager.blade.php");
    write(&vendor_view, "<div>vendor token manager</div>");

    assert!(is_under_vendor(&vendor_view, root));
}

// ============================================================================
// Middleware alias rename — registration-site locator
// ============================================================================

#[test]
fn middleware_alias_locator_finds_per_entry_form() {
    use laravel_lsp::middleware_binding_locator::locate_alias_on_line;

    let dir = TempDir::new().unwrap();
    let kernel = dir.path().join("app/Http/Kernel.php");
    write(
        &kernel,
        "<?php\n\
         protected $routeMiddleware = [\n\
        \x20    'auth' => \\App\\Http\\Middleware\\Authenticate::class,\n\
        \x20    'verified' => \\App\\Http\\Middleware\\Verified::class,\n\
         ];\n",
    );

    // Per-entry form: source_line points at the alias entry directly.
    // 1-based line 3 holds 'auth'.
    let span = locate_alias_on_line(&kernel, 3, "auth").expect("auth alias should locate");
    assert_eq!(span.line, 2, "0-based line index for 1-based line 3");
    // Slice the file content at the returned span; should equal "auth".
    let content = std::fs::read_to_string(&kernel).unwrap();
    let line = content.lines().nth(span.line as usize).unwrap();
    let slice: String = line
        .chars()
        .skip(span.start_column as usize)
        .take((span.end_column - span.start_column) as usize)
        .collect();
    assert_eq!(slice, "auth");
}

#[test]
fn middleware_alias_locator_forward_scans_bulk_array() {
    use laravel_lsp::middleware_binding_locator::locate_alias_on_line;

    let dir = TempDir::new().unwrap();
    let bootstrap = dir.path().join("bootstrap/app.php");
    write(
        &bootstrap,
        "<?php\n\
         return Application::configure()\n\
        \x20    ->withMiddleware(function (Middleware $middleware) {\n\
        \x20        $middleware->alias([\n\
        \x20            'account.active' => VerifyAccountActive::class,\n\
        \x20            'account.type' => VerifyAccountType::class,\n\
        \x20        ]);\n\
         });\n",
    );

    // Real-world regression: in Laravel 11's bulk form, source_line
    // points at line 4 (`$middleware->alias([`), NOT at the alias
    // entries on subsequent lines. The locator must scan forward.
    let span = locate_alias_on_line(&bootstrap, 4, "account.active")
        .expect("locator must forward-scan to find the alias");

    let content = std::fs::read_to_string(&bootstrap).unwrap();
    let line = content.lines().nth(span.line as usize).unwrap();
    let slice: String = line
        .chars()
        .skip(span.start_column as usize)
        .take((span.end_column - span.start_column) as usize)
        .collect();
    assert_eq!(slice, "account.active");
}

#[test]
fn binding_locator_handles_singleton_call_form() {
    use laravel_lsp::middleware_binding_locator::locate_alias_on_line;

    // Container bindings share the same locator with middleware — both
    // have a quoted name on a single line. This test exercises the
    // typical service-provider register() shape.
    let dir = TempDir::new().unwrap();
    let provider = dir.path().join("app/Providers/AppServiceProvider.php");
    write(
        &provider,
        "<?php\n\
         namespace App\\Providers;\n\n\
         class AppServiceProvider {\n\
        \x20    public function register() {\n\
        \x20        $this->app->singleton('cache.store', function ($app) {\n\
        \x20            return new CacheStore();\n\
        \x20        });\n\
        \x20    }\n\
         }\n",
    );

    // source_line for the binding will point at the line with the
    // `singleton(...)` call. Find the alias name on that line.
    let span =
        locate_alias_on_line(&provider, 6, "cache.store").expect("binding name should locate");
    let content = std::fs::read_to_string(&provider).unwrap();
    let line = content.lines().nth(span.line as usize).unwrap();
    let slice: String = line
        .chars()
        .skip(span.start_column as usize)
        .take((span.end_column - span.start_column) as usize)
        .collect();
    assert_eq!(slice, "cache.store");
}

// ============================================================================
// WorkspaceEdit composition — wire-shape correctness for the rename handler
// ============================================================================

#[test]
fn view_rename_assembles_workspace_edit_with_text_and_file_move() {
    // Mirrors the rename handler's orchestration: a View rename ends
    // with one TextDocumentEdit per caller file plus a single RenameFile
    // op for the .blade.php move. Verify the assembled WorkspaceEdit
    // matches that shape.
    let targets = vec![EditTarget {
        file_path: PathBuf::from("/tmp/app/Http/Controllers/UserController.php"),
        line: 12,
        start_column: 18,
        end_column: 31,
        new_text: "users.account".to_string(),
    }];
    let renames = vec![FileRename {
        old_path: PathBuf::from("/tmp/resources/views/users/profile.blade.php"),
        new_path: PathBuf::from("/tmp/resources/views/users/account.blade.php"),
    }];

    let edit =
        build_rename_workspace_edit(&targets, &renames).expect("workspace edit should be produced");
    let DocumentChanges::Operations(ops) = edit.document_changes.unwrap() else {
        panic!("expected Operations variant when file renames are present");
    };
    assert_eq!(ops.len(), 2, "one text edit doc + one file rename op");
    assert!(
        ops.iter()
            .any(|op| matches!(op, DocumentChangeOperation::Op(ResourceOp::Rename(_)))),
        "must include a RenameFile op"
    );
    assert!(
        ops.iter()
            .any(|op| matches!(op, DocumentChangeOperation::Edit(_))),
        "must include a TextDocumentEdit"
    );
    assert!(
        edit.change_annotations.is_some(),
        "file-rename edits should carry a confirmation annotation"
    );
}

#[test]
fn text_only_rename_emits_edits_variant() {
    // Phase 2's shape: no file renames, just text edits. The emitted
    // WorkspaceEdit should use the simpler `Edits` variant (not
    // `Operations`) so it's compatible with editors that don't enable
    // resource operations.
    let targets = vec![
        EditTarget {
            file_path: PathBuf::from("/tmp/routes/web.php"),
            line: 3,
            start_column: 30,
            end_column: 34,
            new_text: "home".to_string(),
        },
        EditTarget {
            file_path: PathBuf::from("/tmp/app/HomeController.php"),
            line: 12,
            start_column: 18,
            end_column: 22,
            new_text: "home".to_string(),
        },
    ];

    let edit = build_rename_edit(&targets).expect("workspace edit should be produced");
    let changes = edit.document_changes.expect("document_changes populated");
    assert!(
        matches!(changes, DocumentChanges::Edits(ref e) if e.len() == 2),
        "text-only renames should use Edits variant with one entry per file"
    );
    assert!(
        edit.change_annotations.is_none(),
        "text-only renames don't need the file-rename confirmation annotation"
    );
}

#[test]
fn empty_inputs_produce_no_workspace_edit() {
    // A rename that resolves to zero edits + zero file moves is a
    // no-op; the handler should return None so the editor doesn't
    // apply an empty change.
    assert!(build_rename_workspace_edit(&[], &[]).is_none());
}

#[test]
fn rename_file_op_carries_safe_collision_options() {
    // `RenameFile` ops are emitted with `overwrite: false, ignore_if_exists: false`
    // so the client errors if the target already exists rather than
    // silently clobbering. Verify the option flags survive the
    // WorkspaceEdit construction.
    let renames = vec![FileRename {
        old_path: PathBuf::from("/tmp/users/profile.blade.php"),
        new_path: PathBuf::from("/tmp/users/account.blade.php"),
    }];
    let edit = build_rename_workspace_edit(&[], &renames).expect("edit produced");
    let DocumentChanges::Operations(ops) = edit.document_changes.unwrap() else {
        panic!("file-rename edits use Operations variant");
    };
    let rename_op = ops
        .iter()
        .find_map(|op| match op {
            DocumentChangeOperation::Op(ResourceOp::Rename(r)) => Some(r),
            _ => None,
        })
        .expect("expected at least one RenameFile op");
    let options = rename_op.options.as_ref().expect("options should be set");
    assert_eq!(options.overwrite, Some(false));
    assert_eq!(options.ignore_if_exists, Some(false));
}

#[test]
fn mixed_text_and_file_rename_annotates_text_edits_too() {
    // When file renames are present, every text edit also gets wrapped
    // in `AnnotatedTextEdit` referencing the same change annotation —
    // so the client groups them as one reviewable change.
    let targets = vec![EditTarget {
        file_path: PathBuf::from("/tmp/app/UserController.php"),
        line: 0,
        start_column: 18,
        end_column: 31,
        new_text: "users.account".to_string(),
    }];
    let renames = vec![FileRename {
        old_path: PathBuf::from("/tmp/users/profile.blade.php"),
        new_path: PathBuf::from("/tmp/users/account.blade.php"),
    }];
    let edit = build_rename_workspace_edit(&targets, &renames).expect("edit produced");
    let DocumentChanges::Operations(ops) = edit.document_changes.unwrap() else {
        panic!("Operations variant");
    };
    let text_doc_edit = ops
        .iter()
        .find_map(|op| match op {
            DocumentChangeOperation::Edit(e) => Some(e),
            _ => None,
        })
        .expect("expected a TextDocumentEdit");
    let edit_entry = &text_doc_edit.edits[0];
    assert!(
        matches!(edit_entry, OneOf::Right(_)),
        "text edits should be wrapped in AnnotatedTextEdit (Right variant) \
         when file renames are present"
    );
}
