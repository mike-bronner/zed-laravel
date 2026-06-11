//! Tests for the magic-member reverse dependency index.

use super::*;

fn set(items: &[&str]) -> HashSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn dependents_union_across_classes() {
    let mut idx = MagicDependencyIndex::new();
    idx.replace_file(
        Path::new("/proj/app/Http/Controllers/UserController.php"),
        set(&["App\\Models\\User", "App\\Models\\Profile"]),
    );
    idx.replace_file(
        Path::new("/proj/app/Jobs/SyncUsers.php"),
        set(&["App\\Models\\User"]),
    );
    idx.replace_file(
        Path::new("/proj/app/Services/Billing.php"),
        set(&["App\\Models\\Invoice"]),
    );

    let deps = idx.dependents_of(["App\\Models\\User"].iter().copied());
    assert_eq!(deps.len(), 2);
    assert!(deps.contains(Path::new("/proj/app/Http/Controllers/UserController.php")));
    assert!(deps.contains(Path::new("/proj/app/Jobs/SyncUsers.php")));

    let deps = idx.dependents_of(
        ["App\\Models\\User", "App\\Models\\Invoice"]
            .iter()
            .copied(),
    );
    assert_eq!(deps.len(), 3);

    let deps = idx.dependents_of(["App\\Models\\Unknown"].iter().copied());
    assert!(deps.is_empty());
}

#[test]
fn replace_file_evicts_stale_dependencies() {
    let mut idx = MagicDependencyIndex::new();
    let path = Path::new("/proj/app/Services/Billing.php");
    idx.replace_file(path, set(&["App\\Models\\Invoice", "App\\Models\\User"]));

    // The file stops referencing User — the old edge must disappear.
    idx.replace_file(path, set(&["App\\Models\\Invoice"]));
    assert!(idx
        .dependents_of(["App\\Models\\User"].iter().copied())
        .is_empty());
    assert_eq!(
        idx.dependents_of(["App\\Models\\Invoice"].iter().copied())
            .len(),
        1
    );
}

#[test]
fn replace_with_empty_set_removes_file() {
    let mut idx = MagicDependencyIndex::new();
    let path = Path::new("/proj/app/Services/Billing.php");
    idx.replace_file(path, set(&["App\\Models\\Invoice"]));
    assert_eq!(idx.file_count(), 1);

    idx.replace_file(path, HashSet::new());
    assert_eq!(idx.file_count(), 0);
    assert_eq!(idx.class_count(), 0);
}

#[test]
fn remove_file_prunes_empty_class_entries() {
    let mut idx = MagicDependencyIndex::new();
    let a = Path::new("/proj/a.php");
    let b = Path::new("/proj/b.php");
    idx.replace_file(a, set(&["App\\X"]));
    idx.replace_file(b, set(&["App\\X", "App\\Y"]));

    idx.remove_file(a);
    // X still has b as a dependent; removing b empties both.
    assert_eq!(idx.dependents_of(["App\\X"].iter().copied()).len(), 1);
    idx.remove_file(b);
    assert_eq!(idx.class_count(), 0);
    assert_eq!(idx.file_count(), 0);
}

#[test]
fn clear_resets_everything() {
    let mut idx = MagicDependencyIndex::new();
    idx.replace_file(Path::new("/proj/a.php"), set(&["App\\X"]));
    idx.clear();
    assert_eq!(idx.file_count(), 0);
    assert_eq!(idx.class_count(), 0);
    assert!(idx.dependents_of(["App\\X"].iter().copied()).is_empty());
}
