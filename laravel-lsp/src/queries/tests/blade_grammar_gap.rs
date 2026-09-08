//! Tripwire tests for behaviour that depends on unreleased `tree-sitter-blade`.
//!
//! `build.rs` fetches the Blade grammar from `EmranMR/tree-sitter-blade`'s
//! `main` branch, not from a tag. That is deliberate: upstream's newest
//! release (v0.12.3, 2025-08-25) sits 21 commits behind `main`, so pinning a
//! tag would move this crate backwards. The cost of tracking `main` is that an
//! upstream regression arrives with no local change to explain it.
//!
//! These tests are the tripwire for that. Each one drives a public extraction
//! surface of this crate and asserts the behaviour a user sees; none asserts a
//! node kind or a parse-tree shape, which would be a test of upstream's code
//! rather than ours. Every assertion below was run against both grammars: all
//! three tests pass on `main` and fail on v0.12.3, so a grammar regression
//! reddens the suite instead of silently degrading hover and go-to-definition.
//!
//! Two other unreleased upstream changes are deliberately not covered. Scoped
//! slots (upstream PR #133) were probed across sixteen shapes, including
//! scoped, self-closing, dotted, multiline, multiple, single-quoted, and
//! nested inside a component. Every one gave identical `components`, `slots`,
//! `directives`, `echo_php` and embedded-PHP-region output under both
//! grammars, so no assertion here can distinguish them. The `parser.c`
//! regeneration makes no behavioural claim to test.

use super::super::*;
use crate::blade_embedded_php::extract_php_regions;
use crate::parser::{language_blade, language_php, parse_blade, parse_php};

/// Extract Blade patterns from `source`, panicking on parse/query failure.
fn blade_patterns(source: &str) -> ExtractedBladePatterns<'_> {
    let tree = parse_blade(source).expect("Should parse Blade");
    let lang = language_blade();
    // The returned patterns borrow `source`, never `tree`, so dropping the
    // tree at the end of this function is fine.
    extract_all_blade_patterns(&tree, source, &lang).expect("Should extract patterns")
}

/// Collect the `arguments` of every directive named `name`.
fn directive_arguments<'a>(
    patterns: &'a ExtractedBladePatterns<'a>,
    name: &str,
) -> Vec<Option<&'a str>> {
    patterns
        .directives
        .iter()
        .filter(|d| d.directive_name == name)
        .map(|d| d.arguments)
        .collect()
}

/// Blade's `@@` escape renders literal text and compiles nothing, so an
/// escaped directive inside an HTML attribute value must not produce a
/// reference the editor can navigate.
///
/// Upstream PR #129 (unreleased). Against v0.12.3 the escaped form yields an
/// `@include` carrying `'partials.header'`, which surfaces as a phantom view
/// reference to a template the page never includes.
#[test]
fn escaped_directive_in_attribute_value_produces_no_reference() {
    // Positive control: the unescaped form *is* a reference, so the negative
    // assertion below cannot pass merely because attribute values are ignored.
    let live = blade_patterns("<div title=\"@include('partials.header')\">y</div>\n");
    assert_eq!(
        directive_arguments(&live, "include"),
        vec![Some("'partials.header'")],
        "Unescaped @include in an attribute value should be a view reference",
    );

    let escaped = blade_patterns("<div title=\"@@include('partials.header')\">y</div>\n");
    assert!(
        directive_arguments(&escaped, "include").is_empty(),
        "Escaped @@include renders literal text and must produce no view \
         reference, got: {:?}",
        escaped.directives,
    );
}

/// A conditional attribute (`@if(...)` between a tag's angle brackets) must
/// report the same `arguments` as the standalone directive form — the bare
/// expression, with no wrapping parentheses.
///
/// Upstream PR #130 (unreleased). Against v0.12.3 the conditional-attribute
/// form yields `($user->isAdmin)` while the standalone form yields
/// `$user->isAdmin`, so every consumer that re-parses `arguments` as PHP sees
/// a different string depending on where the directive sits.
#[test]
fn conditional_attribute_reports_arguments_without_wrapping_parens() {
    let in_tag = blade_patterns("<div @if($user->isAdmin) class=\"admin\" @endif>y</div>\n");
    assert_eq!(
        directive_arguments(&in_tag, "if"),
        vec![Some("$user->isAdmin")],
        "A conditional attribute should report the bare expression",
    );

    let standalone = blade_patterns("@if($user->isAdmin)\ny\n@endif\n");
    assert_eq!(
        directive_arguments(&in_tag, "if"),
        directive_arguments(&standalone, "if"),
        "Conditional-attribute and standalone @if should agree on arguments",
    );
}

/// The PHP expression inside a conditional attribute must reach the embedded
/// PHP pass, which is what gives members inside it hover and go-to-definition.
///
/// Upstream PR #130 (unreleased). Against v0.12.3 `extract_php_regions`
/// returns nothing for a conditional attribute, so `$user->isAdmin` is
/// invisible to the member-access pipeline that `salsa_impl` and
/// `pattern_indexer` both feed from.
#[test]
fn conditional_attribute_expression_reaches_the_embedded_php_pass() {
    let source = "<div @if($user->isAdmin) class=\"admin\" @endif>y</div>\n";

    let regions = extract_php_regions(source);
    let contents: Vec<&str> = regions.iter().map(|r| r.content.as_str()).collect();
    assert_eq!(
        contents,
        vec!["$user->isAdmin"],
        "The conditional attribute's expression should be an embedded PHP region",
    );
    assert_eq!(
        (regions[0].row, regions[0].column),
        (0, 9),
        "The region should point at the expression, not the directive",
    );

    // Drive the same wrap-and-reparse the Salsa and indexer paths perform, and
    // assert the member the editor would resolve.
    let lang_php = language_php();
    let members: Vec<(String, String)> = regions
        .iter()
        .flat_map(|region| {
            let wrapped = format!("<?php {}", region.content);
            let tree = parse_php(&wrapped).expect("Should parse wrapped PHP");
            let patterns = extract_all_php_patterns(&tree, &wrapped, &lang_php)
                .expect("Should extract PHP patterns");
            patterns
                .member_accesses
                .iter()
                .map(|m| (m.receiver.to_string(), m.member.to_string()))
                .collect::<Vec<_>>()
        })
        .collect();

    assert_eq!(
        members,
        vec![("$user".to_string(), "isAdmin".to_string())],
        "The member inside a conditional attribute should resolve",
    );
}
