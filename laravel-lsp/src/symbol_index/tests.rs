//! Unit tests for the inverted symbol index.

use super::*;
use crate::salsa_impl::{ParsedPatternsData, RouteReferenceData, SymbolRefData, ViewReferenceData};
use std::path::PathBuf;
use std::sync::Arc;

/// Build a `ParsedPatternsData` with one view and one route, both
/// named for clarity in test failure messages.
fn fixture(view: &str, route: &str) -> ParsedPatternsData {
    let mut d = ParsedPatternsData::default();
    d.views.push(Arc::new(ViewReferenceData {
        name: view.to_string(),
        line: 10,
        column: 5,
        end_column: 20,
        is_route_view: false,
    }));
    d.route_refs.push(Arc::new(RouteReferenceData {
        name: route.to_string(),
        line: 11,
        column: 7,
        end_column: 25,
    }));
    d
}

#[test]
fn insert_then_find_returns_locations() {
    let mut idx = SymbolIndex::default();
    let path = PathBuf::from("/proj/app/controllers/Home.php");
    idx.insert_file(&path, &fixture("welcome", "home"));

    let hits = idx.find(&SymbolRefData::View("welcome".into()));
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].file_path, path);
    assert_eq!(hits[0].line, 10);

    let route_hits = idx.find(&SymbolRefData::Route("home".into()));
    assert_eq!(route_hits.len(), 1);
    assert_eq!(route_hits[0].line, 11);
}

#[test]
fn find_with_unknown_name_returns_empty() {
    let mut idx = SymbolIndex::default();
    idx.insert_file(&PathBuf::from("/proj/a.php"), &fixture("welcome", "home"));

    let hits = idx.find(&SymbolRefData::View("not-a-real-view".into()));
    assert!(hits.is_empty());
}

#[test]
fn find_aggregates_across_multiple_files() {
    let mut idx = SymbolIndex::default();
    idx.insert_file(&PathBuf::from("/proj/a.php"), &fixture("welcome", "home"));
    idx.insert_file(
        &PathBuf::from("/proj/b.php"),
        &fixture("welcome", "different-route"),
    );

    let hits = idx.find(&SymbolRefData::View("welcome".into()));
    assert_eq!(hits.len(), 2, "should aggregate across both files");

    let route_hits = idx.find(&SymbolRefData::Route("home".into()));
    assert_eq!(route_hits.len(), 1, "route only in a.php");
}

#[test]
fn remove_file_strips_only_its_entries() {
    let mut idx = SymbolIndex::default();
    let a = PathBuf::from("/proj/a.php");
    let b = PathBuf::from("/proj/b.php");
    idx.insert_file(&a, &fixture("shared-view", "route-a"));
    idx.insert_file(&b, &fixture("shared-view", "route-b"));

    idx.remove_file(&a);

    // Shared view should still have b's occurrence.
    let view_hits = idx.find(&SymbolRefData::View("shared-view".into()));
    assert_eq!(view_hits.len(), 1);
    assert_eq!(view_hits[0].file_path, b);

    // Route from a is gone; route from b remains.
    assert!(idx.find(&SymbolRefData::Route("route-a".into())).is_empty());
    assert_eq!(idx.find(&SymbolRefData::Route("route-b".into())).len(), 1);
}

#[test]
fn remove_file_drops_empty_buckets_from_forward_map() {
    let mut idx = SymbolIndex::default();
    let path = PathBuf::from("/proj/only-file.php");
    idx.insert_file(&path, &fixture("only-view", "only-route"));

    assert_eq!(idx.distinct_symbol_count(), 2);
    idx.remove_file(&path);
    assert_eq!(
        idx.distinct_symbol_count(),
        0,
        "forward map should drop keys whose Vec is empty after removal"
    );
}

#[test]
fn remove_unknown_file_is_a_noop() {
    let mut idx = SymbolIndex::default();
    idx.insert_file(&PathBuf::from("/proj/a.php"), &fixture("welcome", "home"));

    idx.remove_file(&PathBuf::from("/proj/never-indexed.php"));

    // Original entries should be untouched.
    assert_eq!(idx.find(&SymbolRefData::View("welcome".into())).len(), 1);
}

