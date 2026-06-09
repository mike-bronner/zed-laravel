//! Tests for intra-procedural flow tracking.
//!
//! Every test calls [`resolve_at`] which parses a snippet, finds the
//! Nth occurrence of `$<var>`, and runs the unified
//! [`crate::query_chain::var_type::resolve`] (which delegates to
//! [`super::resolve`]). The Nth occurrence lets us pin completion-side
//! resolution to a specific use site — assignments before it apply,
//! after it don't.

use crate::parser::parse_php;
use crate::query_chain::use_aliases::extract_use_aliases;
use crate::query_chain::var_type::resolve;

/// Find the Nth (0-indexed) `variable_name` node matching `$var` in the
/// parse tree, depth-first pre-order. Used to pin tests to a specific
/// occurrence of the variable.
fn find_nth_var<'tree>(
    tree: &'tree tree_sitter::Tree,
    bytes: &[u8],
    var: &str,
    n: usize,
) -> Option<tree_sitter::Node<'tree>> {
    let mut stack = vec![tree.root_node()];
    let mut hits = Vec::new();
    // Pre-order DFS, left to right. We push children in reverse so they
    // pop in source order.
    while let Some(node) = stack.pop() {
        if node.kind() == "variable_name" {
            let start = node.start_byte();
            let end = node.end_byte();
            if let Ok(text) = std::str::from_utf8(&bytes[start..end]) {
                if text.trim_start_matches('$') == var {
                    hits.push(node);
                }
            }
        }
        let mut cursor = node.walk();
        let children: Vec<_> = node.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
    hits.into_iter().nth(n)
}

/// Run the full resolution chain against the Nth occurrence of `$var`
/// in the snippet. `n=0` is the first (typically the first assignment's
/// LHS); subsequent values target uses further into the function.
fn resolve_at(src: &str, var: &str, n: usize) -> Option<String> {
    let wrapped = format!("<?php\n{src}");
    let tree = parse_php(&wrapped).expect("parse");
    let bytes = wrapped.as_bytes();
    let aliases = extract_use_aliases(&tree, &wrapped);
    let node = find_nth_var(&tree, bytes, var, n)?;
    resolve(node, bytes, var, &aliases)
}

// ---- Direct seeding -----------------------------------------------------

#[test]
fn flow_simple_seed_from_static_query() {
    // `$q = User::query();` then `$q->where('|')` — flow tracks $q
    // back to User. n=1 picks the use site (after the assignment LHS at
    // n=0).
    let src = r#"
function search() {
    $q = User::query();
    $q->where('email', 'a@b.c');
}
"#;
    assert_eq!(resolve_at(src, "q", 1).as_deref(), Some("User"));
}

#[test]
fn flow_resolves_through_use_alias() {
    // Same as above, but with a `use App\Models\User as Member;` alias.
    // Flow tracking must run RHS through alias resolution.
    let src = r#"
use App\Models\User as Member;

function search() {
    $q = Member::query();
    $q->where('email', 'a@b.c');
}
"#;
    assert_eq!(
        resolve_at(src, "q", 1).as_deref(),
        Some("App\\Models\\User")
    );
}

#[test]
fn flow_simple_seed_from_static_where() {
    // `User::where(...)` is also an Eloquent starter — flow tracks the
    // same way regardless of which static method seeded the chain.
    let src = r#"
function search($email) {
    $q = User::where('email', $email);
    $q->orderBy('name');
}
"#;
    assert_eq!(resolve_at(src, "q", 1).as_deref(), Some("User"));
}

#[test]
fn flow_simple_seed_from_paren_new() {
    // `(new User)->...` form — extract the class from the
    // object_creation_expression. Common when starting a chain off a
    // freshly-constructed model.
    let src = r#"
function search() {
    $q = (new User)->newQuery();
    $q->where('a', 1);
}
"#;
    assert_eq!(resolve_at(src, "q", 1).as_deref(), Some("User"));
}

// ---- Reassignment chains ------------------------------------------------

#[test]
fn flow_reassignment_preserves_type() {
    // `$q = User::query(); $q = $q->where(...); $q->orderBy('|')` —
    // the second assignment's RHS is `$q->where(...)`, which recurses
    // back to the first assignment and finds User. n=3 is the use after
    // both reassignments (n=0 first LHS, n=1 second LHS, n=2 RHS receiver,
    // n=3 final use).
    let src = r#"
function search() {
    $q = User::query();
    $q = $q->where('active', true);
    $q->orderBy('name');
}
"#;
    assert_eq!(resolve_at(src, "q", 3).as_deref(), Some("User"));
}

