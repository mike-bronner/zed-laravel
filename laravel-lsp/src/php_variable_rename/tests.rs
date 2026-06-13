use super::*;
use std::collections::HashSet;
use std::path::PathBuf;

/// Byte offset of the `nth` (0-based) occurrence of `needle`, nudged one byte
/// in so the cursor lands *inside* the `$name` token (on the identifier).
fn cursor_byte(source: &str, needle: &str, nth: usize) -> usize {
    let pos = source
        .match_indices(needle)
        .nth(nth)
        .unwrap_or_else(|| panic!("occurrence {nth} of {needle:?} not found"))
        .0;
    pos + 1
}

/// Absolute byte offset of a 0-based `(line, column)` position. Uses
/// `split_inclusive` so newlines are counted exactly.
fn abs_byte(source: &str, line: u32, col: u32) -> usize {
    let mut offset = 0usize;
    for (i, l) in source.split_inclusive('\n').enumerate() {
        if i as u32 == line {
            return offset + col as usize;
        }
        offset += l.len();
    }
    offset + col as usize
}

/// The source text an edit target rewrites — used to assert every edit lands
/// on a `$name` token, never a stray slice.
fn target_text(source: &str, t: &EditTarget) -> String {
    let line = source.split_inclusive('\n').nth(t.line as usize).unwrap();
    line[t.start_column as usize..t.end_column as usize].to_string()
}

/// The set of absolute byte offsets the targets rewrite.
fn edited_offsets(source: &str, targets: &[EditTarget]) -> HashSet<usize> {
    targets
        .iter()
        .map(|t| abs_byte(source, t.line, t.start_column))
        .collect()
}

fn rename(source: &str, needle: &str, nth: usize, new_name: &str) -> Vec<EditTarget> {
    variable_rename_targets(
        source,
        &PathBuf::from("test.php"),
        cursor_byte(source, needle, nth),
        new_name,
    )
    .expect("rename should not error")
}

// ── Simple function-local rename ──────────────────────────────────────────

#[test]
fn renames_every_in_scope_occurrence() {
    let src = "\
<?php
function greet($user) {
    $user = trim($user);
    return \"Hello \" . $user;
}
";
    let targets = rename(src, "$user", 0, "$account");
    // param + assignment LHS + trim() arg + concatenation = 4 sites.
    assert_eq!(targets.len(), 4, "all four in-scope occurrences");
    for t in &targets {
        assert_eq!(target_text(src, t), "$user");
        assert_eq!(t.new_text, "$account");
    }
}

#[test]
fn rename_accepts_new_name_with_or_without_dollar() {
    let src = "<?php\nfunction f($x) { return $x; }\n";
    let with = rename(src, "$x", 0, "$y");
    let without = rename(src, "$x", 0, "y");
    assert_eq!(with.len(), 2);
    assert_eq!(with, without, "leading $ is optional in the new name");
    assert!(with.iter().all(|t| t.new_text == "$y"));
}

// ── Nested closure isolation ──────────────────────────────────────────────

#[test]
fn nested_closure_without_use_is_isolated() {
    let src = "\
<?php
function outer() {
    $user = 1;
    $fn = function () {
        $user = 2;
        return $user;
    };
    return $user + $fn();
}
";
    // Renaming the OUTER $user touches only the two outer sites.
    let outer = rename(src, "$user", 0, "$person");
    assert_eq!(outer.len(), 2, "outer scope only");

    // The closure's two $user occurrences (the 2nd and 3rd in the file) must
    // be untouched — only the outer #0 and #3 sites get rewritten.
    let closure_user_1 = abs_byte_of_match(src, "$user", 1); // `$user = 2`
    let closure_user_2 = abs_byte_of_match(src, "$user", 2); // `return $user`
    let edited = edited_offsets(src, &outer);
    assert!(!edited.contains(&closure_user_1));
    assert!(!edited.contains(&closure_user_2));
}

#[test]
fn nested_closure_variable_renames_only_itself() {
    let src = "\
<?php
function outer() {
    $user = 1;
    $fn = function () {
        $user = 2;
        return $user;
    };
    return $user + $fn();
}
";
    // Cursor on the closure's own $user (3rd occurrence, 0-based index 2).
    let inner = rename(src, "$user", 2, "$local");
    assert_eq!(inner.len(), 2, "closure scope only");
    // Both edits sit inside the closure (lines 4 and 5).
    assert!(inner.iter().all(|t| t.line == 4 || t.line == 5));
}

