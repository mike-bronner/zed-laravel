//! Unit tests for scope-aware Blade variable rename and controller→view
//! binding rename. Each AC case from issue #55 has at least one test:
//! `@foreach` / `@forelse` / `@for` / `@php` scoping, the
//! `view(..., ['key' => …])` and `compact('key')` patterns, nested scope
//! conflicts, and the multi-controller cross-contamination guard.

use super::*;

// ── helpers ───────────────────────────────────────────────────────────────

#[test]
fn normalize_strips_sigil_and_whitespace() {
    assert_eq!(normalize_new_var_name("  $bar "), "bar");
    assert_eq!(normalize_new_var_name("bar"), "bar");
    assert_eq!(normalize_new_var_name("$user"), "user");
}

#[test]
fn identifier_validation() {
    assert!(is_valid_identifier("foo"));
    assert!(is_valid_identifier("_foo1"));
    assert!(!is_valid_identifier("1foo"));
    assert!(!is_valid_identifier("foo-bar"));
    assert!(!is_valid_identifier(""));
    assert!(!is_valid_identifier("foo bar"));
}

// ── variable_spans ──────────────────────────────────────────────────────────

#[test]
fn finds_variable_occurrences_excluding_property_access() {
    let src = "{{ $user }} and {{ $user->name }} but not $username";
    let spans = variable_spans(src, "user");
    // Two `$user` occurrences; `$user->name` matches the variable, `$username`
    // does not (word boundary).
    assert_eq!(spans.len(), 2);
    // First `$user`: `$` at col 3, name `user` at cols 4..8.
    assert_eq!(spans[0], VarSpan::new(0, 4, 8));
    // Second `$user` in `$user->name`: `$` at col 19, name at 20..24.
    assert_eq!(spans[1], VarSpan::new(0, 20, 24));
}

#[test]
fn variable_spans_skip_blade_comments() {
    let src = "{{ $foo }}\n{{-- $foo is hidden --}}\n{{ $foo }}";
    let spans = variable_spans(src, "foo");
    assert_eq!(spans.len(), 2, "the commented $foo must be ignored");
    assert_eq!(spans[0].line, 0);
    assert_eq!(spans[1].line, 2);
}

#[test]
fn variable_spans_skip_verbatim() {
    let src = "{{ $foo }}\n@verbatim\n{{ $foo }}\n@endverbatim\n{{ $foo }}";
    let spans = variable_spans(src, "foo");
    assert_eq!(spans.len(), 2);
    assert_eq!(spans[0].line, 0);
    assert_eq!(spans[1].line, 4);
}

// ── in_scope_spans: @foreach ────────────────────────────────────────────────

#[test]
fn foreach_scopes_rename_to_the_loop_block() {
    let src = "\
{{ $item }}
@foreach ($items as $item)
    {{ $item->name }}
@endforeach
{{ $item }}";
    // Cursor inside the loop body (line 2) — rename only the in-loop `$item`s.
    let spans = in_scope_spans(src, "item", 2);
    let lines: Vec<u32> = spans.iter().map(|s| s.line).collect();
    assert_eq!(lines, vec![1, 2], "loop directive + body, not lines 0 or 4");
}

#[test]
fn foreach_binding_line_is_in_scope() {
    let src = "\
@foreach ($items as $item)
    {{ $item }}
@endforeach";
    // Cursor on the `@foreach` binding line itself.
    let spans = in_scope_spans(src, "item", 0);
    let lines: Vec<u32> = spans.iter().map(|s| s.line).collect();
    assert_eq!(lines, vec![0, 1]);
}

#[test]
fn file_scoped_variable_skips_loop_rebinding_same_name() {
    // `$item` at file level (line 0) and a loop that re-binds `$item`.
    let src = "\
{{ $item }}
@foreach ($items as $item)
    {{ $item }}
@endforeach
{{ $item }}";
    // Cursor on the file-level `$item` (line 0) — must NOT touch the loop's
    // shadowing `$item` (lines 1–2), only the file-level ones (lines 0, 4).
    let spans = in_scope_spans(src, "item", 0);
    let lines: Vec<u32> = spans.iter().map(|s| s.line).collect();
    assert_eq!(lines, vec![0, 4]);
}

// ── in_scope_spans: @forelse ────────────────────────────────────────────────

