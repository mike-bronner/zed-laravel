//! Intra-procedural flow-sensitive type tracking for chain receiver variables.
//!
//! Phase 9 (see [`super::var_type`]) resolves a chain-receiver variable's
//! class from its declared form — a typed function parameter or a `@var`
//! docblock. That's enough when the variable is the function arg itself,
//! but breaks down for the very common Laravel pattern of building a
//! query across multiple statements:
//!
//! ```php
//! $query = User::query();
//!
//! if ($activeOnly) {
//!     $query = $query->where('active', true);
//! }
//!
//! $query->orderBy('|');   // ← here, $query is still rooted at User.
//! ```
//!
//! There's no typed param, no docblock — just an assignment chain. This
//! module walks backward from the use site through assignments in the
//! enclosing scope, classifying each right-hand-side expression and
//! propagating the type forward.
//!
//! ## What we recognise as an Eloquent-producing RHS
//!
//! - `Class::query()` / `Class::where(...)` / any Eloquent static
//!   starter on a non-DB class — emits the resolved class FQCN.
//! - `(new Class)->...` — same, with the class extracted from the
//!   constructor expression.
//! - `$other->method(...)->...` — recurses on `$other` using the same
//!   resolution chain (boundary monotonically decreases, so cycles
//!   terminate).
//! - Anything else — emits `None`, which signals "flow tracking is
//!   cleared." The caller then falls through to declared types
//!   (typed_param / docblock) within the same scope — those represent
//!   the user's asserted intent and aren't invalidated by an
//!   unrecognised reassignment. See AC #3 in issue #23.
//!
//! ## `use ($var)` capture
//!
//! Closures with explicit `use ($outer)` clauses inherit the captured
//! variable's tracked type from the parent scope. Arrow functions
//! auto-capture by value — we treat every outer-scope variable mentioned
//! in the body as captured. Methods and top-level functions don't
//! capture at all (their scope is terminal).
//!
//! ## Closeness ordering
//!
//! Within each scope, the order is:
//!
//! 1. Latest assignment to `$var` before the use site (this module).
//! 2. Typed parameter binding `$var` ([`super::var_type::typed_param_in`]).
//! 3. `@var $var` docblock anywhere in scope ([`super::var_type::docblock_in`]).
//!
//! If none of those resolve, and the scope is a closure capturing `$var`,
//! we step out one scope and try again. This is the "closest to use,
//! walking backward" model — a reassignment overrides any declared type
//! because PHP allows free retyping; a declared type is consulted only
//! when no reassignment has happened in this scope.

use tree_sitter::Node;

use super::methods::is_eloquent_static_starter;
use super::use_aliases::{resolve_class_name, UseAliases};
use crate::salsa_impl::Confidence;

/// Cap how many `$a = $b->...; $b = $a->...; ...` hops we'll follow.
/// In practice a single hop is the common case (`$q = $q->where(...)`),
/// and three or four hops covers any sane reassignment chain. Cycles
/// terminate naturally via the monotonically-decreasing byte boundary
/// (see below), so this guard exists only against pathological inputs.
const MAX_RECURSION_DEPTH: usize = 8;

/// Confidence for a resolution that bottomed out at recursion `depth`.
///
/// A receiver resolved directly (depth 0 — a typed param, `@var`, a direct
/// local assignment to a static call / `new`, or a closure-captured var that
/// resolves the same way in its parent scope) is [`Confidence::High`]. One
/// reached only by following assignment-chain hops (`$a = $b->…; $b = …`)
/// is indirect flow → [`Confidence::Medium`]. Closure-capture scope walks do
/// not increment depth, so a captured typed param stays HIGH.
fn confidence_for(depth: usize) -> Confidence {
    if depth == 0 {
        Confidence::High
    } else {
        Confidence::Medium
    }
}

/// Public entry point. Returns the resolved FQCN if `var_name`'s type at
/// `use_site` can be inferred through any of the scope-walking strategies
/// (flow > typed param > docblock, repeated outward through `use()`
/// captures). `None` means we couldn't infer anything — completion
/// callers treat this as "don't fire."
pub fn resolve(
    use_site: Node,
    bytes: &[u8],
    var_name: &str,
    aliases: &UseAliases,
) -> Option<String> {
    resolve_with_confidence(use_site, bytes, var_name, aliases).map(|(fqcn, _)| fqcn)
}