#[test]
fn flow_reassignment_in_branch_preserves_type() {
    // The motivating example from issue #23: build query in conditional
    // branches. `$query` keeps its User type across `if` blocks because
    // every reassignment to it is `$query->where(...)` which classifies
    // back to User.
    let src = r#"
function search($activeOnly, $search) {
    $query = User::query();

    if ($activeOnly) {
        $query = $query->where('active', true);
    }

    if ($search) {
        $query = $query->where('name', 'like', '%foo%');
    }

    $query->orderBy('name');
}
"#;
    // Last $query occurrence is the use site.
    let wrapped = format!("<?php\n{src}");
    let bytes = wrapped.as_bytes();
    let tree = parse_php(&wrapped).expect("parse");
    let aliases = extract_use_aliases(&tree, &wrapped);
    // Find the final occurrence (largest n).
    let mut last = None;
    let mut n = 0;
    while let Some(node) = find_nth_var(&tree, bytes, "query", n) {
        last = Some(node);
        n += 1;
    }
    let node = last.expect("at least one occurrence");
    assert_eq!(
        resolve(node, bytes, "query", &aliases).as_deref(),
        Some("User")
    );
}

// ---- Clearing behaviour -------------------------------------------------

#[test]
fn flow_clears_on_unknown_rhs() {
    // AC #3: reassignment from a non-Builder type clears tracking. Once
    // flow tracking is cleared, completion falls through to declared
    // types (typed_param / docblock). When neither is present, the
    // result is None — we don't carry forward the prior tracked type.
    //
    // Here: an earlier `$u = User::query()` would normally track $u as
    // User, but the later reassignment to `wat()` (unclassifiable RHS)
    // clears that tracking. No typed param, no docblock → None.
    let src = r#"
function show() {
    $u = User::query();
    $u = wat();
    $u->where('email', 'a@b.c');
}
"#;
    // n=2 is the use site (after both LHS positions at n=0, n=1).
    assert_eq!(resolve_at(src, "u", 2), None);
}

#[test]
fn flow_clears_keeps_typed_param_as_fallback() {
    // When flow is cleared but a typed param exists, the typed param
    // applies. The user declared intent at the function boundary; an
    // unrecognised reassignment doesn't silently invalidate it.
    //
    // This is the "falls back to other resolution" part of AC #3 —
    // the prior FLOW-tracked type is gone, but typed_param survives.
    let src = r#"
function show(User $u) {
    $u = wat();
    $u->where('email', 'a@b.c');
}
"#;
    assert_eq!(resolve_at(src, "u", 2).as_deref(), Some("User"));
}

#[test]
fn flow_clears_keeps_docblock_as_fallback() {
    // Same as above with a docblock instead of a typed param. The
    // docblock declares result-type intent and survives an
    // unrecognised reassignment.
    let src = r#"
function show() {
    /** @var User $u */
    $u = SomeRepository::findOne(1);
    $u->where('email', 'a@b.c');
}
"#;
    assert_eq!(resolve_at(src, "u", 1).as_deref(), Some("User"));
}

#[test]
fn flow_assignment_after_use_does_not_apply() {
    // `$q->where('|')` runs BEFORE `$q = User::query()` in source
    // order — so the assignment doesn't establish $q's type at the use
    // site. Note: this is contrived; PHP would error at runtime, but
    // we shouldn't crash or pretend the later assignment applies.
    let src = r#"
function show($q) {
    $q->where('email', 'a@b.c');
    $q = User::query();
}
"#;
    // n=0 is the LHS of the param, n=1 is the use site (before
    // the assignment).
    assert_eq!(resolve_at(src, "q", 1), None);
}

// ---- Declared-type fallback ---------------------------------------------

#[test]
fn typed_param_wins_when_no_assignment() {
    // No assignment in scope — typed param still works as before
    // (regression check: we haven't broken the Phase 9 behaviour).
    let src = r#"
function show(User $user) {
    $user->newQuery();
}
"#;
    assert_eq!(resolve_at(src, "user", 0).as_deref(), Some("User"));
}

#[test]
fn docblock_with_no_assignment_resolves() {
    // Pure docblock case — variable used directly with only the
    // docblock declaring its type. No assignment means flow doesn't
    // fire and docblock wins.
    let src = r#"
function pull() {
    /** @var User $user */
    extract(['user' => fetch()]);
    $user->newQuery();
}
"#;
    assert_eq!(resolve_at(src, "user", 0).as_deref(), Some("User"));
}