#[test]
fn closure_use_clause_cascades() {
    let src = "\
<?php
function make() {
    $count = 0;
    $inc = function () use ($count) {
        return $count + 1;
    };
    return $inc();
}
";
    // Renaming $count must rewrite the assignment, the `use (...)` capture,
    // and the body reference together — otherwise the closure breaks.
    let targets = rename(src, "$count", 0, "$total");
    assert_eq!(targets.len(), 3, "assignment + use-clause + body");
    assert!(targets.iter().all(|t| t.new_text == "$total"));
}

#[test]
fn closure_use_by_reference_cascades() {
    let src = "\
<?php
function make() {
    $count = 0;
    $inc = function () use (&$count) {
        $count++;
    };
    $inc();
    return $count;
}
";
    // The by-reference capture `use (&$count)` binds to the OUTER $count, so a
    // rename must cascade through the assignment, the `use (&...)` capture, the
    // closure body, and the final return — leaving the closure intact. Missing
    // the capture (the `by_ref` wrapper hides the `variable_name`) would sever
    // it and silently corrupt valid code.
    let targets = rename(src, "$count", 0, "$total");
    assert_eq!(
        targets.len(),
        4,
        "assignment + use(&...) capture + body + return"
    );
    assert!(targets.iter().all(|t| t.new_text == "$total"));
    // Every edit lands on the `$count` token only — the `&` reference marker is
    // preserved (`use (&$total)`, not `&$total` mangled).
    for t in &targets {
        assert_eq!(target_text(src, t), "$count");
    }
    // The capture site and the body site must both be among the rewritten
    // offsets — the cascade reaches into the closure, not just the outer scope.
    let edited = edited_offsets(src, &targets);
    let capture_site = abs_byte_of_match(src, "$count", 1); // `use (&$count)`
    let body_site = abs_byte_of_match(src, "$count", 2); // `$count++`
    assert!(
        edited.contains(&capture_site),
        "use(&...) capture rewritten"
    );
    assert!(edited.contains(&body_site), "closure body rewritten");
}

#[test]
fn dynamic_property_access_renames_the_variable_not_the_property() {
    let src = "\
<?php
function f($obj, $key) {
    $val = $obj->$key;
    return $this->{$key} . $val;
}
";
    // `$obj->$key` and `$this->{$key}` use `$key` as a *real local variable*
    // (the dynamic member name), so renaming $key must rewrite all three of its
    // occurrences while leaving the property mechanism (`$obj`, `$this`) and the
    // unrelated `$val` untouched.
    let targets = rename(src, "$key", 0, "$prop");
    assert_eq!(targets.len(), 3, "param + $obj->$key + $this->{{$key}}");
    for t in &targets {
        assert_eq!(target_text(src, t), "$key");
        assert_eq!(t.new_text, "$prop");
    }
    let edited = edited_offsets(src, &targets);
    // The objects and the unrelated local stay put.
    assert!(!edited.contains(&abs_byte_of_match(src, "$obj", 0)));
    assert!(!edited.contains(&abs_byte_of_match(src, "$this", 0)));
    assert!(!edited.contains(&abs_byte_of_match(src, "$val", 0)));
}

// ── Arrow-function captures + shadowing ───────────────────────────────────

#[test]
fn arrow_function_captures_outer_variable() {
    let src = "\
<?php
function calc() {
    $base = 10;
    $add = fn ($x) => $x + $base;
    return $add(5) + $base;
}
";
    // $base is auto-captured by the arrow function — renaming it reaches
    // inside the arrow body.
    let targets = rename(src, "$base", 0, "$origin");
    assert_eq!(targets.len(), 3, "assignment + arrow body + return");
    assert!(targets.iter().all(|t| t.new_text == "$origin"));
}