/// Classify a standalone expression node to its Eloquent model FQCN +
/// confidence: a static starter (`User::all()`, `User::find($id)`), a chain on
/// one (`User::query()->where(…)->first()`), or a construction (`new User`,
/// `(new User)->…`). Returns `None` for anything that isn't an Eloquent-
/// producing chain — notably a bare variable, which callers resolve via
/// [`resolve`]/[`resolve_with_confidence`] instead.
///
/// This is the value-expression counterpart to [`resolve`]: it types the
/// *expression itself* rather than looking up a variable's prior assignment.
/// Used to type controller `view()` data and Volt `state()`/`with()`/`computed`
/// values where the model is produced inline.
pub fn resolve_expression(
    node: Node,
    bytes: &[u8],
    aliases: &UseAliases,
) -> Option<(String, Confidence)> {
    classify_rhs(node, bytes, aliases, 0, node.start_byte())
}

/// Like [`resolve`], but also reports the [`Confidence`] tier of the
/// resolution. The magic-member engine (M3) uses this so each resolved site
/// can be confidence-gated by consumers (find-references/lens take HIGH+MEDIUM;
/// rename takes HIGH only).
pub fn resolve_with_confidence(
    use_site: Node,
    bytes: &[u8],
    var_name: &str,
    aliases: &UseAliases,
) -> Option<(String, Confidence)> {
    let before_byte = use_site.start_byte();
    resolve_with_boundary(use_site, before_byte, bytes, var_name, aliases, 0)
}

/// Like `resolve`, but the caller specifies the byte boundary explicitly.
/// When classifying an assignment's RHS, recursion uses the assignment's
/// own start byte as the new boundary — so a recursive lookup of `$q`
/// inside `$q = $q->where(...)` finds an EARLIER `$q = User::query()`
/// rather than re-discovering the assignment we're already inside.
///
/// Because the boundary strictly decreases with each recursive call (or
/// stays the same only when we walk into an outer scope, where it
/// becomes the closure node's own start), cycles terminate naturally.
fn resolve_with_boundary(
    use_site: Node,
    mut before_byte: usize,
    bytes: &[u8],
    var_name: &str,
    aliases: &UseAliases,
    depth: usize,
) -> Option<(String, Confidence)> {
    if depth > MAX_RECURSION_DEPTH {
        return None;
    }

    let mut current = use_site;
    loop {
        let scope = enclosing_scope(current)?;

        // Closest-first within this scope: a local assignment to `$var`
        // is the strongest signal when we can classify its RHS — it
        // reflects the most recent write to the variable. If the RHS
        // can't be classified ("flow tracking is cleared"), we still
        // fall through to declared types (`typed_param` / `docblock`),
        // because those represent the user's stated intent — they're
        // not silently invalidated by an unrecognised reassignment.
        if let Some((rhs, assignment_start)) =
            latest_assignment_rhs(scope, bytes, var_name, before_byte)
        {
            if let Some(resolved) = classify_rhs(rhs, bytes, aliases, depth, assignment_start) {
                return Some(resolved);
            }
        }

        // Declared types — typed param first, then docblock. Both are
        // user-asserted intent that survives an unrecognised
        // reassignment.
        if let Some(raw) = super::var_type::typed_param_in(scope, bytes, var_name) {
            return Some((resolve_class_name(&raw, aliases), confidence_for(depth)));
        }
        if let Some(raw) = super::var_type::docblock_in(scope, bytes, var_name) {
            return Some((resolve_class_name(&raw, aliases), confidence_for(depth)));
        }

        // Nothing for `$var` in this scope. If this scope is a closure
        // that captures `$var` (explicit `use ($var)` or arrow-function
        // auto-capture), step out and try the parent scope. The new
        // boundary becomes the closure's own start — assignments in the
        // outer scope that come BEFORE the closure are visible; the
        // closure itself isn't.
        if !scope_captures_var(scope, bytes, var_name) {
            return None;
        }
        before_byte = scope.start_byte();
        current = scope.parent()?;
    }
}

