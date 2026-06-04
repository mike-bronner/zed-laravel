use super::*;

#[test]
fn locates_single_named_leaf_route() {
    let src = r#"<?php
Route::get('/home', [HomeController::class, 'index'])->name('home');
"#;
    let decls = extract_route_name_declarations(src);
    assert_eq!(decls.len(), 1);
    let d = &decls[0];
    assert_eq!(d.full_name, "home");
    assert_eq!(d.local_segment, "home");
    assert_eq!(d.line, 1);
    // Source: `...->name('home');` — the name string content sits between
    // the quotes. Computed column equals byte offset of `h` on line 1.
    assert_eq!(
        &src.lines().nth(1).unwrap()[d.start_column as usize..d.end_column as usize],
        "home"
    );
}

#[test]
fn distinguishes_two_unrelated_names() {
    let src = r#"<?php
Route::get('/a', [A::class, 'x'])->name('a');
Route::get('/b', [B::class, 'x'])->name('b');
"#;
    let decls = extract_route_name_declarations(src);
    assert_eq!(decls.len(), 2);
    assert_eq!(decls[0].full_name, "a");
    assert_eq!(decls[1].full_name, "b");
}

#[test]
fn handles_as_alias() {
    let src = r#"<?php
Route::get('/home', [HomeController::class, 'index'])->as('homepage');
"#;
    let decls = extract_route_name_declarations(src);
    assert_eq!(decls.len(), 1);
    assert_eq!(decls[0].full_name, "homepage");
}

#[test]
fn applies_group_name_prefix_to_full_name() {
    let src = r#"<?php
Route::name('admin.')->group(function () {
    Route::get('/users', [UserController::class, 'index'])->name('users');
});
"#;
    let decls = extract_route_name_declarations(src);
    // The group's own `->name('admin.')` is itself a declaration.
    // Then inside the group, the leaf route declares `->name('users')`,
    // which combined with the inherited prefix yields `admin.users`.
    let names: Vec<&str> = decls.iter().map(|d| d.full_name.as_str()).collect();
    assert!(names.contains(&"admin."));
    assert!(names.contains(&"admin.users"));

    // The *physical source* of the leaf-route name is only `users` — the
    // prefix lives at a separate position and gets its own rename target.
    let leaf = decls.iter().find(|d| d.full_name == "admin.users").unwrap();
    assert_eq!(leaf.local_segment, "users");
}

#[test]
fn returns_empty_for_non_route_chains() {
    let src = r#"<?php
$builder->name('not-a-route')->save();
SomethingElse::name('also-not')->go();
"#;
    let decls = extract_route_name_declarations(src);
    assert!(decls.is_empty(), "non-Route:: chains must not be reported");
}

#[test]
fn returns_empty_for_routes_without_a_name() {
    let src = r#"<?php
Route::get('/anon', function () {});
Route::get('/no-name', [Controller::class, 'method']);
"#;
    let decls = extract_route_name_declarations(src);
    assert!(decls.is_empty());
}

#[test]
fn find_declarations_named_filters() {
    let src = r#"<?php
Route::get('/a', [A::class, 'x'])->name('a');
Route::get('/b', [B::class, 'x'])->name('b');
Route::get('/a-also', [A::class, 'y'])->name('a');
"#;
    // Two declarations share the same full name 'a' (admittedly a Laravel
    // anti-pattern, but the locator should still surface both so a rename
    // touches every position).
    let found = find_declarations_named(src, "a");
    assert_eq!(found.len(), 2);
    assert!(found.iter().all(|d| d.full_name == "a"));
}

#[test]
fn handles_double_quoted_name() {
    let src = r#"<?php
Route::get('/x', [X::class, 'i'])->name("dq.name");
"#;
    let decls = extract_route_name_declarations(src);
    assert_eq!(decls.len(), 1);
    assert_eq!(decls[0].full_name, "dq.name");
    assert_eq!(decls[0].local_segment, "dq.name");
}

#[test]
fn nested_group_prefixes_compose() {
    let src = r#"<?php
Route::name('api.')->group(function () {
    Route::name('v1.')->group(function () {
        Route::get('/users', [U::class, 'i'])->name('users');
    });
});
"#;
    let decls = extract_route_name_declarations(src);
    let names: Vec<&str> = decls.iter().map(|d| d.full_name.as_str()).collect();
    assert!(names.contains(&"api."));
    assert!(names.contains(&"api.v1."));
    assert!(names.contains(&"api.v1.users"));

    // Each individual segment's physical position is just the literal
    // string in source — not the composed name.
    let leaf = decls
        .iter()
        .find(|d| d.full_name == "api.v1.users")
        .unwrap();
    assert_eq!(leaf.local_segment, "users");
}

#[test]
fn rewritten_segment_strips_group_prefix_from_new_name() {
    // Regression: renaming `admin.users` → `admin.dashboard` on a route
    // nested in `Route::name('admin.')->group(...)`. The leaf declaration
    // physically spans only `users`, so it must receive `dashboard`, not
    // the full `admin.dashboard` (which would yield `admin.admin.dashboard`).
    let src = r#"<?php
Route::name('admin.')->group(function () {
    Route::get('/users', [UserController::class, 'index'])->name('users');
});
"#;
    let decls = extract_route_name_declarations(src);
    let leaf = decls.iter().find(|d| d.full_name == "admin.users").unwrap();
    assert_eq!(leaf.rewritten_segment("admin.dashboard"), "dashboard");
}

