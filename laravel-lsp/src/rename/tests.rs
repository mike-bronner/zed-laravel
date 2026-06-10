use super::*;
use std::path::PathBuf;
use tower_lsp::lsp_types::{DocumentChangeOperation, DocumentChanges, OneOf, ResourceOp};

fn target(path: &str, line: u32, start: u32, end: u32, new_text: &str) -> EditTarget {
    EditTarget {
        file_path: PathBuf::from(path),
        line,
        start_column: start,
        end_column: end,
        new_text: new_text.to_string(),
    }
}

fn file_rename(old: &str, new: &str) -> FileRename {
    FileRename {
        old_path: PathBuf::from(old),
        new_path: PathBuf::from(new),
    }
}

#[test]
fn can_rename_accepts_enabled_kinds() {
    // The four string-keyed Laravel patterns Phase 2 ships rename for.
    // Each has a declaration locator that finds the source-of-truth
    // position (route_name_locator, config_key_locator,
    // translation_key_locator, env_key_locator).
    assert!(can_rename(&SymbolRef::Route("home".into())));
    assert!(can_rename(&SymbolRef::Config("app.name".into())));
    assert!(can_rename(&SymbolRef::Translation("auth.failed".into())));
    assert!(can_rename(&SymbolRef::Env("APP_KEY".into())));
}

#[test]
fn can_rename_accepts_view_kind() {
    // Phase 3a wired the View file-move pipeline; rename now applies.
    assert!(can_rename(&SymbolRef::View("users.profile".into())));
}

#[test]
fn can_rename_accepts_component_kind() {
    // Phase 3b wired the Blade-component file-move + class-decl pipeline.
    assert!(can_rename(&SymbolRef::Component("button".into())));
}

#[test]
fn can_rename_accepts_livewire_kind() {
    // Phase 3c dispatches over V4 SFC, V4 MFC, V3 Class, Volt.
    assert!(can_rename(&SymbolRef::Livewire("counter".into())));
}

#[test]
fn can_rename_accepts_middleware_kind() {
    // Phase 3e wired the middleware-alias registration-site walker via
    // the unified `middleware_binding_locator`. Symbol shape: the alias
    // string itself (`'auth'` → `'authenticate'`).
    assert!(can_rename(&SymbolRef::Middleware("auth".into())));
}

#[test]
fn can_rename_accepts_binding_kind() {
    // Phase 3e wired container-binding rename through the same locator
    // — the registration-site shape (`$this->app->bind('cache', …)`) is
    // structurally identical to middleware alias registration.
    assert!(can_rename(&SymbolRef::Binding("cache.store".into())));
}

#[test]
fn empty_targets_returns_none() {
    assert!(build_rename_edit(&[]).is_none());
}

#[test]
fn single_target_produces_one_edit() {
    let targets = vec![target("/tmp/routes/web.php", 5, 30, 34, "home2")];
    let edit = build_rename_edit(&targets).expect("expected an edit");
    let changes = edit.changes.expect("changes map populated");
    assert_eq!(changes.len(), 1);
    let edits = changes.values().next().unwrap();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].new_text, "home2");
    assert_eq!(edits[0].range.start.line, 5);
    assert_eq!(edits[0].range.start.character, 30);
    assert_eq!(edits[0].range.end.character, 34);
}

#[test]
fn groups_edits_by_uri() {
    let targets = vec![
        target("/tmp/routes/web.php", 1, 30, 34, "home2"),
        target("/tmp/routes/web.php", 5, 30, 34, "home2"),
        target(
            "/tmp/app/Http/Controllers/HomeController.php",
            10,
            12,
            16,
            "home2",
        ),
    ];
    let edit = build_rename_edit(&targets).expect("expected an edit");
    let changes = edit.changes.expect("changes map populated");
    assert_eq!(changes.len(), 2, "two distinct files → two entries");
    let total: usize = changes.values().map(|v| v.len()).sum();
    assert_eq!(total, 3);
}