#[test]
fn arrow_function_parameter_shadows_outer() {
    let src = "\
<?php
function f() {
    $x = 1;
    $g = fn ($x) => $x * 2;
    return $g($x);
}
";
    // Renaming the OUTER $x leaves the arrow's parameter + body untouched.
    let outer = rename(src, "$x", 0, "$seed");
    assert_eq!(outer.len(), 2, "outer scope only (assignment + call arg)");
    let arrow_param = abs_byte_of_match(src, "$x", 1); // `fn ($x)`
    let arrow_body = abs_byte_of_match(src, "$x", 2); // `=> $x * 2`
    let edited = edited_offsets(src, &outer);
    assert!(!edited.contains(&arrow_param));
    assert!(!edited.contains(&arrow_body));

    // Renaming the arrow's own $x touches only the arrow's two occurrences.
    let inner = rename(src, "$x", 1, "$n");
    assert_eq!(inner.len(), 2, "arrow scope only");
    assert!(inner.iter().all(|t| t.line == 3));
}

// ── Property exclusion ────────────────────────────────────────────────────

#[test]
fn properties_are_not_caught_by_variable_rename() {
    let src = "\
<?php
class Account {
    public static $user = 'static';
    public function show($user) {
        $this->user = $user;
        return self::$user . $user;
    }
}
";
    // Renaming the local $user (the method parameter): param + RHS of the
    // assignment + the concatenation = 3 sites. The static property
    // `self::$user` and the object property `$this->user` stay put.
    let targets = rename(src, "$user", 1, "$person");
    assert_eq!(targets.len(), 3, "only the three local-variable sites");
    for t in &targets {
        assert_eq!(target_text(src, t), "$user");
    }

    let edited = edited_offsets(src, &targets);
    // `self::$user` — the `$user` token starts right after `self::`.
    let static_prop = src.match_indices("self::$user").next().unwrap().0 + "self::".len();
    assert!(
        !edited.contains(&static_prop),
        "static property self::$user must be excluded"
    );
    // The static property declaration on line 2 must also be untouched.
    let decl_prop = abs_byte_of_match(src, "$user", 0);
    assert!(!edited.contains(&decl_prop));
}

#[test]
fn this_is_not_renameable() {
    let src = "<?php\nclass C {\n    public function m() { return $this->x; }\n}\n";
    let byte = cursor_byte(src, "$this", 0);
    assert!(variable_at_cursor(src, byte).is_none());
    assert!(
        variable_rename_targets(src, &PathBuf::from("t.php"), byte, "$that")
            .unwrap()
            .is_empty()
    );
}

// ── Validation + prepare-rename range ─────────────────────────────────────

#[test]
fn invalid_new_name_is_an_error() {
    let src = "<?php\nfunction f($x) { return $x; }\n";
    let byte = cursor_byte(src, "$x", 0);
    assert!(variable_rename_targets(src, &PathBuf::from("t.php"), byte, "1bad").is_err());
    assert!(variable_rename_targets(src, &PathBuf::from("t.php"), byte, "$ ").is_err());
}

#[test]
fn renaming_to_same_name_is_a_noop() {
    let src = "<?php\nfunction f($x) { return $x; }\n";
    let byte = cursor_byte(src, "$x", 0);
    assert!(
        variable_rename_targets(src, &PathBuf::from("t.php"), byte, "$x")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn variable_at_cursor_spans_the_whole_token() {
    let src = "<?php\nfunction f($user) { return $user; }\n";
    let byte = cursor_byte(src, "$user", 0);
    let (line, start, end) = variable_at_cursor(src, byte).expect("renameable");
    assert_eq!(line, 1);
    let line_text = src.split_inclusive('\n').nth(1).unwrap();
    assert_eq!(&line_text[start as usize..end as usize], "$user");
}

#[test]
fn variable_at_cursor_none_off_a_variable() {
    let src = "<?php\nfunction greet() { return 1; }\n";
    // Cursor on the function name, not a variable.
    let byte = src.match_indices("greet").next().unwrap().0 + 1;
    assert!(variable_at_cursor(src, byte).is_none());
}

/// Absolute byte offset of the `nth` raw match of `needle` (no cursor nudge).
fn abs_byte_of_match(source: &str, needle: &str, nth: usize) -> usize {
    source
        .match_indices(needle)
        .nth(nth)
        .unwrap_or_else(|| panic!("occurrence {nth} of {needle:?} not found"))
        .0
}