#[test]
fn rewritten_segment_preserves_dotted_leaf_under_group() {
    // The leaf segment itself is dotted (`users.index`). A naive
    // `rsplit('.').next()` would corrupt it to `index`; prefix stripping
    // keeps the whole local segment intact.
    let src = r#"<?php
Route::name('admin.')->group(function () {
    Route::get('/users', [UserController::class, 'index'])->name('users.index');
});
"#;
    let decls = extract_route_name_declarations(src);
    let leaf = decls
        .iter()
        .find(|d| d.full_name == "admin.users.index")
        .unwrap();
    assert_eq!(leaf.local_segment, "users.index");
    assert_eq!(
        leaf.rewritten_segment("admin.users.list"),
        "users.list",
        "the multi-segment leaf must survive — only the group prefix is dropped"
    );
}

#[test]
fn rewritten_segment_composes_nested_group_prefixes() {
    // Two nested groups (`api.` + `v1.`). The leaf prefix is `api.v1.`.
    let src = r#"<?php
Route::name('api.')->group(function () {
    Route::name('v1.')->group(function () {
        Route::get('/users', [U::class, 'i'])->name('users');
    });
});
"#;
    let decls = extract_route_name_declarations(src);
    let leaf = decls
        .iter()
        .find(|d| d.full_name == "api.v1.users")
        .unwrap();
    assert_eq!(leaf.rewritten_segment("api.v1.accounts"), "accounts");
}

#[test]
fn rewritten_segment_is_verbatim_for_ungrouped_route() {
    // No group prefix → inherited prefix is empty → the new name is written
    // as-is, including a dotted form like `users.index`.
    let src = r#"<?php
Route::get('/home', [HomeController::class, 'index'])->name('home');
"#;
    let decls = extract_route_name_declarations(src);
    let d = &decls[0];
    assert_eq!(d.rewritten_segment("dashboard"), "dashboard");
    assert_eq!(d.rewritten_segment("users.index"), "users.index");
}

// ============================================================================
// External-file group prefixes (issue #43)
// ============================================================================

#[test]
fn find_with_external_matches_prefixed_target() {
    // A file loaded via `Route::as('admin.')->group(base_path('that.php'))`
    // has its in-file `->name('x')` resolve to `admin.x` at the project level.
    // `find_declarations_named_with_external` must match target `admin.x` and
    // still yield the leaf `x` for rewrite.
    let src = r#"<?php
Route::get('/x', [X::class, 'i'])->name('x');
"#;
    let found = find_declarations_named_with_external(
        src,
        "admin.x",
        &["".to_string(), "admin.".to_string()],
    );
    assert_eq!(found.len(), 1, "the prefixed target must match");
    let d = &found[0];
    // The decl is re-anchored to the resolved project-level name so
    // `rewritten_segment` strips the WHOLE prefix and rewrites only the leaf.
    assert_eq!(d.full_name, "admin.x");
    assert_eq!(d.local_segment, "x");
    assert_eq!(d.rewritten_segment("admin.accounts"), "accounts");
}

#[test]
fn find_with_external_still_matches_bare_target() {
    // The same call must keep matching the bare name `x` via the always-present
    // empty prefix — the loaded file is also scanned directly.
    let src = r#"<?php
Route::get('/x', [X::class, 'i'])->name('x');
"#;
    let found =
        find_declarations_named_with_external(src, "x", &["".to_string(), "admin.".to_string()]);
    assert_eq!(found.len(), 1, "the bare target must still match");
    assert_eq!(found[0].full_name, "x");
    assert_eq!(found[0].local_segment, "x");
}

#[test]
fn find_with_external_empty_prefixes_behaves_like_plain() {
    // An empty external-prefix slice is treated as `[""]`, so this is exactly
    // `find_declarations_named`.
    let src = r#"<?php
Route::get('/x', [X::class, 'i'])->name('x');
"#;
    let found = find_declarations_named_with_external(src, "x", &[]);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].full_name, "x");
    let none = find_declarations_named_with_external(src, "admin.x", &[]);
    assert!(none.is_empty(), "no external prefix → no prefixed match");
}

#[test]
fn find_with_external_combines_with_in_file_group_prefix() {
    // External prefix `admin.` + in-file group `api.` → the leaf `x` resolves
    // to `admin.api.x`. The decl's source position still spans only `x`, so the
    // rewrite must strip the full `admin.api.` prefix.
    let src = r#"<?php
Route::name('api.')->group(function () {
    Route::get('/x', [X::class, 'i'])->name('x');
});
"#;
    let found = find_declarations_named_with_external(
        src,
        "admin.api.x",
        &["".to_string(), "admin.".to_string()],
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].full_name, "admin.api.x");
    assert_eq!(found[0].local_segment, "x");
    assert_eq!(found[0].rewritten_segment("admin.api.accounts"), "accounts");
}

#[test]
fn rewritten_segment_falls_back_when_prefix_mismatches() {
    // The user rewrote the group portion (`admin.` → `web.`) — something a
    // leaf rename can't express coherently. Best effort: write the new name
    // verbatim rather than silently dropping a non-matching prefix.
    let decl = RouteNameDeclaration {
        full_name: "admin.users".to_string(),
        local_segment: "users".to_string(),
        line: 0,
        start_column: 0,
        end_column: 5,
    };
    assert_eq!(decl.rewritten_segment("web.dashboard"), "web.dashboard");
}