#[test]
fn populates_both_changes_and_document_changes() {
    // Modern Zed and Helix prefer documentChanges; older clients fall back
    // to changes. Both should always be populated.
    let targets = vec![target("/tmp/routes/web.php", 1, 30, 34, "home2")];
    let edit = build_rename_edit(&targets).expect("expected an edit");
    assert!(edit.changes.is_some());
    assert!(edit.document_changes.is_some());
}

#[test]
fn text_only_emits_edits_variant_not_operations() {
    // Phase 2 wire shape preservation: when no file renames are present, the
    // builder emits the simpler `DocumentChanges::Edits` form. Anything else
    // would be a visible LSP-message change for routes/config/translation/env
    // renames, and we want zero behavioral drift from Phase 2.
    let targets = vec![target("/tmp/routes/web.php", 1, 30, 34, "home2")];
    let edit = build_rename_edit(&targets).expect("expected an edit");
    match edit.document_changes.expect("document_changes populated") {
        DocumentChanges::Edits(_) => {}
        DocumentChanges::Operations(_) => {
            panic!("text-only rename must keep the Edits variant for Phase 2 parity")
        }
    }
}

#[test]
fn workspace_edit_empty_both_returns_none() {
    assert!(build_rename_workspace_edit(&[], &[]).is_none());
}

#[test]
fn file_renames_only_emit_operations_variant() {
    // Phase 3 view-rename shape when the view has zero call sites — just
    // moves the .blade.php file. Still a valid workspace edit.
    let renames = vec![file_rename(
        "/tmp/resources/views/old.blade.php",
        "/tmp/resources/views/new.blade.php",
    )];
    let edit = build_rename_workspace_edit(&[], &renames).expect("expected an edit");

    // No text edits → `changes` map suppressed entirely.
    assert!(edit.changes.is_none());

    let ops = match edit.document_changes.expect("document_changes populated") {
        DocumentChanges::Operations(ops) => ops,
        DocumentChanges::Edits(_) => panic!("file renames require Operations variant"),
    };
    assert_eq!(ops.len(), 1);
    match &ops[0] {
        DocumentChangeOperation::Op(ResourceOp::Rename(_)) => {}
        _ => panic!("expected a Rename op"),
    }
}

#[test]
fn text_and_file_renames_combine_in_operations() {
    // The Phase 3 view-rename shape with call sites: rewrites every
    // `view('old')` to `view('new')` AND moves the .blade.php file. Both
    // travel in one workspace edit.
    let targets = vec![target(
        "/tmp/app/Http/Controllers/HomeController.php",
        4,
        16,
        21,
        "new",
    )];
    let renames = vec![file_rename(
        "/tmp/resources/views/old.blade.php",
        "/tmp/resources/views/new.blade.php",
    )];
    let edit = build_rename_workspace_edit(&targets, &renames).expect("expected an edit");

    // Text portion still surfaces in `changes` for clients without
    // documentChanges support — the file portion silently no-ops there,
    // which is the expected degraded behavior on older clients.
    assert!(edit.changes.is_some());

    let ops = match edit.document_changes.expect("document_changes populated") {
        DocumentChanges::Operations(ops) => ops,
        DocumentChanges::Edits(_) => panic!("mixed text+file must use Operations"),
    };
    assert_eq!(ops.len(), 2, "one text-edit op + one rename op");

    // Ordering matters: text edits land first (rewriting source while the
    // file is still at its old path) and the rename moves it afterward.
    match &ops[0] {
        DocumentChangeOperation::Edit(_) => {}
        _ => panic!("text edit must precede the file rename"),
    }
    match &ops[1] {
        DocumentChangeOperation::Op(ResourceOp::Rename(_)) => {}
        _ => panic!("rename op must follow the text edit"),
    }
}

#[test]
fn text_only_rename_has_no_change_annotations() {
    // Phase 2 (text-only) renames stay silent-apply — they're small in
    // scope and the always-confirm UX would be obnoxious for every route
    // / config / translation / env rename.
    let targets = vec![target("/tmp/routes/web.php", 1, 30, 34, "home2")];
    let edit = build_rename_edit(&targets).expect("edit");
    assert!(
        edit.change_annotations.is_none(),
        "text-only rename must not request confirmation"
    );
}