#[test]
fn forelse_scopes_rename() {
    let src = "\
@forelse ($users as $user)
    {{ $user->email }}
@empty
    none
@endforelse
{{ $user }}";
    let spans = in_scope_spans(src, "user", 1);
    let lines: Vec<u32> = spans.iter().map(|s| s.line).collect();
    // Body inside forelse, not the trailing line-5 `$user`.
    assert_eq!(lines, vec![0, 1]);
}

// ── in_scope_spans: @for ────────────────────────────────────────────────────

#[test]
fn for_loop_scopes_rename() {
    let src = "\
@for ($i = 0; $i < 3; $i++)
    {{ $i }}
@endfor
{{ $i }}";
    let spans = in_scope_spans(src, "i", 1);
    let lines: Vec<u32> = spans.iter().map(|s| s.line).collect();
    // Lines 0 (three `$i` occurrences) and 1 are in-loop; line 3 is out.
    assert!(lines.iter().all(|&l| l == 0 || l == 1));
    assert!(!lines.contains(&3));
}

// ── in_scope_spans: @php block (file-scoped) ────────────────────────────────

#[test]
fn php_block_variable_is_file_scoped() {
    let src = "\
@php $total = 0; @endphp
{{ $total }}
@foreach ($rows as $row)
    {{ $total }}
@endforeach";
    // `$total` is not loop-introduced, so it is file-scoped: every occurrence
    // renames, including the one inside the loop (the loop doesn't re-bind it).
    let spans = in_scope_spans(src, "total", 0);
    let lines: Vec<u32> = spans.iter().map(|s| s.line).collect();
    assert_eq!(lines, vec![0, 1, 3]);
}

// ── in_scope_spans: nested scope conflict ───────────────────────────────────

#[test]
fn nested_loops_rebinding_same_name_do_not_cross_contaminate() {
    let src = "\
@foreach ($outer as $item)
    {{ $item }}
    @foreach ($inner as $item)
        {{ $item }}
    @endforeach
    {{ $item }}
@endforeach";
    // Cursor in the OUTER loop body (line 1). The inner loop (lines 2–4)
    // re-binds `$item`, so its occurrences (lines 2, 3) are excluded; the
    // outer `$item`s (lines 0, 1, 5) rename.
    let outer = in_scope_spans(src, "item", 1);
    let outer_lines: Vec<u32> = outer.iter().map(|s| s.line).collect();
    assert_eq!(outer_lines, vec![0, 1, 5]);

    // Cursor in the INNER loop body (line 3) — only the inner `$item`s
    // (lines 2, 3) rename.
    let inner = in_scope_spans(src, "item", 3);
    let inner_lines: Vec<u32> = inner.iter().map(|s| s.line).collect();
    assert_eq!(inner_lines, vec![2, 3]);
}

#[test]
fn file_scope_spans_exclude_loop_rebinding() {
    // The controller→view path renames a file-scoped `$user`, but a loop that
    // re-binds `$user` is a separate scope and must be left alone.
    let src = "\
{{ $user->name }}
@foreach ($admins as $user)
    {{ $user }}
@endforeach
{{ $user->email }}";
    let spans = file_scope_spans(src, "user");
    let lines: Vec<u32> = spans.iter().map(|s| s.line).collect();
    assert_eq!(lines, vec![0, 4], "loop-rebound $user (lines 1–2) excluded");
}

// ── view_binding_key_at: array key ──────────────────────────────────────────

const ARRAY_CONTROLLER: &str = "\
<?php
class UserController
{
    public function show()
    {
        return view('users.profile', ['name' => $user->name]);
    }
}";

#[test]
fn array_key_binding_detected_under_cursor() {
    // Line 5, the `name` key sits inside the quotes after `['`.
    // `        return view('users.profile', ['name' => ...`
    // Find the column of `name` inside `['name'`.
    let line = ARRAY_CONTROLLER.lines().nth(5).unwrap();
    let key_quote = line.find("'name'").unwrap();
    let cursor = (key_quote + 2) as u32; // somewhere inside `name`

    let binding = view_binding_key_at(ARRAY_CONTROLLER, 5, cursor).expect("key under cursor");
    assert_eq!(binding.view_name, "users.profile");
    assert_eq!(binding.key, "name");
    assert_eq!(binding.form, BindingForm::ArrayKey);
    // Span covers `name` (4 chars) inside the quotes.
    assert_eq!(binding.key_span.line, 5);
    assert_eq!(
        binding.key_span.end_col - binding.key_span.start_col,
        4,
        "span covers the 4-char key name only"
    );
}

