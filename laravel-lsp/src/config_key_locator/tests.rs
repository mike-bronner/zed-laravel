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

// ── enumerate_keys_in_source ──────────────────────────────────────────────

fn enum_keys(src: &str) -> Vec<String> {
    enumerate_keys_in_source(src)
        .into_iter()
        .map(|(k, _)| k)
        .collect()
}

#[test]
fn enumerate_top_level_keys_with_positions() {
    let src = r#"<?php
return [
    'name' => env('APP_NAME', 'Laravel'),
    'env' => env('APP_ENV', 'production'),
];
"#;
    let entries = enumerate_keys_in_source(src);
    assert_eq!(
        entries.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
        vec!["name", "env"]
    );
    // Position is the key string content (matches locate_in_source).
    assert_eq!(entries[0].1, locate_in_source(src, &["name"]).unwrap());
}

#[test]
fn enumerate_emits_nested_paths_leaf_and_intermediate() {
    let src = r#"<?php
return [
    'default' => 'mysql',
    'connections' => [
        'mysql' => [
            'host' => '127.0.0.1',
            'port' => 3306,
        ],
    ],
];
"#;
    let keys = enum_keys(src);
    assert_eq!(
        keys,
        vec![
            "default",
            "connections",
            "connections.mysql",
            "connections.mysql.host",
            "connections.mysql.port",
        ]
    );
}

#[test]
fn enumerate_skips_numeric_list_entries() {
    let src = r#"<?php
return [
    'providers' => [
        App\Providers\AppServiceProvider::class,
        App\Providers\AuthServiceProvider::class,
    ],
    'aliases' => [
        'Route' => Illuminate\Support\Facades\Route::class,
    ],
];
"#;
    let keys = enum_keys(src);
    // `providers` is keyed; its list items are numeric → skipped. `aliases`
    // and its string-keyed child are emitted.
    assert_eq!(keys, vec!["providers", "aliases", "aliases.Route"]);
}

#[test]
fn enumerate_empty_for_non_array_php() {
    assert!(enum_keys("<?php echo 'hi';").is_empty());
}
