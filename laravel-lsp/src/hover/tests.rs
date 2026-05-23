use super::*;

// ============================================================================
// render — section presence, ordering, omission
// ============================================================================

#[test]
fn render_empty_content_returns_empty_string() {
    let out = render(&HoverContent::default());
    assert_eq!(out, "");
}

#[test]
fn render_header_only() {
    let out = render(&HoverContent {
        header: Some("App\\Models\\User"),
        ..Default::default()
    });
    assert_eq!(out, "**App\\Models\\User**");
}

#[test]
fn render_source_link_only() {
    let out = render(&HoverContent {
        source_link: Some("[app/Models/User.php](file:///abs/User.php)"),
        ..Default::default()
    });
    // No `at ` prefix — the link renders verbatim. No backticks around the
    // label either — those would give it inline-code styling.
    assert_eq!(out, "[app/Models/User.php](file:///abs/User.php)");
}

#[test]
fn render_php_code_block_prepends_php_opening_tag() {
    // The opening tag is required for Zed's tree-sitter-php grammar to
    // parse the snippet (the standard `php` grammar variant requires it).
    let out = render(&HoverContent {
        code: Some(CodeBlock {
            language: CodeLanguage::Php,
            content: "public string $email;",
        }),
        ..Default::default()
    });
    assert_eq!(out, "```php\n<?php\npublic string $email;\n```");
}

#[test]
fn render_plain_code_block_omits_language() {
    let out = render(&HoverContent {
        code: Some(CodeBlock {
            language: CodeLanguage::Plain,
            content: "Laravel",
        }),
        ..Default::default()
    });
    assert_eq!(out, "```\nLaravel\n```");
}

#[test]
fn render_full_section_set_in_order() {
    let tags = vec![
        "@param mixed $x".to_string(),
        "@return Response".to_string(),
    ];
    let out = render(&HoverContent {
        header: Some("App\\Foo::bar"),
        detail: Some("Some detail line"),
        description: Some("Description prose."),
        code: Some(CodeBlock {
            language: CodeLanguage::Php,
            content: "public function bar()",
        }),
        tags: &tags,
        source_link: Some("[app/Foo.php:10](file:///abs/Foo.php#L10)"),
        trailer: None,
    });
    let expected = "**App\\Foo::bar**\n\
                    \n\
                    Some detail line\n\
                    \n\
                    Description prose.\n\
                    \n\
                    ```php\n\
                    <?php\n\
                    public function bar()\n\
                    ```\n\
                    \n\
                    *@param mixed $x*\n\
                    \n\
                    *@return Response*\n\
                    \n\
                    [app/Foo.php:10](file:///abs/Foo.php#L10)";
    assert_eq!(out, expected);
}

#[test]
fn render_skips_absent_sections() {
    let out = render(&HoverContent {
        header: Some("App\\Foo"),
        // no detail, no description, no code, no tags
        source_link: Some("[app/Foo.php](file:///abs/Foo.php)"),
        ..Default::default()
    });
    let expected = "**App\\Foo**\n\n[app/Foo.php](file:///abs/Foo.php)";
    assert_eq!(out, expected);
}

#[test]
fn render_trailer_appears_last() {
    let out = render(&HoverContent {
        trailer: Some("*(not registered)*"),
        ..Default::default()
    });
    assert_eq!(out, "*(not registered)*");
}

#[test]
fn render_empty_tags_slice_omits_section() {
    let tags: Vec<String> = Vec::new();
    let out = render(&HoverContent {
        header: Some("App\\Foo"),
        tags: &tags,
        ..Default::default()
    });
    assert_eq!(out, "**App\\Foo**");
}

#[test]
fn render_multiple_tags_separated_by_blank_line() {
    let tags = vec![
        "@param mixed $a".to_string(),
        "@param mixed $b".to_string(),
        "@return Response".to_string(),
    ];
    let out = render(&HoverContent {
        tags: &tags,
        ..Default::default()
    });
    let expected = "*@param mixed $a*\n\n*@param mixed $b*\n\n*@return Response*";
    assert_eq!(out, expected);
}

// ============================================================================
// Utility predicates
// ============================================================================

#[test]
fn is_class_like_type_distinguishes_classes_from_primitives() {
    assert!(is_class_like_type("App\\Models\\User"));
    assert!(is_class_like_type("\\App\\Models\\User"));
    assert!(is_class_like_type("Carbon"));
    assert!(is_class_like_type("Collection"));
    assert!(is_class_like_type("?Carbon"));

    assert!(!is_class_like_type("mixed"));
    assert!(!is_class_like_type("string"));
    assert!(!is_class_like_type("int"));
    assert!(!is_class_like_type("?int"));
    assert!(!is_class_like_type("null"));
    assert!(!is_class_like_type("array"));
}

#[test]
fn source_link_with_line_includes_fragment() {
    let link = source_link("app/Foo.php", "file:///abs/Foo.php", Some(42));
    // No backticks around the label — those would render as inline code.
    assert_eq!(link, "[app/Foo.php:42](file:///abs/Foo.php#L42)");
}

#[test]
fn source_link_without_line_omits_fragment() {
    let link = source_link("app/Foo.php", "file:///abs/Foo.php", None);
    assert_eq!(link, "[app/Foo.php](file:///abs/Foo.php)");
}

#[test]
fn truncate_for_display_clips_long_strings() {
    let long = "x".repeat(500);
    let out = truncate_for_display(&long, 200);
    assert!(out.ends_with('…'));
    assert_eq!(out.chars().filter(|c| *c == 'x').count(), 200);
}

#[test]
fn truncate_for_display_passes_short_strings_through() {
    let short = "short";
    let out = truncate_for_display(short, 200);
    assert_eq!(out, "short");
}

#[test]
fn truncate_for_display_handles_multibyte_chars_at_boundary() {
    // 200 multibyte chars — make sure we count chars not bytes
    let s: String = "日".repeat(300);
    let out = truncate_for_display(&s, 200);
    assert!(out.ends_with('…'));
    assert_eq!(out.chars().count(), 201); // 200 + ellipsis
}