// ---- `use ($var)` capture ----------------------------------------------

#[test]
fn use_clause_captures_typed_outer_param() {
    // AC #4: closure `use ($query)` inherits the captured variable's
    // tracked type. Outer scope has `User $query` typed param; the
    // closure body's `$query->where(...)` should resolve to User by
    // walking out of the closure scope.
    let src = r#"
function show(User $query) {
    collect([])->each(function ($item) use ($query) {
        $query->where('id', $item);
    });
}
"#;
    // Find the $query inside the closure (the LAST occurrence).
    let wrapped = format!("<?php\n{src}");
    let bytes = wrapped.as_bytes();
    let tree = parse_php(&wrapped).expect("parse");
    let aliases = extract_use_aliases(&tree, &wrapped);
    let mut last = None;
    let mut n = 0;
    while let Some(node) = find_nth_var(&tree, bytes, "query", n) {
        last = Some(node);
        n += 1;
    }
    let node = last.expect("at least one $query");
    assert_eq!(
        resolve(node, bytes, "query", &aliases).as_deref(),
        Some("User")
    );
}

#[test]
fn use_clause_captures_flow_tracked_outer_assignment() {
    // The captured variable's type was established by a flow-tracked
    // assignment in the outer scope, not a typed param. Walking out of
    // the closure must re-run flow resolution in the outer scope.
    let src = r#"
function show() {
    $query = User::query();

    collect([])->each(function ($item) use ($query) {
        $query->where('id', $item);
    });
}
"#;
    // Last $query is inside the closure.
    let wrapped = format!("<?php\n{src}");
    let bytes = wrapped.as_bytes();
    let tree = parse_php(&wrapped).expect("parse");
    let aliases = extract_use_aliases(&tree, &wrapped);
    let mut last = None;
    let mut n = 0;
    while let Some(node) = find_nth_var(&tree, bytes, "query", n) {
        last = Some(node);
        n += 1;
    }
    let node = last.expect("at least one $query");
    assert_eq!(
        resolve(node, bytes, "query", &aliases).as_deref(),
        Some("User")
    );
}

#[test]
fn arrow_function_auto_captures_outer_var() {
    // Arrow functions auto-capture by value. `fn ($i) => $query->...`
    // sees $query from the outer scope without an explicit use clause.
    let src = r#"
function show() {
    $query = User::query();

    collect([])->each(fn ($i) => $query->where('id', $i));
}
"#;
    // The last $query is inside the arrow function.
    let wrapped = format!("<?php\n{src}");
    let bytes = wrapped.as_bytes();
    let tree = parse_php(&wrapped).expect("parse");
    let aliases = extract_use_aliases(&tree, &wrapped);
    let mut last = None;
    let mut n = 0;
    while let Some(node) = find_nth_var(&tree, bytes, "query", n) {
        last = Some(node);
        n += 1;
    }
    let node = last.expect("at least one $query");
    assert_eq!(
        resolve(node, bytes, "query", &aliases).as_deref(),
        Some("User")
    );
}

#[test]
fn closure_without_use_does_not_leak_outer_vars() {
    // A plain anonymous function (no `use` clause) doesn't capture
    // anything. If the closure body references `$query`, that's a
    // PHP error — but for our purposes we should refuse to resolve.
    let src = r#"
function show() {
    $query = User::query();

    $cb = function ($item) {
        $query->where('id', $item);
    };
}
"#;
    // Last $query — inside the closure body, no use clause.
    let wrapped = format!("<?php\n{src}");
    let bytes = wrapped.as_bytes();
    let tree = parse_php(&wrapped).expect("parse");
    let aliases = extract_use_aliases(&tree, &wrapped);
    let mut last = None;
    let mut n = 0;
    while let Some(node) = find_nth_var(&tree, bytes, "query", n) {
        last = Some(node);
        n += 1;
    }
    let node = last.expect("at least one $query");
    assert_eq!(resolve(node, bytes, "query", &aliases), None);
}

// ---- Scope isolation ----------------------------------------------------

