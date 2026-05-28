use super::*;

// ---- arg_kind: what does the cursor's link expect? ---------------------

#[test]
fn arg_kind_where_is_column() {
    assert_eq!(arg_kind("where"), ArgKind::Column);
    assert_eq!(arg_kind("orWhere"), ArgKind::Column);
    assert_eq!(arg_kind("whereIn"), ArgKind::Column);
    assert_eq!(arg_kind("orderBy"), ArgKind::Column);
}

#[test]
fn arg_kind_pluck_is_column_even_though_it_terminates() {
    // pluck() takes a column name as its first arg AND terminates the chain
    // by returning a Collection. ArgKind cares about the first; ChainEffect
    // cares about the second. This test pins that orthogonality.
    assert_eq!(arg_kind("pluck"), ArgKind::Column);
    assert_eq!(chain_effect("pluck"), ChainEffect::FlipToCollection);
}

#[test]
fn arg_kind_with_is_relation() {
    assert_eq!(arg_kind("with"), ArgKind::Relation);
    assert_eq!(arg_kind("load"), ArgKind::Relation);
    assert_eq!(arg_kind("loadMissing"), ArgKind::Relation);
    assert_eq!(arg_kind("has"), ArgKind::Relation);
}

#[test]
fn arg_kind_closure_carriers_win_over_relation() {
    // whereHas appears in both RELATION_METHODS and CLOSURE_CARRIERS.
    // ClosureCarrier wins because the walker needs that signal to descend
    // into the closure scope correctly.
    assert_eq!(arg_kind("whereHas"), ArgKind::ClosureCarrier);
    assert_eq!(arg_kind("whereDoesntHave"), ArgKind::ClosureCarrier);
    assert_eq!(arg_kind("withCount"), ArgKind::ClosureCarrier);
}

#[test]
fn arg_kind_terminators_have_no_arg() {
    // Methods that just terminate the chain don't expose a completable arg.
    assert_eq!(arg_kind("get"), ArgKind::None);
    assert_eq!(arg_kind("first"), ArgKind::None);
    assert_eq!(arg_kind("count"), ArgKind::None);
    assert_eq!(arg_kind("toBase"), ArgKind::None);
}

#[test]
fn arg_kind_transparent_has_no_arg() {
    assert_eq!(arg_kind("clone"), ArgKind::None);
    assert_eq!(arg_kind("tap"), ArgKind::None);
    assert_eq!(arg_kind("when"), ArgKind::None);
}

#[test]
fn arg_kind_unknown_is_none() {
    assert_eq!(arg_kind("totallyMadeUp"), ArgKind::None);
    assert_eq!(arg_kind(""), ArgKind::None);
}

// ---- chain_effect: what does the walker do for prior links? ------------

#[test]
fn chain_effect_to_base_flips_mode() {
    assert_eq!(chain_effect("toBase"), ChainEffect::FlipToBase);
    assert_eq!(chain_effect("getQuery"), ChainEffect::FlipToBase);
}

#[test]
fn chain_effect_collection_terminators() {
    assert_eq!(chain_effect("get"), ChainEffect::FlipToCollection);
    assert_eq!(chain_effect("cursor"), ChainEffect::FlipToCollection);
    assert_eq!(chain_effect("paginate"), ChainEffect::FlipToCollection);
    assert_eq!(
        chain_effect("simplePaginate"),
        ChainEffect::FlipToCollection
    );
    assert_eq!(
        chain_effect("cursorPaginate"),
        ChainEffect::FlipToCollection
    );
    assert_eq!(chain_effect("lazy"), ChainEffect::FlipToCollection);
    // pluck is also FlipToCollection — verified above; cross-checked here.
    assert_eq!(chain_effect("pluck"), ChainEffect::FlipToCollection);
}

#[test]
fn chain_effect_chain_terminators() {
    assert_eq!(chain_effect("first"), ChainEffect::Terminate);
    assert_eq!(chain_effect("find"), ChainEffect::Terminate);
    assert_eq!(chain_effect("findOrFail"), ChainEffect::Terminate);
    assert_eq!(chain_effect("count"), ChainEffect::Terminate);
    assert_eq!(chain_effect("exists"), ChainEffect::Terminate);
    assert_eq!(chain_effect("update"), ChainEffect::Terminate);
    assert_eq!(chain_effect("delete"), ChainEffect::Terminate);
    assert_eq!(chain_effect("dd"), ChainEffect::Terminate);
}

#[test]
fn chain_effect_where_is_passthrough() {
    // where/with/etc. accept args but don't change the chain context.
    assert_eq!(chain_effect("where"), ChainEffect::None);
    assert_eq!(chain_effect("with"), ChainEffect::None);
    assert_eq!(chain_effect("whereHas"), ChainEffect::None);
    assert_eq!(chain_effect("orderBy"), ChainEffect::None);
}

