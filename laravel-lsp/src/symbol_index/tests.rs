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

/// A magic-member entry with explicit column span (the shared `magic` helper
/// fixes columns at 4..12; position tests need control over the span).
fn magic_at(fqcn: &str, member: &str, line: u32, column: u32, end_column: u32) -> MagicMemberEntry {
    MagicMemberEntry {
        fqcn: fqcn.to_string(),
        member: member.to_string(),
        line,
        column,
        end_column,
    }
}

#[test]
fn references_at_returns_full_bucket_for_clicked_usage() {
    let mut idx = SymbolIndex::default();
    let model = PathBuf::from("/proj/app/Models/Post.php");
    let blade = PathBuf::from("/proj/resources/views/blog/index.blade.php");
    // One PHP self-reference + two Blade usages of Post#status.
    idx.insert_magic_members(
        &model,
        &[magic_at("App\\Models\\Post", "status", 65, 22, 28)],
    );
    idx.insert_magic_members(
        &blade,
        &[
            magic_at("App\\Models\\Post", "status", 78, 43, 49),
            magic_at("App\\Models\\Post", "status", 81, 38, 44),
        ],
    );

    // Click inside the member name in the Blade file (col 45 ∈ 43..49).
    let hits = idx.references_at(&blade, 78, 45);
    assert_eq!(
        hits.len(),
        3,
        "should return every reference, not just the click"
    );
    assert!(hits.iter().any(|l| l.file_path == model && l.line == 65));
    assert_eq!(hits.iter().filter(|l| l.file_path == blade).count(), 2);
}

#[test]
fn references_at_matches_span_boundaries_and_misses_outside() {
    let mut idx = SymbolIndex::default();
    let blade = PathBuf::from("/proj/resources/views/x.blade.php");
    idx.insert_magic_members(
        &blade,
        &[magic_at("App\\Models\\Post", "status", 10, 43, 49)],
    );

    assert_eq!(
        idx.references_at(&blade, 10, 43).len(),
        1,
        "start col inclusive"
    );
    assert_eq!(
        idx.references_at(&blade, 10, 49).len(),
        1,
        "end col inclusive"
    );
    assert!(idx.references_at(&blade, 10, 42).is_empty(), "before span");
    assert!(idx.references_at(&blade, 10, 50).is_empty(), "after span");
    assert!(idx.references_at(&blade, 11, 45).is_empty(), "wrong line");
}

#[test]
fn references_at_unknown_file_is_empty() {
    let idx = SymbolIndex::default();
    assert!(idx
        .references_at(&PathBuf::from("/proj/nope.php"), 1, 1)
        .is_empty());
}

#[test]
fn references_at_unions_overlapping_symbols_deduped() {
    let mut idx = SymbolIndex::default();
    let blade = PathBuf::from("/proj/resources/views/u.blade.php");
    let a = PathBuf::from("/proj/app/Models/Post.php");
    let b = PathBuf::from("/proj/app/Models/Draft.php");
    // A union-typed `$item->status`: same position resolves to two classes.
    idx.insert_magic_members(
        &blade,
        &[
            magic_at("App\\Models\\Post", "status", 5, 10, 16),
            magic_at("App\\Models\\Draft", "status", 5, 10, 16),
        ],
    );
    idx.insert_magic_members(&a, &[magic_at("App\\Models\\Post", "status", 1, 0, 6)]);
    idx.insert_magic_members(&b, &[magic_at("App\\Models\\Draft", "status", 2, 0, 6)]);

    let hits = idx.references_at(&blade, 5, 12);
    // Post: blade@5 + a@1 ; Draft: blade@5 + b@2 ; blade@5 deduped once.
    assert_eq!(hits.len(), 3, "union of both symbols, blade site deduped");
    assert!(hits.iter().any(|l| l.file_path == a));
    assert!(hits.iter().any(|l| l.file_path == b));
    assert_eq!(hits.iter().filter(|l| l.file_path == blade).count(), 1);
}

#[test]
fn remove_literal_entries_preserves_magic_members() {
    let mut idx = SymbolIndex::default();
    let path = PathBuf::from("/proj/app/Models/Post.php");
    // Literals (view + route) and a magic member, all from one file.
    idx.insert_file(&path, &fixture("welcome", "home"));
    idx.insert_magic_members(
        &path,
        &[magic_at("App\\Models\\Post", "status", 65, 22, 28)],
    );

    idx.remove_literal_entries(&path);

    // Literals dropped…
    assert!(idx.find(&SymbolRefData::View("welcome".into())).is_empty());
    assert!(idx.find(&SymbolRefData::Route("home".into())).is_empty());
    // …magic member survives.
    let magic_hits = idx.find(&SymbolRefData::MagicMember {
        fqcn: "App\\Models\\Post".into(),
        member: "status".into(),
    });
    assert_eq!(
        magic_hits.len(),
        1,
        "magic member must survive a literal-only eviction"
    );
    // And the position lookup still resolves (by_file retained the magic key).
    assert_eq!(idx.references_at(&path, 65, 24).len(), 1);
}