#[test]
fn nested_closure_assignment_does_not_pollute_outer() {
    // `$q = User::query()` in outer scope. A nested closure reassigns
    // `$q = something()`. The outer use site after the closure must
    // still see User — the inner reassignment is scoped to the closure
    // and doesn't leak out (PHP closures capture by value unless
    // `use (&$var)`, which we don't model).
    let src = r#"
function show() {
    $q = User::query();

    $cb = function ($i) {
        $q = wat();
        $q->where('a', 1);
    };

    $q->where('email', 'a@b.c');
}
"#;
    // Final $q is the outermost use site.
    let wrapped = format!("<?php\n{src}");
    let bytes = wrapped.as_bytes();
    let tree = parse_php(&wrapped).expect("parse");
    let aliases = extract_use_aliases(&tree, &wrapped);
    let mut last = None;
    let mut n = 0;
    while let Some(node) = find_nth_var(&tree, bytes, "q", n) {
        last = Some(node);
        n += 1;
    }
    let node = last.expect("at least one $q");
    assert_eq!(resolve(node, bytes, "q", &aliases).as_deref(), Some("User"));
}

// ---- Recursion / cycle guards ------------------------------------------

#[test]
fn mutual_reassignment_does_not_loop() {
    // Pathological: $a depends on $b, $b depends on $a. Without the
    // visited guard, classify_rhs would recurse forever. With it, we
    // bail and return None.
    let src = r#"
function show() {
    $a = User::query();
    $b = $a->where('x', 1);
    $a = $b->where('y', 2);
    $b = $a->where('z', 3);
    $a->orderBy('id');
}
"#;
    // The last $a is the use site.
    let wrapped = format!("<?php\n{src}");
    let bytes = wrapped.as_bytes();
    let tree = parse_php(&wrapped).expect("parse");
    let aliases = extract_use_aliases(&tree, &wrapped);
    let mut last = None;
    let mut n = 0;
    while let Some(node) = find_nth_var(&tree, bytes, "a", n) {
        last = Some(node);
        n += 1;
    }
    let node = last.expect("at least one $a");
    // The interesting property here is that we don't hang. The exact
    // result depends on resolution order, but we should either resolve
    // back to User (the original seed) or return None safely.
    let _ = resolve(node, bytes, "a", &aliases);
}

// ---- Confidence tiers (M3) ---------------------------------------------

use crate::query_chain::flow::resolve_with_confidence;
use crate::salsa_impl::Confidence;

/// Like `resolve_at`, but reports the confidence tier alongside the FQCN.
fn resolve_conf_at(src: &str, var: &str, n: usize) -> Option<(String, Confidence)> {
    let wrapped = format!("<?php\n{src}");
    let tree = parse_php(&wrapped).expect("parse");
    let bytes = wrapped.as_bytes();
    let aliases = extract_use_aliases(&tree, &wrapped);
    let node = find_nth_var(&tree, bytes, var, n)?;
    resolve_with_confidence(node, bytes, var, &aliases)
}

#[test]
fn confidence_typed_param_is_high() {
    let src = r#"
function show(User $user) {
    $user->where('id', 1);
}
"#;
    let (fqcn, conf) = resolve_conf_at(src, "user", 1).expect("typed param resolves");
    assert_eq!(fqcn, "User");
    assert_eq!(conf, Confidence::High);
}

#[test]
fn confidence_direct_static_assignment_is_high() {
    let src = r#"
function search() {
    $q = User::query();
    $q->where('email', 'a@b.c');
}
"#;
    let (_, conf) = resolve_conf_at(src, "q", 1).expect("direct assignment resolves");
    assert_eq!(conf, Confidence::High);
}

#[test]
fn confidence_paren_new_is_high() {
    let src = r#"
function make() {
    $u = (new User)->newQuery();
    $u->where('id', 1);
}
"#;
    let (_, conf) = resolve_conf_at(src, "u", 1).expect("(new X) resolves");
    assert_eq!(conf, Confidence::High);
}

#[test]
fn confidence_docblock_var_is_high() {
    let src = r#"
function show() {
    /** @var User $user */
    $user = resolve_it();
    $user->where('id', 1);
}
"#;
    let (_, conf) = resolve_conf_at(src, "user", 1).expect("@var resolves");
    assert_eq!(conf, Confidence::High);
}

#[test]
fn confidence_multi_hop_is_medium() {
    // `$a` is seeded from `$b`'s chain, and `$b` from a static call.
    // Resolving `$a` requires an extra assignment hop → indirect flow.
    let src = r#"
function search() {
    $b = User::query();
    $a = $b->where('active', 1);
    $a->orderBy('name');
}
"#;
    let (fqcn, conf) = resolve_conf_at(src, "a", 1).expect("multi-hop resolves");
    assert_eq!(fqcn, "User");
    assert_eq!(
        conf,
        Confidence::Medium,
        "an extra flow hop lowers confidence"
    );
}