/// Walk up to the closest function-like ancestor (or the file root).
pub fn enclosing_scope(node: Node) -> Option<Node> {
    let mut cur = Some(node);
    while let Some(n) = cur {
        if matches!(
            n.kind(),
            "function_definition"
                | "method_declaration"
                | "anonymous_function"
                | "anonymous_function_creation_expression"
                | "arrow_function"
                | "program"
        ) {
            return Some(n);
        }
        cur = n.parent();
    }
    None
}

/// Whether `scope` is a closure that captures `var_name` from its parent
/// scope. Returns `false` for non-closure scopes (top-level functions,
/// methods, `program`) — those don't capture anything.
///
/// Three flavors of closure:
///
/// - **Arrow function** (`fn ($x) => ...`): auto-captures every outer
///   variable referenced in the body. We don't try to be precise about
///   which ones — if the body would compile, the variable is captured.
///   Returning `true` for arrow functions delegates the actual lookup
///   to the parent scope.
/// - **Anonymous function with `use ($a, $b)`**: explicit capture list.
///   Variable must appear in the `anonymous_function_use_clause`.
/// - **Anonymous function without `use`**: doesn't capture anything.
fn scope_captures_var(scope: Node, bytes: &[u8], var_name: &str) -> bool {
    match scope.kind() {
        "arrow_function" => true,
        "anonymous_function" | "anonymous_function_creation_expression" => {
            // Find the use clause; if missing, nothing is captured.
            let mut cursor = scope.walk();
            for child in scope.children(&mut cursor) {
                if child.kind() == "anonymous_function_use_clause" {
                    let mut inner = child.walk();
                    for v in child.children(&mut inner) {
                        if v.kind() == "variable_name" {
                            if let Some(t) = node_text(v, bytes) {
                                if t.trim_start_matches('$') == var_name {
                                    return true;
                                }
                            }
                        }
                    }
                    return false;
                }
            }
            false
        }
        _ => false,
    }
}

/// Find the latest `$var_name = <rhs>` assignment in `scope` whose
/// position is strictly before `before_byte`. Returns the RHS node and
/// the assignment's start byte (so callers can use it as the new
/// boundary for recursive resolution into the RHS).
///
/// We don't descend into nested closures / functions — those are their
/// own scopes and an assignment inside them doesn't affect the outer
/// `$var`'s tracking (PHP closures see captured vars by value unless
/// declared `use (&$var)`, and we don't model by-reference captures
/// here).
fn latest_assignment_rhs<'tree>(
    scope: Node<'tree>,
    bytes: &[u8],
    var_name: &str,
    before_byte: usize,
) -> Option<(Node<'tree>, usize)> {
    let mut best: Option<Node> = None;
    let mut best_byte: usize = 0;
    let mut found = false;

    let mut stack: Vec<Node> = vec![scope];
    while let Some(n) = stack.pop() {
        // Skip nested function-like scopes — assignments inside them are
        // local to the inner scope, not visible to the outer chain.
        if n.id() != scope.id()
            && matches!(
                n.kind(),
                "function_definition"
                    | "method_declaration"
                    | "anonymous_function"
                    | "anonymous_function_creation_expression"
                    | "arrow_function"
            )
        {
            continue;
        }

        if n.kind() == "assignment_expression" {
            if let (Some(lhs), Some(rhs)) = (
                n.child_by_field_name("left"),
                n.child_by_field_name("right"),
            ) {
                let is_match = lhs.kind() == "variable_name"
                    && node_text(lhs, bytes)
                        .map(|t| t.trim_start_matches('$') == var_name)
                        .unwrap_or(false)
                    && n.start_byte() < before_byte;
                if is_match && (!found || n.start_byte() >= best_byte) {
                    best = Some(rhs);
                    best_byte = n.start_byte();
                    found = true;
                }
            }
        }

        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }

    best.map(|rhs| (rhs, best_byte))
}