#[test]
fn file_rename_emits_change_annotation_with_confirmation() {
    // The signal that triggers Zed's multi-buffer preview. Without this,
    // file moves apply silently and the user can't review what changed.
    let renames = vec![file_rename(
        "/tmp/resources/views/old.blade.php",
        "/tmp/resources/views/new.blade.php",
    )];
    let edit = build_rename_workspace_edit(&[], &renames).expect("edit");

    let annotations = edit
        .change_annotations
        .expect("change_annotations populated");
    assert_eq!(annotations.len(), 1);
    let (_id, annotation) = annotations.iter().next().unwrap();
    assert_eq!(annotation.needs_confirmation, Some(true));
    assert!(!annotation.label.is_empty());
}

#[test]
fn file_rename_op_references_the_annotation_id() {
    // The annotation only works when the resource op's annotation_id
    // matches a key in the annotations map. Defensive: assert they line up.
    let renames = vec![file_rename(
        "/tmp/resources/views/old.blade.php",
        "/tmp/resources/views/new.blade.php",
    )];
    let edit = build_rename_workspace_edit(&[], &renames).expect("edit");

    let annotations = edit.change_annotations.as_ref().unwrap();
    let annotation_keys: Vec<&String> = annotations.keys().collect();

    let ops = match edit.document_changes.as_ref().unwrap() {
        DocumentChanges::Operations(ops) => ops,
        _ => panic!("expected Operations"),
    };
    let rename = match &ops[0] {
        DocumentChangeOperation::Op(ResourceOp::Rename(r)) => r,
        _ => panic!("expected Rename op"),
    };
    let id = rename
        .annotation_id
        .as_ref()
        .expect("rename op must carry an annotation_id");
    assert!(
        annotation_keys.contains(&id),
        "rename op's annotation_id {} not found in annotations map keys",
        id
    );
}

#[test]
fn mixed_text_and_file_rename_annotates_text_edits_too() {
    // For the multi-buffer preview to show every change (not just the
    // file move), the text edits also need the annotation. Verify they
    // land as AnnotatedTextEdit not plain TextEdit.
    let targets = vec![target(
        "/tmp/app/Http/Controllers/HomeController.php",
        4,
        16,
        21,
        "new",
    )];
    let renames = vec![file_rename(
        "/tmp/resources/views/old.blade.php",
        "/tmp/resources/views/new.blade.php",
    )];
    let edit = build_rename_workspace_edit(&targets, &renames).expect("edit");

    let ops = match edit.document_changes.as_ref().unwrap() {
        DocumentChanges::Operations(ops) => ops,
        _ => panic!("expected Operations"),
    };
    let doc_edit = match &ops[0] {
        DocumentChangeOperation::Edit(e) => e,
        _ => panic!("expected text edit first"),
    };
    let edit_entry = &doc_edit.edits[0];
    match edit_entry {
        OneOf::Right(_) => {}
        OneOf::Left(_) => {
            panic!("text edit must be AnnotatedTextEdit when a file rename is in the mix")
        }
    }
}

#[test]
fn rename_op_uses_safe_collision_options() {
    // Mike's collision policy: emit RenameFile with `overwrite: false` and
    // `ignore_if_exists: false` so the client surfaces a loud error when the
    // target path already exists rather than silently clobbering or skipping.
    let renames = vec![file_rename(
        "/tmp/resources/views/old.blade.php",
        "/tmp/resources/views/new.blade.php",
    )];
    let edit = build_rename_workspace_edit(&[], &renames).expect("expected an edit");
    let ops = match edit.document_changes.unwrap() {
        DocumentChanges::Operations(ops) => ops,
        _ => panic!("expected Operations"),
    };
    let rename = match &ops[0] {
        DocumentChangeOperation::Op(ResourceOp::Rename(r)) => r,
        _ => panic!("expected Rename op"),
    };
    let options = rename.options.as_ref().expect("options populated");
    assert_eq!(options.overwrite, Some(false));
    assert_eq!(options.ignore_if_exists, Some(false));
}

