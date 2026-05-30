use super::*;
use std::collections::HashSet;

/// Build a lowercased known-directive set from bare names (no leading `@`).
fn known(names: &[&str]) -> HashSet<String> {
    names.iter().map(|n| n.to_lowercase()).collect()
}

#[test]
fn highlights_known_directives() {
    let set = known(&["if", "endif"]);
    let positions = directive_token_positions("@if ($x)\n@endif", &set);
    assert_eq!(positions, vec![(0, 0, 3), (1, 0, 6)]);
}

#[test]
fn highlights_registered_custom_inline_directive() {
    // `@money` stands in for a custom inline directive registered via
    // Blade::directive() — tree-sitter can't colour these, so this is the
    // case the LSP uniquely covers.
    let set = known(&["money"]);
    let positions = directive_token_positions("<span>@money($total)</span>", &set);
    assert_eq!(positions, vec![(0, 6, 6)]);
}

#[test]
fn matches_custom_names_with_digits_and_underscores() {
    let set = known(&["feature2", "my_directive"]);
    let positions = directive_token_positions("@feature2 @my_directive", &set);
    assert_eq!(positions, vec![(0, 0, 9), (0, 10, 13)]);
}

#[test]
fn rejects_unknown_at_words() {
    // A PHPDoc tag, a CSS at-rule, and an email local-part boundary are all
    // `@word` shaped but none are registered directives.
    let set = known(&["if", "foreach"]);
    assert!(directive_token_positions("@param string $name", &set).is_empty());
    assert!(directive_token_positions("@media (min-width: 1px) {}", &set).is_empty());
    assert!(directive_token_positions("mail hello@example.com", &set).is_empty());
}

#[test]
fn skips_directives_inside_blade_comments() {
    let set = known(&["if", "include"]);
    let positions = directive_token_positions("{{-- @if @include('x') --}}\n@if", &set);
    // Only the live `@if` on line 1 survives; the commented ones are dropped.
    assert_eq!(positions, vec![(1, 0, 3)]);
}

#[test]
fn skips_directives_inside_html_comments() {
    let set = known(&["foreach"]);
    let positions = directive_token_positions("<!-- @foreach --> @foreach", &set);
    assert_eq!(positions, vec![(0, 18, 8)]);
}

#[test]
fn matches_directive_names_case_insensitively() {
    let set = known(&["csrf"]);
    let positions = directive_token_positions("@CSRF", &set);
    assert_eq!(positions, vec![(0, 0, 5)]);
}

#[test]
fn comment_spans_cover_blade_and_html() {
    let spans = blade_comment_spans("a {{-- x --}} b <!-- y --> c");
    assert_eq!(spans.len(), 2);
}

#[test]
fn delta_encodes_multiple_tokens_on_one_line() {
    let set = known(&["if", "csrf"]);
    let tokens = extract_blade_directive_tokens("@if @csrf", &set);
    assert_eq!(tokens.len(), 2);
    // First token: absolute (line 0, col 0), length of "@if".
    assert_eq!(
        (
            tokens[0].delta_line,
            tokens[0].delta_start,
            tokens[0].length
        ),
        (0, 0, 3)
    );
    // Second token: same line, so delta_start is relative (4 - 0), length "@csrf".
    assert_eq!(
        (
            tokens[1].delta_line,
            tokens[1].delta_start,
            tokens[1].length
        ),
        (0, 4, 5)
    );
}

#[test]
fn empty_when_no_known_directives_present() {
    let set = known(&["if"]);
    assert!(directive_token_positions("plain text with no directives", &set).is_empty());
    assert!(extract_blade_directive_tokens("plain text", &set).is_empty());
}
