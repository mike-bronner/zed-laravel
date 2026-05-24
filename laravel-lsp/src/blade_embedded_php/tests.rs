use super::*;

#[test]
fn extracts_single_echo_region() {
    let src = r#"<a href="{{ route('home') }}">Home</a>
"#;
    let regions = extract_php_regions(src);
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].content.trim(), "route('home')");
    assert_eq!(regions[0].row, 0);
}

#[test]
fn extracts_multiple_echo_regions_in_order() {
    let src = r#"<nav>
<a href="{{ route('home') }}">Home</a>
<a href="{{ route('users.index') }}">Users</a>
</nav>
"#;
    let regions = extract_php_regions(src);
    assert_eq!(regions.len(), 2);
    assert_eq!(regions[0].row, 1);
    assert_eq!(regions[1].row, 2);
}

#[test]
fn extracts_raw_echo_alongside_regular_echo() {
    // `{!! !!}` should be captured the same way `{{ }}` is — both produce
    // php_statement > php_only nodes in the Blade grammar.
    let src = r#"<div>
{{ route('home') }}
{!! route('admin.home') !!}
</div>
"#;
    let regions = extract_php_regions(src);
    let names: Vec<&str> = regions.iter().map(|r| r.content.as_str().trim()).collect();
    assert!(
        names.contains(&"route('home')"),
        "echo missing from {:?}",
        names
    );
    assert!(
        names.contains(&"route('admin.home')"),
        "raw echo missing from {:?}",
        names
    );
}

#[test]
fn extracts_php_block_region() {
    let src = r#"@php
    $url = route('home');
@endphp
"#;
    let regions = extract_php_regions(src);
    assert!(!regions.is_empty(), "expected at least one @php region");
    let block = regions
        .iter()
        .find(|r| r.content.contains("route('home')"))
        .expect("@php block content not extracted");
    assert!(block.content.contains("$url"));
}

#[test]
fn ignores_blade_files_without_any_php_regions() {
    let src = r#"<div>
    <h1>Plain HTML</h1>
    <p>No Blade expressions here.</p>
</div>
"#;
    let regions = extract_php_regions(src);
    assert!(regions.is_empty(), "expected no regions, got {:?}", regions);
}

#[test]
fn adjust_inner_position_on_row_zero_subtracts_prefix() {
    // Snippet: `<?php route('home')` — `route` token is at snippet col 6.
    // For an echo at blade (row=3, col=12), `route` should land at
    // (row=3, col=12) — the wrapper prefix is stripped.
    let (line, col) = adjust_inner_position(0, 6, 3, 12);
    assert_eq!(line, 3);
    assert_eq!(col, 12);
}

#[test]
fn adjust_inner_position_on_later_rows_preserves_column() {
    // Snippet: a multi-line region. tree-sitter reports a token at (1, 5).
    // Blade absolute row = 4 (region.row=3 + inner.row=1); column stays 5.
    let (line, col) = adjust_inner_position(1, 5, 3, 12);
    assert_eq!(line, 4);
    assert_eq!(col, 5);
}

#[test]
fn adjust_inner_position_handles_token_at_prefix_boundary() {
    // Snippet column 0 (impossible in practice, but defend against
    // underflow): saturating sub keeps us at 0.
    let (_, col) = adjust_inner_position(0, 0, 3, 12);
    assert_eq!(col, 12);
}