#[test]
fn file_rename_skips_unrepresentable_paths() {
    // Defensive: `Url::from_file_path` rejects relative paths. A rename with
    // every path unrepresentable returns None rather than emitting a
    // nonsensical edit.
    let renames = vec![FileRename {
        old_path: PathBuf::from("relative/path.blade.php"),
        new_path: PathBuf::from("other/path.blade.php"),
    }];
    assert!(build_rename_workspace_edit(&[], &renames).is_none());
}

#[test]
fn unsupported_rename_error_returns_generic_message_for_all_kinds() {
    // After Phase 3e wired Middleware + Binding, every classifier-known
    // SymbolRef variant is renameable. `unsupported_rename_error`
    // survives only as a defensive fallback for future symbol kinds
    // that get added without updating `can_rename` — so the message no
    // longer names a specific kind. Just confirm it produces a coherent
    // "not implemented" string that points to the feature-request URL.
    let middleware_err = unsupported_rename_error(&SymbolRef::Middleware("auth".into()));
    assert!(middleware_err.message.contains("not yet implemented"));

    let binding_err = unsupported_rename_error(&SymbolRef::Binding("cache.store".into()));
    assert!(binding_err.message.contains("not yet implemented"));
}

#[test]
fn unsupported_rename_error_points_to_feature_request_url() {
    // Every "not implemented" toast directs the user to the GitHub issues
    // page so they have a clear path to ask for the missing feature.
    let err = unsupported_rename_error(&SymbolRef::Middleware("auth".into()));
    assert!(
        err.message.contains(FEATURE_REQUEST_URL),
        "message should include the feature-request URL: {}",
        err.message
    );
    assert!(
        err.message.contains("feature request"),
        "message should explicitly invite a feature request: {}",
        err.message
    );
}

#[test]
fn unsupported_rename_error_omits_server_name_prefix() {
    // Zed wraps the error with its own attribution ("Error: Prepare rename
    // via laravel-lsp failed: <message>"), so we don't repeat the server
    // name in the body. Keeps the toast tight.
    let err = unsupported_rename_error(&SymbolRef::Middleware("x".into()));
    assert!(
        !err.message.contains("laravel-lsp"),
        "message should not duplicate the server name Zed adds: {}",
        err.message
    );
    assert!(err.message.starts_with("renaming"));
}

#[test]
fn unsupported_rename_error_uses_server_error_code() {
    // Not a standard JSON-RPC code (those are reserved for protocol-level
    // errors) — a server-defined code keeps it out of those buckets so
    // generic LSP client error handlers don't mis-classify the response.
    let err = unsupported_rename_error(&SymbolRef::View("x".into()));
    assert!(matches!(
        err.code,
        tower_lsp::jsonrpc::ErrorCode::ServerError(_)
    ));
}

#[test]
fn supports_per_target_new_text() {
    // For config renames we write the leaf segment at the decl position but
    // the full dotted form at every call site. The two targets share a
    // rename but carry different text.
    let targets = vec![
        // Declaration site in config/app.php: just the leaf segment.
        target("/tmp/config/app.php", 2, 5, 9, "label"),
        // Call site in a controller: the full dotted form.
        target(
            "/tmp/app/Http/Controllers/HomeController.php",
            12,
            16,
            24,
            "app.label",
        ),
    ];
    let edit = build_rename_edit(&targets).expect("edit");
    let changes = edit.changes.expect("changes map populated");
    let mut new_texts: Vec<String> = changes
        .values()
        .flat_map(|v| v.iter().map(|t| t.new_text.clone()))
        .collect();
    new_texts.sort();
    assert_eq!(
        new_texts,
        vec!["app.label".to_string(), "label".to_string()]
    );
}

// ── Magic-member rename (M7) ──────────────────────────────────────────────

use crate::salsa_impl::MagicMemberKind;

