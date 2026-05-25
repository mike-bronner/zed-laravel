//! Unit tests for the inverted symbol index.

use super::*;
use crate::salsa_impl::{
    ParsedPatternsData, RouteReferenceData, SymbolRefData, ViewReferenceData,
};
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
    idx.insert_file(
        &PathBuf::from("/proj/a.php"),
        &fixture("welcome", "home"),
    );

    let hits = idx.find(&SymbolRefData::View("not-a-real-view".into()));
    assert!(hits.is_empty());
}

#[test]
fn find_aggregates_across_multiple_files() {
    let mut idx = SymbolIndex::default();
    idx.insert_file(
        &PathBuf::from("/proj/a.php"),
        &fixture("welcome", "home"),
    );
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
    idx.insert_file(
        &PathBuf::from("/proj/a.php"),
        &fixture("welcome", "home"),
    );

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
    assert_eq!(hits.len(), 1, "refresh idiom should yield exactly one entry");
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
    idx.insert_file(
        &PathBuf::from("/proj/a.php"),
        &fixture("welcome", "home"),
    );
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
    idx.insert_file(
        &PathBuf::from("/proj/file.php"),
        &fixture("home", "home"),
    );

    let view_hits = idx.find(&SymbolRefData::View("home".into()));
    let route_hits = idx.find(&SymbolRefData::Route("home".into()));
    assert_eq!(view_hits.len(), 1);
    assert_eq!(route_hits.len(), 1);
    // Both should match their respective occurrence lines.
    assert_eq!(view_hits[0].line, 10);
    assert_eq!(route_hits[0].line, 11);
}