/// Classify an RHS expression as an Eloquent-producing chain and return
/// the model FQCN. Three recognised shapes — anything else returns
/// `None`, which clears flow tracking for the caller's variable.
///
/// `boundary_before` is the start byte of the assignment whose RHS we're
/// classifying. When this RHS references another variable that needs
/// resolution, we pass `boundary_before` as the new lookup boundary so
/// the recursion looks for assignments strictly before this one — not
/// inside the RHS itself.
fn classify_rhs(
    rhs: Node,
    bytes: &[u8],
    aliases: &UseAliases,
    depth: usize,
    boundary_before: usize,
) -> Option<(String, Confidence)> {
    let node = unwrap_parens(rhs);
    match node.kind() {
        // `User::query()` — bottom of a chain or single static call.
        "scoped_call_expression" => {
            classify_scoped_call(node, bytes, aliases).map(|c| (c, confidence_for(depth)))
        }

        // `$x->...` chain. Walk down to the chain root and dispatch.
        "member_call_expression" => {
            let root = chain_root(node);
            match root.kind() {
                "scoped_call_expression" => {
                    classify_scoped_call(root, bytes, aliases).map(|c| (c, confidence_for(depth)))
                }
                "variable_name" => {
                    let raw = node_text(root, bytes)?;
                    let inner = raw.trim_start_matches('$').to_string();
                    // Resolving another variable is an extra flow hop — the
                    // deeper recursion carries the (lower) confidence.
                    resolve_with_boundary(root, boundary_before, bytes, &inner, aliases, depth + 1)
                }
                "parenthesized_expression" => {
                    // Unwrapping parens is syntactic, not a flow hop — keep the
                    // same depth so `(new User)->newQuery()` stays HIGH.
                    let inner = unwrap_parens(root);
                    classify_rhs(inner, bytes, aliases, depth, boundary_before)
                }
                _ => None,
            }
        }

        // `new Class` — bare, without parens. `(new Class)->...` lands
        // here too after `unwrap_parens` strips the outer parens.
        "object_creation_expression" => {
            extract_new_class(node, bytes, aliases).map(|c| (c, confidence_for(depth)))
        }

        _ => None,
    }
}

/// Walk down the `object` field of nested `member_call_expression`s
/// until we hit a non-call node — that's the chain receiver.
fn chain_root(mut node: Node) -> Node {
    loop {
        if node.kind() == "member_call_expression" {
            match node.child_by_field_name("object") {
                Some(o) => node = o,
                None => return node,
            }
        } else {
            return node;
        }
    }
}

/// Classify a static call like `User::query()`. Only emits the class
/// name when the method is on the Eloquent static-starter list and the
/// class isn't the DB facade (those produce base-builder receivers, not
/// Eloquent models — handled elsewhere).
fn classify_scoped_call(node: Node, bytes: &[u8], aliases: &UseAliases) -> Option<String> {
    let scope = node.child_by_field_name("scope")?;
    let name = node.child_by_field_name("name")?;
    let method = node_text(name, bytes)?;
    if !is_eloquent_static_starter(method) {
        return None;
    }
    let class_text = node_text(scope, bytes)?;
    let basename = class_text.rsplit('\\').next().unwrap_or(class_text);
    if basename.eq_ignore_ascii_case("DB") {
        return None;
    }
    Some(resolve_class_name(class_text, aliases))
}

/// Extract the class name from `new Class(...)`. Mirrors the
/// `parenthesized_receiver` logic in [`super::extractor`] but without the
/// `self` / `static` / `parent` handling — flow tracking for those is
/// rare enough to skip in this phase.
fn extract_new_class(node: Node, bytes: &[u8], aliases: &UseAliases) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "name" | "qualified_name" | "relative_name" => {
                let raw = node_text(child, bytes)?;
                return Some(resolve_class_name(raw, aliases));
            }
            _ => {}
        }
    }
    None
}

fn unwrap_parens(mut node: Node) -> Node {
    while node.kind() == "parenthesized_expression" {
        match node.named_child(0) {
            Some(inner) => node = inner,
            None => return node,
        }
    }
    node
}

fn node_text<'a>(node: Node<'_>, bytes: &'a [u8]) -> Option<&'a str> {
    let start = node.start_byte();
    let end = node.end_byte();
    std::str::from_utf8(bytes.get(start..end)?).ok()
}

#[cfg(test)]
mod tests;