#[test]
fn chain_effect_transparent_is_passthrough() {
    assert_eq!(chain_effect("clone"), ChainEffect::None);
    assert_eq!(chain_effect("tap"), ChainEffect::None);
    assert_eq!(chain_effect("when"), ChainEffect::None);
    assert_eq!(chain_effect("unless"), ChainEffect::None);
}

#[test]
fn chain_effect_unknown_is_passthrough() {
    // Unrecognised methods don't break the chain — they're treated as
    // transparent so the walker keeps the current context.
    assert_eq!(chain_effect("totallyMadeUp"), ChainEffect::None);
    assert_eq!(chain_effect(""), ChainEffect::None);
}

// ---- is_eloquent_static_starter ----------------------------------------

#[test]
fn is_eloquent_static_starter_recognises_idiomatic_forms() {
    // The common forms — not just query() — are all recognised.
    assert!(is_eloquent_static_starter("query"));
    assert!(is_eloquent_static_starter("where"));
    assert!(is_eloquent_static_starter("find"));
    assert!(is_eloquent_static_starter("findOrFail"));
    assert!(is_eloquent_static_starter("first"));
    assert!(is_eloquent_static_starter("firstWhere"));
    assert!(is_eloquent_static_starter("all"));
    assert!(is_eloquent_static_starter("with"));
    assert!(is_eloquent_static_starter("create"));
    assert!(is_eloquent_static_starter("count"));
}

#[test]
fn is_eloquent_static_starter_rejects_unrelated_calls() {
    // Carbon::now(), Str::random(), etc. — these aren't query starters.
    assert!(!is_eloquent_static_starter("now"));
    assert!(!is_eloquent_static_starter("random"));
    assert!(!is_eloquent_static_starter("today"));
    assert!(!is_eloquent_static_starter("frobnicate"));
}

// ---- dedupe sanity ------------------------------------------------------

#[test]
fn column_methods_are_unique() {
    let mut sorted = COLUMN_METHODS.to_vec();
    sorted.sort();
    let mut deduped = sorted.clone();
    deduped.dedup();
    assert_eq!(
        sorted, deduped,
        "COLUMN_METHODS contains duplicates: {:?}",
        sorted
    );
}

#[test]
fn relation_methods_are_unique() {
    let mut sorted = RELATION_METHODS.to_vec();
    sorted.sort();
    let mut deduped = sorted.clone();
    deduped.dedup();
    assert_eq!(
        sorted, deduped,
        "RELATION_METHODS contains duplicates: {:?}",
        sorted
    );
}

#[test]
fn chain_terminators_are_unique() {
    let mut sorted = CHAIN_TERMINATORS.to_vec();
    sorted.sort();
    let mut deduped = sorted.clone();
    deduped.dedup();
    assert_eq!(
        sorted, deduped,
        "CHAIN_TERMINATORS contains duplicates: {:?}",
        sorted
    );
}

// ---- raw SQL methods: first arg is opaque SQL, not a column ------------

#[test]
fn raw_methods_have_none_arg_kind() {
    // Raw-SQL methods take an opaque SQL string as their first arg, not a
    // column name. They must never trigger our column completion — if they
    // did, accepting a suggestion would replace whatever SQL the user was
    // typing with a single column name. The PHP LSP handles completion for
    // raw SQL strings; we yield to it.
    for &name in RAW_METHODS {
        assert_eq!(
            arg_kind(name),
            ArgKind::None,
            "{name} is a raw-SQL method; arg_kind must return None so our \
             column-completion path doesn't fire."
        );
    }
}

#[test]
fn raw_methods_are_not_in_column_methods() {
    // Belt-and-suspenders for `raw_methods_have_none_arg_kind`: if a future
    // contributor adds, say, `whereRaw` to COLUMN_METHODS, this test fires
    // and names the invariant directly. Catches the regression that
    // motivated this whole defensive pattern (havingRaw was historically
    // in COLUMN_METHODS by mistake — see issue #22 / PR #27).
    for &raw in RAW_METHODS {
        assert!(
            !COLUMN_METHODS.contains(&raw),
            "{raw} is in COLUMN_METHODS but it's a raw-SQL method — \
             column completion would clobber the user's SQL. Remove it from \
             COLUMN_METHODS."
        );
    }
}

#[test]
fn raw_methods_are_unique() {
    let mut sorted = RAW_METHODS.to_vec();
    sorted.sort();
    let mut deduped = sorted.clone();
    deduped.dedup();
    assert_eq!(
        sorted, deduped,
        "RAW_METHODS contains duplicates: {:?}",
        sorted
    );
}