#[test]
fn magic_member_decl_name_relationship_is_verbatim() {
    // Relationship method name == usage name → no transform.
    assert_eq!(
        magic_member_decl_name(MagicMemberKind::Relationship, "posts", "articles"),
        "articles"
    );
    assert_eq!(
        magic_member_decl_name(MagicMemberKind::DynamicFinder, "whereEmail", "whereLogin"),
        "whereLogin"
    );
}

#[test]
fn magic_member_decl_name_scope_keeps_affix() {
    // active (scopeActive) → enabled (scopeEnabled).
    assert_eq!(
        magic_member_decl_name(MagicMemberKind::Scope, "scopeActive", "enabled"),
        "scopeEnabled"
    );
    // Multi-word usage name.
    assert_eq!(
        magic_member_decl_name(MagicMemberKind::Scope, "scopeActive", "recently_seen"),
        "scopeRecentlySeen"
    );
}

#[test]
fn magic_member_decl_name_accessor_old_and_new_style() {
    // Old style: getFullNameAttribute → getDisplayNameAttribute.
    assert_eq!(
        magic_member_decl_name(
            MagicMemberKind::Accessor,
            "getFullNameAttribute",
            "display_name"
        ),
        "getDisplayNameAttribute"
    );
    // New style: a camelCase method (fullName(): Attribute) → camelCase.
    assert_eq!(
        magic_member_decl_name(MagicMemberKind::Accessor, "fullName", "display_name"),
        "displayName"
    );
}

#[test]
fn locate_method_name_finds_the_name_token() {
    let src = "<?php\nclass User {\n    public function scopeActive($q) { return $q; }\n}\n";
    // Method on line 2 (0-based); name token columns within the line.
    let (line, start, end) = locate_method_name(src, "scopeActive").expect("found");
    assert_eq!(line, 2);
    let line_text = src.lines().nth(2).unwrap();
    assert_eq!(start as usize, line_text.find("scopeActive").unwrap());
    assert_eq!((end - start) as usize, "scopeActive".len());
}

#[test]
fn locate_method_name_absent_is_none() {
    let src = "<?php\nclass User {\n    public function posts() {}\n}\n";
    assert!(locate_method_name(src, "missing").is_none());
}

#[test]
fn locate_magic_member_declaration_relationship_is_verbatim() {
    // `$user->posts` → the `posts()` method name token.
    let src = "<?php\nclass User {\n    public function posts() { return $this->hasMany(Post::class); }\n}\n";
    let (line, start, end) =
        locate_magic_member_declaration(src, MagicMemberKind::Relationship, "posts")
            .expect("found");
    assert_eq!(line, 2);
    let line_text = src.lines().nth(2).unwrap();
    assert_eq!(start as usize, line_text.find("posts").unwrap());
    assert_eq!((end - start) as usize, "posts".len());
}

#[test]
fn locate_magic_member_declaration_accessor_old_style() {
    // `$user->full_name` → `getFullNameAttribute()`.
    let src = "<?php\nclass User {\n    public function getFullNameAttribute() { return ''; }\n}\n";
    let (line, start, _end) =
        locate_magic_member_declaration(src, MagicMemberKind::Accessor, "full_name")
            .expect("found");
    assert_eq!(line, 2);
    let line_text = src.lines().nth(2).unwrap();
    assert_eq!(
        start as usize,
        line_text.find("getFullNameAttribute").unwrap()
    );
}

#[test]
fn locate_magic_member_declaration_accessor_new_style_falls_through() {
    // No `get…Attribute` method — the new-style camelCase candidate matches.
    let src = "<?php\nclass User {\n    protected function fullName(): Attribute { return Attribute::make(); }\n}\n";
    let (line, start, _end) =
        locate_magic_member_declaration(src, MagicMemberKind::Accessor, "full_name")
            .expect("found");
    assert_eq!(line, 2);
    let line_text = src.lines().nth(2).unwrap();
    assert_eq!(start as usize, line_text.find("fullName").unwrap());
}

#[test]
fn locate_magic_member_declaration_absent_is_none() {
    let src = "<?php\nclass User {\n    public function posts() {}\n}\n";
    assert!(
        locate_magic_member_declaration(src, MagicMemberKind::Relationship, "comments").is_none()
    );
}
