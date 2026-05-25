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
