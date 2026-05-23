use super::*;
use std::fs;
use tempfile::TempDir;

#[test]
fn captures_single_line_declaration() {
    let src = "<div>\n@props(['user' => null, 'showAvatar' => true])\n<h1>...</h1>\n</div>\n";
    let got = extract_props_directive_from_source(src).expect("should find @props");
    assert_eq!(got, "@props(['user' => null, 'showAvatar' => true])");
}

#[test]
fn captures_multiline_declaration() {
    let src = "@props([\n    'user' => null,\n    'showAvatar' => true,\n])\n<div></div>\n";
    let got = extract_props_directive_from_source(src).expect("should find @props");
    assert!(got.starts_with("@props(["));
    assert!(got.ends_with("])"));
    assert!(got.contains("'user' => null"));
    assert!(got.contains("'showAvatar' => true"));
}

#[test]
fn returns_none_when_directive_absent() {
    let src = "<div>no props here</div>\n";
    assert_eq!(extract_props_directive_from_source(src), None);
}

#[test]
fn ignores_strings_containing_parens() {
    // Embedded `(` / `)` inside a string literal must not throw off the
    // paren balancer — we should capture all the way to the matching close.
    let src = "@props(['note' => 'something (parens) inside', 'count' => 0])\n";
    let got = extract_props_directive_from_source(src).expect("should find @props");
    assert!(got.contains("'something (parens) inside'"));
    assert!(got.ends_with("])"));
}

#[test]
fn does_not_match_propsextended_substring() {
    // `@propsExtended(...)` starts with `@props` but is a different directive.
    // The word-boundary check should reject it.
    let src = "@propsExtended(['x' => 1])\n@props(['y' => 2])\n";
    let got = extract_props_directive_from_source(src).expect("should find @props");
    assert!(got.contains("'y' => 2"));
    assert!(
        !got.contains("'x' => 1"),
        "must not match @propsExtended: {}",
        got
    );
}

#[test]
fn captures_first_directive_when_multiple_present() {
    // A file can technically have multiple `@props` (rare, but possible
    // across components). We only return the first.
    let src = "@props(['a' => 1])\n@props(['b' => 2])\n";
    let got = extract_props_directive_from_source(src).expect("should find @props");
    assert!(got.contains("'a' => 1"));
}

#[test]
fn reads_from_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("view.blade.php");
    fs::write(&path, "@props(['user' => null])\n").unwrap();
    let got = extract_props_directive(&path).expect("should find @props");
    assert_eq!(got, "@props(['user' => null])");
}

#[test]
fn returns_none_for_nonexistent_file() {
    let nonexistent = std::path::PathBuf::from("/nonexistent/view.blade.php");
    assert_eq!(extract_props_directive(&nonexistent), None);
}