#[test]
fn remove_literal_entries_then_reinsert_has_no_duplicates() {
    let mut idx = SymbolIndex::default();
    let path = PathBuf::from("/proj/app/Models/Post.php");
    idx.insert_file(&path, &fixture("welcome", "home"));
    idx.insert_magic_members(
        &path,
        &[magic_at("App\\Models\\Post", "status", 65, 22, 28)],
    );

    // Simulate the dirty-drain: evict literals, re-parse, re-insert literals.
    idx.remove_literal_entries(&path);
    idx.insert_file(&path, &fixture("welcome", "home"));

    assert_eq!(
        idx.find(&SymbolRefData::View("welcome".into())).len(),
        1,
        "no literal dupes"
    );
    assert_eq!(
        idx.find(&SymbolRefData::MagicMember {
            fqcn: "App\\Models\\Post".into(),
            member: "status".into()
        })
        .len(),
        1,
        "magic member still present and not duplicated"
    );
}

#[test]
fn remove_literal_entries_drops_by_file_when_no_magic_remains() {
    let mut idx = SymbolIndex::default();
    let path = PathBuf::from("/proj/app/Http/Controllers/Home.php");
    idx.insert_file(&path, &fixture("welcome", "home"));

    idx.remove_literal_entries(&path);

    // Pure-literal file: everything gone, no lingering by_file bucket.
    assert_eq!(idx.indexed_file_count(), 0);
    assert!(idx.find(&SymbolRefData::View("welcome".into())).is_empty());
}

// ─── Chain-aware config/translation ancestor entries ────────────────────

/// `ParsedPatternsData` with a single config ref under `key`.
fn config_fixture(key: &str) -> ParsedPatternsData {
    let mut d = ParsedPatternsData::default();
    d.config_refs
        .push(Arc::new(crate::salsa_impl::ConfigReferenceData {
            key: key.to_string(),
            line: 3,
            column: 8,
            end_column: 8 + key.len() as u32,
        }));
    d
}

#[test]
fn config_ref_counts_toward_every_ancestor_key() {
    let mut idx = SymbolIndex::default();
    let path = PathBuf::from("/proj/app/Jobs/Sync.php");
    idx.insert_file(&path, &config_fixture("reporting.redshift_sync.enabled"));

    // Exact key.
    assert_eq!(
        idx.find(&SymbolRefData::Config(
            "reporting.redshift_sync.enabled".into()
        ))
        .len(),
        1
    );
    // Parent: `config('reporting.redshift_sync.enabled')` reaches THROUGH
    // the `redshift_sync` array, so the parent's lens must count it.
    assert_eq!(
        idx.find(&SymbolRefData::Config("reporting.redshift_sync".into()))
            .len(),
        1
    );
    // Grandparent (file level).
    assert_eq!(
        idx.find(&SymbolRefData::Config("reporting".into())).len(),
        1
    );
    // Non-ancestors must NOT match.
    assert!(idx
        .find(&SymbolRefData::Config("reporting.redshift".into()))
        .is_empty());
    assert!(idx
        .find(&SymbolRefData::Config(
            "reporting.redshift_sync.enabled.x".into()
        ))
        .is_empty());
}

#[test]
fn translation_ref_counts_toward_ancestors() {
    let mut idx = SymbolIndex::default();
    let path = PathBuf::from("/proj/app/Http/Controllers/Home.php");
    let mut d = ParsedPatternsData::default();
    d.translation_refs
        .push(Arc::new(crate::salsa_impl::TranslationReferenceData {
            key: "messages.errors.not_found".to_string(),
            line: 5,
            column: 4,
            end_column: 30,
        }));
    idx.insert_file(&path, &d);

    assert_eq!(
        idx.find(&SymbolRefData::Translation("messages.errors".into()))
            .len(),
        1
    );
    assert_eq!(
        idx.find(&SymbolRefData::Translation("messages".into()))
            .len(),
        1
    );
}

#[test]
fn ancestor_entries_are_evicted_with_the_file() {
    let mut idx = SymbolIndex::default();
    let path = PathBuf::from("/proj/app/Jobs/Sync.php");
    idx.insert_file(&path, &config_fixture("reporting.redshift_sync.enabled"));

    idx.remove_file(&path);

    assert!(idx
        .find(&SymbolRefData::Config("reporting.redshift_sync".into()))
        .is_empty());
    assert_eq!(idx.entry_count(), 0, "no orphaned ancestor entries");
}