#[test]
fn cursor_on_value_expression_is_not_a_binding_key() {
    let line = ARRAY_CONTROLLER.lines().nth(5).unwrap();
    let value = line.find("$user").unwrap();
    let binding = view_binding_key_at(ARRAY_CONTROLLER, 5, (value + 1) as u32);
    assert!(binding.is_none(), "value side must not classify as a key");
}

#[test]
fn cursor_on_view_name_is_not_a_binding_key() {
    let line = ARRAY_CONTROLLER.lines().nth(5).unwrap();
    let view = line.find("users.profile").unwrap();
    let binding = view_binding_key_at(ARRAY_CONTROLLER, 5, (view + 1) as u32);
    assert!(binding.is_none(), "view name is not a data-binding key");
}

#[test]
fn multiple_controllers_different_keys_do_not_cross_contaminate() {
    // Same view rendered from two controllers under different key names.
    // A controller-initiated rename of one key only rewrites that key's
    // in-view usages; the other key's `$other` is a different identifier and
    // is left untouched (AC: no cross-contamination across key names).
    let view = "\
{{ $name }}
{{ $other }}
{{ $name->email }}";
    let name_spans = file_scope_spans(view, "name");
    let other_spans = file_scope_spans(view, "other");
    let name_lines: Vec<u32> = name_spans.iter().map(|s| s.line).collect();
    let other_lines: Vec<u32> = other_spans.iter().map(|s| s.line).collect();
    assert_eq!(name_lines, vec![0, 2], "only $name usages move");
    assert_eq!(other_lines, vec![1], "the other key's usages are untouched");
    // The two rename sets are disjoint — no span is shared.
    assert!(name_spans.iter().all(|s| !other_spans.contains(s)));
}

// ── view_binding_key_at: compact ────────────────────────────────────────────

const COMPACT_CONTROLLER: &str = "\
<?php
class UserController
{
    public function show()
    {
        $name = $user->name;
        return view('users.profile', compact('name'));
    }
}";

#[test]
fn compact_key_binding_detected_under_cursor() {
    let line = COMPACT_CONTROLLER.lines().nth(6).unwrap();
    let key_quote = line.find("'name'").unwrap();
    let cursor = (key_quote + 2) as u32;

    let binding = view_binding_key_at(COMPACT_CONTROLLER, 6, cursor).expect("compact key");
    assert_eq!(binding.view_name, "users.profile");
    assert_eq!(binding.key, "name");
    assert_eq!(binding.form, BindingForm::Compact);
}

#[test]
fn compact_renames_enclosing_function_local() {
    // The `compact('name')` case must also rename the controller-local `$name`
    // within the enclosing method so the code stays valid.
    let anchor = VarSpan::new(6, 0, 0); // anchor on the `view(...)` line
    let spans = enclosing_function_local_spans(COMPACT_CONTROLLER, "name", anchor);
    // `$name` appears once as the assignment target on line 5.
    let lines: Vec<u32> = spans.iter().map(|s| s.line).collect();
    assert_eq!(lines, vec![5]);
}

#[test]
fn enclosing_local_scope_does_not_leak_across_methods() {
    let src = "\
<?php
class C
{
    public function a()
    {
        $name = 1;
    }
    public function b()
    {
        $name = 2;
        return view('v', compact('name'));
    }
}";
    // Anchor in method b (line 10). Only b's `$name` (line 9) should be found,
    // not a()'s `$name` (line 5).
    let anchor = VarSpan::new(10, 0, 0);
    let spans = enclosing_function_local_spans(src, "name", anchor);
    let lines: Vec<u32> = spans.iter().map(|s| s.line).collect();
    assert_eq!(lines, vec![9]);
}

// ── view_binding_key_at: ->with chains ──────────────────────────────────────

#[test]
fn with_array_chain_binding_detected() {
    let src = "\
<?php
return view('dash')->with(['count' => 3]);";
    let line = src.lines().nth(1).unwrap();
    let key_quote = line.find("'count'").unwrap();
    let binding = view_binding_key_at(src, 1, (key_quote + 2) as u32);
    // ->with chains aren't a direct view() data arg; documented as not yet
    // routed through the key classifier. Assert current behavior so a future
    // expansion updates the test deliberately.
    assert!(binding.is_none());
}