#[test]
fn re_insert_does_not_double_count() {
    // Real-world: remove + insert is the idiom for file refresh. Test
    // that pattern produces no duplicate locations.
    let mut idx = SymbolIndex::default();
    let path = PathBuf::from("/proj/refresh.php");
    idx.insert_file(&path, &fixture("welcome", "home"));

    idx.remove_file(&path);
    idx.insert_file(&path, &fixture("welcome", "home"));

    let hits = idx.find(&SymbolRefData::View("welcome".into()));
    assert_eq!(
        hits.len(),
        1,
        "refresh idiom should yield exactly one entry"
    );
}

#[test]
fn take_dirty_returns_marked_paths_and_clears() {
    let mut idx = SymbolIndex::default();
    let a = PathBuf::from("/proj/a.php");
    let b = PathBuf::from("/proj/b.php");
    idx.mark_dirty(&a);
    idx.mark_dirty(&b);
    idx.mark_dirty(&a); // duplicate — HashSet should collapse

    let mut taken = idx.take_dirty();
    taken.sort();
    assert_eq!(taken, vec![a, b]);

    // Second take should yield nothing.
    assert!(idx.take_dirty().is_empty());
}

#[test]
fn clear_resets_everything() {
    let mut idx = SymbolIndex::default();
    idx.insert_file(&PathBuf::from("/proj/a.php"), &fixture("welcome", "home"));
    idx.mark_dirty(&PathBuf::from("/proj/b.php"));

    idx.clear();

    assert_eq!(idx.entry_count(), 0);
    assert_eq!(idx.indexed_file_count(), 0);
    assert_eq!(idx.distinct_symbol_count(), 0);
    assert!(idx.take_dirty().is_empty());
}

#[test]
fn distinct_kinds_dont_collide() {
    // A view named "home" and a route named "home" are different symbols
    // — they must live under different forward keys.
    let mut idx = SymbolIndex::default();
    idx.insert_file(&PathBuf::from("/proj/file.php"), &fixture("home", "home"));

    let view_hits = idx.find(&SymbolRefData::View("home".into()));
    let route_hits = idx.find(&SymbolRefData::Route("home".into()));
    assert_eq!(view_hits.len(), 1);
    assert_eq!(route_hits.len(), 1);
    // Both should match their respective occurrence lines.
    assert_eq!(view_hits[0].line, 10);
    assert_eq!(route_hits[0].line, 11);
}

// ─── Magic members (M4) ──────────────────────────────────────────────────

fn magic(fqcn: &str, member: &str, line: u32) -> MagicMemberEntry {
    MagicMemberEntry {
        fqcn: fqcn.to_string(),
        member: member.to_string(),
        line,
        column: 4,
        end_column: 12,
    }
}

#[test]
fn magic_member_insert_then_find() {
    let mut idx = SymbolIndex::default();
    let path = PathBuf::from("/proj/app/Http/Controllers/UserController.php");
    idx.insert_magic_members(
        &path,
        &[
            magic("App\\Models\\User", "email", 10),
            magic("App\\Models\\User", "email", 14),
            magic("App\\Models\\User", "posts", 20),
        ],
    );

    let email = idx.find(&SymbolRefData::MagicMember {
        fqcn: "App\\Models\\User".into(),
        member: "email".into(),
    });
    assert_eq!(email.len(), 2, "two email usages");
    assert!(email.iter().all(|l| l.file_path == path));

    let posts = idx.find(&SymbolRefData::MagicMember {
        fqcn: "App\\Models\\User".into(),
        member: "posts".into(),
    });
    assert_eq!(posts.len(), 1);
}

