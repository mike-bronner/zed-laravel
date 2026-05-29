//! Tests for `is_column_surface_sparse` — the wire-up helper that decides
//! whether Phase 3 should supplement the source-derived column list with
//! the live DB schema.
//!
//! Sparse = developer-declared-nothing case (typically `$guarded = []`
//! with no `$fillable`/`$casts`/etc). Only framework conventions and
//! parent-class implications survived `compute_column_surface`. Rich =
//! any explicit user declaration is present.

use laravel_lsp::laravel_introspector::{ColumnInfo, ColumnSource};

fn col(source: ColumnSource) -> ColumnInfo {
    ColumnInfo {
        name: "x".to_string(),
        php_type: "mixed".to_string(),
        source,
    }
}

#[test]
fn empty_surface_is_sparse() {
    assert!(crate::is_column_surface_sparse(&[]));
}

#[test]
fn convention_only_is_sparse() {
    let cols = vec![col(ColumnSource::Convention), col(ColumnSource::Convention)];
    assert!(crate::is_column_surface_sparse(&cols));
}

#[test]
fn parent_class_only_is_sparse() {
    // Authenticatable's columns alone don't count as user intent.
    let cols = vec![
        col(ColumnSource::ParentClass),
        col(ColumnSource::ParentClass),
    ];
    assert!(crate::is_column_surface_sparse(&cols));
}

#[test]
fn convention_plus_parent_class_is_sparse() {
    // The typical default `User extends Authenticatable {}` shape:
    // id/created_at/updated_at + auth columns. Still sparse.
    let cols = vec![
        col(ColumnSource::Convention),
        col(ColumnSource::ParentClass),
        col(ColumnSource::Convention),
    ];
    assert!(crate::is_column_surface_sparse(&cols));
}

#[test]
fn fillable_makes_surface_rich() {
    let cols = vec![col(ColumnSource::Convention), col(ColumnSource::Fillable)];
    assert!(!crate::is_column_surface_sparse(&cols));
}

#[test]
fn casts_make_surface_rich() {
    let cols = vec![col(ColumnSource::Convention), col(ColumnSource::Cast)];
    assert!(!crate::is_column_surface_sparse(&cols));
}

#[test]
fn dates_make_surface_rich() {
    let cols = vec![col(ColumnSource::Dates)];
    assert!(!crate::is_column_surface_sparse(&cols));
}

#[test]
fn attributes_make_surface_rich() {
    let cols = vec![col(ColumnSource::Attributes)];
    assert!(!crate::is_column_surface_sparse(&cols));
}

#[test]
fn trait_signal_makes_surface_rich() {
    // SoftDeletes alone is enough — the developer explicitly opted in
    // to a behaviour that pulls a column in. That's intent.
    let cols = vec![col(ColumnSource::Convention), col(ColumnSource::Trait)];
    assert!(!crate::is_column_surface_sparse(&cols));
}
