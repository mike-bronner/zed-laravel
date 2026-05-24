use super::*;

#[test]
fn locates_top_level_key() {
    let src = r#"<?php
return [
    'name' => env('APP_NAME', 'Laravel'),
    'env' => env('APP_ENV', 'production'),
];
"#;
    let pos = locate_in_source(src, &["name"]).expect("expected a position");
    assert_eq!(pos.line, 2);
    // The string content `name` sits between the quotes; the column should
    // point at the `n`.
    assert_eq!(
        &src.lines().nth(2).unwrap()[pos.start_column as usize..pos.end_column as usize],
        "name"
    );
}

#[test]
fn locates_nested_key() {
    let src = r#"<?php
return [
    'database' => [
        'connections' => [
            'mysql' => [
                'host' => '127.0.0.1',
            ],
        ],
    ],
];
"#;
    let pos = locate_in_source(src, &["database", "connections", "mysql", "host"]).expect("pos");
    assert_eq!(pos.line, 5);
    assert_eq!(
        &src.lines().nth(5).unwrap()[pos.start_column as usize..pos.end_column as usize],
        "host"
    );
}

#[test]
fn returns_none_for_missing_top_level_key() {
    let src = r#"<?php
return [
    'name' => 'Laravel',
];
"#;
    assert!(locate_in_source(src, &["missing"]).is_none());
}

#[test]
fn returns_none_for_missing_nested_key() {
    let src = r#"<?php
return [
    'database' => [
        'connections' => [
            'mysql' => ['host' => '127.0.0.1'],
        ],
    ],
];
"#;
    assert!(locate_in_source(src, &["database", "connections", "pgsql", "host"]).is_none());
}

#[test]
fn returns_none_when_path_passes_through_non_array() {
    let src = r#"<?php
return [
    'name' => 'Laravel',
];
"#;
    // 'name' resolves to a string, not an array, so descending further fails.
    assert!(locate_in_source(src, &["name", "deeper"]).is_none());
}

#[test]
fn handles_double_quoted_key() {
    let src = r#"<?php
return [
    "name" => "Laravel",
];
"#;
    let pos = locate_in_source(src, &["name"]).expect("pos");
    assert_eq!(
        &src.lines().nth(2).unwrap()[pos.start_column as usize..pos.end_column as usize],
        "name"
    );
}

#[test]
fn empty_path_returns_none() {
    let src = r#"<?php
return ['x' => 1];
"#;
    assert!(locate_in_source(src, &[]).is_none());
}

#[test]
fn handles_file_with_use_statements_above_return() {
    let src = r#"<?php

use Illuminate\Support\Str;

return [
    'name' => 'Laravel',
    'env' => 'production',
];
"#;
    let pos = locate_in_source(src, &["env"]).expect("pos");
    // Slice the line the locator actually pointed at — robust to whatever
    // line numbering the raw string convention produces.
    let line_text = src.lines().nth(pos.line as usize).unwrap();
    assert_eq!(
        &line_text[pos.start_column as usize..pos.end_column as usize],
        "env"
    );
}