#[test]
fn magic_member_distinct_declaring_classes_dont_collide() {
    // `email` on User and `email` on Account are different symbols.
    let mut idx = SymbolIndex::default();
    let path = PathBuf::from("/proj/a.php");
    idx.insert_magic_members(
        &path,
        &[
            magic("App\\Models\\User", "email", 1),
            magic("App\\Models\\Account", "email", 2),
        ],
    );

    let user = idx.find(&SymbolRefData::MagicMember {
        fqcn: "App\\Models\\User".into(),
        member: "email".into(),
    });
    let account = idx.find(&SymbolRefData::MagicMember {
        fqcn: "App\\Models\\Account".into(),
        member: "email".into(),
    });
    assert_eq!(user.len(), 1);
    assert_eq!(account.len(), 1);
    assert_eq!(user[0].line, 1);
    assert_eq!(account[0].line, 2);
}

#[test]
fn magic_members_aggregate_across_files() {
    let mut idx = SymbolIndex::default();
    let key = SymbolRefData::MagicMember {
        fqcn: "App\\Models\\User".into(),
        member: "email".into(),
    };
    idx.insert_magic_members(
        &PathBuf::from("/proj/a.php"),
        &[magic("App\\Models\\User", "email", 1)],
    );
    idx.insert_magic_members(
        &PathBuf::from("/proj/b.php"),
        &[magic("App\\Models\\User", "email", 2)],
    );
    assert_eq!(idx.find(&key).len(), 2);
}

#[test]
fn remove_file_evicts_magic_members_alongside_literals() {
    let mut idx = SymbolIndex::default();
    let path = PathBuf::from("/proj/mixed.php");
    // Literal + magic entries from the same file.
    idx.insert_file(&path, &fixture("welcome", "home"));
    idx.insert_magic_members(&path, &[magic("App\\Models\\User", "email", 5)]);

    // Both present.
    assert_eq!(idx.find(&SymbolRefData::View("welcome".into())).len(), 1);
    assert_eq!(
        idx.find(&SymbolRefData::MagicMember {
            fqcn: "App\\Models\\User".into(),
            member: "email".into()
        })
        .len(),
        1
    );

    idx.remove_file(&path);

    // Both gone — magic keys tracked in by_file the same as literals.
    assert!(idx.find(&SymbolRefData::View("welcome".into())).is_empty());
    assert!(idx
        .find(&SymbolRefData::MagicMember {
            fqcn: "App\\Models\\User".into(),
            member: "email".into()
        })
        .is_empty());
}

#[test]
fn reindex_file_replaces_stale_magic_members() {
    // Mirrors the actor's `ReindexFileMagic` sequence (the instant per-file
    // incremental refresh): remove_file → re-insert literals → insert fresh
    // magic. An edit that drops a member must not leave its stale usage behind.
    let mut idx = SymbolIndex::default();
    let path = PathBuf::from("/proj/app/Models/User.php");

    idx.insert_file(&path, &fixture("welcome", "home"));
    idx.insert_magic_members(
        &path,
        &[
            magic("App\\Models\\User", "email", 5),
            magic("App\\Models\\User", "nickname", 6),
        ],
    );
    assert_eq!(
        idx.find(&SymbolRefData::MagicMember {
            fqcn: "App\\Models\\User".into(),
            member: "nickname".into()
        })
        .len(),
        1
    );

    // Re-index: the file no longer references `nickname`, but still `email`.
    idx.remove_file(&path);
    idx.insert_file(&path, &fixture("welcome", "home"));
    idx.insert_magic_members(&path, &[magic("App\\Models\\User", "email", 5)]);

    // Stale `nickname` is gone; `email` survives; literals intact; no dupes.
    assert!(idx
        .find(&SymbolRefData::MagicMember {
            fqcn: "App\\Models\\User".into(),
            member: "nickname".into()
        })
        .is_empty());
    assert_eq!(
        idx.find(&SymbolRefData::MagicMember {
            fqcn: "App\\Models\\User".into(),
            member: "email".into()
        })
        .len(),
        1
    );
    assert_eq!(idx.find(&SymbolRefData::View("welcome".into())).len(), 1);
}
