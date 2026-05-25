use super::*;
use std::path::PathBuf;

fn target(path: &str, line: u32, start: u32, end: u32, new_text: &str) -> EditTarget {
    EditTarget {
        file_path: PathBuf::from(path),
        line,
        start_column: start,
        end_column: end,
        new_text: new_text.to_string(),
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
fn can_rename_rejects_kinds_without_decl_finder() {
    // Class-backed kinds are deferred to Phase 3 (require PHP class
    // rename infrastructure). Middleware and binding aren't gated on
    // Phase 3 per se but they don't have a renameable declaration
    // shape that fits the current model — middleware aliases live in
    // `bootstrap/app.php` `withMiddleware(...)` closures, bindings in
    // service-provider `register()` methods. Both are deferred until
    // a tree-sitter walker for those specific shapes lands.
    assert!(!can_rename(&SymbolRef::View("users.profile".into())));
    assert!(!can_rename(&SymbolRef::Component("button".into())));
    assert!(!can_rename(&SymbolRef::Livewire("counter".into())));
    assert!(!can_rename(&SymbolRef::Middleware("auth".into())));
    assert!(!can_rename(&SymbolRef::Binding("cache.store".into())));
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
