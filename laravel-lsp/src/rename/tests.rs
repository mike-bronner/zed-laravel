use super::*;
use std::path::PathBuf;

fn target(path: &str, line: u32, start: u32, end: u32) -> EditTarget {
    EditTarget {
        file_path: PathBuf::from(path),
        line,
        start_column: start,
        end_column: end,
    }
}

#[test]
fn can_rename_accepts_routes() {
    // First wave: routes have a declaration-site walker (route_name_locator).
    assert!(can_rename(&SymbolRef::Route("home".into())));
}

#[test]
fn can_rename_rejects_kinds_without_decl_finder() {
    // Config + translation are gated until their respective decl-site
    // walkers ship. Phase 3 kinds are gated indefinitely (require PHP class
    // rename infra).
    assert!(!can_rename(&SymbolRef::Config("app.name".into())));
    assert!(!can_rename(&SymbolRef::Translation("auth.failed".into())));
    assert!(!can_rename(&SymbolRef::View("users.profile".into())));
    assert!(!can_rename(&SymbolRef::Component("button".into())));
    assert!(!can_rename(&SymbolRef::Livewire("counter".into())));
    assert!(!can_rename(&SymbolRef::Env("APP_KEY".into())));
    assert!(!can_rename(&SymbolRef::Middleware("auth".into())));
    assert!(!can_rename(&SymbolRef::Binding("cache.store".into())));
}

#[test]
fn empty_targets_returns_none() {
    assert!(build_rename_edit("new", &[]).is_none());
}

#[test]
fn single_target_produces_one_edit() {
    let targets = vec![target("/tmp/routes/web.php", 5, 30, 34)];
    let edit = build_rename_edit("home2", &targets).expect("expected an edit");
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
        target("/tmp/routes/web.php", 1, 30, 34),
        target("/tmp/routes/web.php", 5, 30, 34),
        target("/tmp/app/Http/Controllers/HomeController.php", 10, 12, 16),
    ];
    let edit = build_rename_edit("home2", &targets).expect("expected an edit");
    let changes = edit.changes.expect("changes map populated");
    assert_eq!(changes.len(), 2, "two distinct files → two entries");
    let total: usize = changes.values().map(|v| v.len()).sum();
    assert_eq!(total, 3);
}

#[test]
fn populates_both_changes_and_document_changes() {
    // Modern Zed and Helix prefer documentChanges; older clients fall back
    // to changes. Both should always be populated.
    let targets = vec![target("/tmp/routes/web.php", 1, 30, 34)];
    let edit = build_rename_edit("home2", &targets).expect("expected an edit");
    assert!(edit.changes.is_some());
    assert!(edit.document_changes.is_some());
}
