use super::*;

#[test]
fn resolves_top_level_string_value() {
    let src = r#"<?php
return [
    'name' => 'Laravel',
    'env' => 'production',
];
"#;
    assert_eq!(
        resolve_in_source(src, &["name"]).as_deref(),
        Some("'Laravel'")
    );
    assert_eq!(
        resolve_in_source(src, &["env"]).as_deref(),
        Some("'production'")
    );
}

#[test]
fn resolves_value_with_env_helper() {
    let src = r#"<?php
return [
    'name' => env('APP_NAME', 'Laravel'),
];
"#;
    assert_eq!(
        resolve_in_source(src, &["name"]).as_deref(),
        Some("env('APP_NAME', 'Laravel')")
    );
}

#[test]
fn resolves_double_quoted_key() {
    let src = r#"<?php
return [
    "name" => "Laravel",
];
"#;
    assert_eq!(
        resolve_in_source(src, &["name"]).as_deref(),
        Some("\"Laravel\"")
    );
}

#[test]
fn resolves_nested_two_levels() {
    let src = r#"<?php
return [
    'from' => [
        'address' => 'hello@example.com',
        'name' => 'Example',
    ],
];
"#;
    assert_eq!(
        resolve_in_source(src, &["from", "address"]).as_deref(),
        Some("'hello@example.com'")
    );
    assert_eq!(
        resolve_in_source(src, &["from", "name"]).as_deref(),
        Some("'Example'")
    );
}

#[test]
fn resolves_nested_three_levels() {
    let src = r#"<?php
return [
    'connections' => [
        'mysql' => [
            'host' => '127.0.0.1',
            'port' => 3306,
        ],
    ],
];
"#;
    assert_eq!(
        resolve_in_source(src, &["connections", "mysql", "host"]).as_deref(),
        Some("'127.0.0.1'")
    );
    assert_eq!(
        resolve_in_source(src, &["connections", "mysql", "port"]).as_deref(),
        Some("3306")
    );
}

#[test]
fn returns_none_for_missing_key() {
    let src = "<?php\nreturn ['name' => 'Laravel'];\n";
    assert_eq!(resolve_in_source(src, &["missing"]), None);
}

#[test]
fn returns_none_when_path_passes_through_non_array() {
    // 'name' resolves to a string, but we tried to drill into it as if it were an array.
    let src = "<?php\nreturn ['name' => 'Laravel'];\n";
    assert_eq!(resolve_in_source(src, &["name", "deeper"]), None);
}

#[test]
fn skips_line_and_block_comments_before_return() {
    let src = r#"<?php
// This is a config file.
/* Block comment with the word return inside.
   Should not confuse the parser. */
return [
    'key' => 'value',
];
"#;
    assert_eq!(resolve_in_source(src, &["key"]).as_deref(), Some("'value'"));
}

#[test]
fn resolves_top_level_array_value() {
    let src = r#"<?php
return [
    'providers' => [
        Provider::class,
    ],
];
"#;
    let result = resolve_in_source(src, &["providers"]);
    assert!(result.is_some(), "should return the inner array text");
    assert!(
        result.unwrap().contains("Provider::class"),
        "should include the array contents"
    );
}

#[test]
fn returns_none_for_non_existent_file_when_resolving_via_root() {
    use std::path::PathBuf;
    let nonexistent = PathBuf::from("/nonexistent/path/to/laravel/project");
    assert_eq!(resolve_value(&nonexistent, "app.name"), None);
}

#[test]
fn handles_keys_with_dots_in_values() {
    // The value 'foo.bar' contains a dot but is a single string — it should
    // round-trip unchanged, and key splitting should not be confused.
    let src = "<?php\nreturn ['x' => 'foo.bar.baz'];\n";
    assert_eq!(
        resolve_in_source(src, &["x"]).as_deref(),
        Some("'foo.bar.baz'")
    );
}

#[test]
fn handles_escaped_quotes_in_key() {
    // Backslash escapes inside the key — the parser must unescape so the key
    // comparison succeeds.
    let src = r#"<?php
return [
    'has\'quote' => 'matched',
];
"#;
    assert_eq!(
        resolve_in_source(src, &["has'quote"]).as_deref(),
        Some("'matched'")
    );
}
