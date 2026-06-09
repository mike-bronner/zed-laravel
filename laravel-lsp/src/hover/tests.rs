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

// ============================================================================
// magic_member_card (M6) — classification hover for Eloquent magic members
// ============================================================================

use crate::salsa_impl::{Confidence, MagicMemberKind};

#[test]
fn magic_member_card_relationship_high_confidence() {
    let out = magic_member_card(
        MagicMemberKind::Relationship,
        "posts",
        "App\\Models\\User",
        Confidence::High,
        None,
        None,
        Some("[app/Models/User.php:12](file:///p/app/Models/User.php#L12)"),
    );
    assert_eq!(
        out,
        "**Eloquent relationship**\n\n`posts` on `App\\Models\\User`\n\n[app/Models/User.php:12](file:///p/app/Models/User.php#L12)"
    );
}

#[test]
fn magic_member_card_with_definition_renders_php_code_block() {
    let out = magic_member_card(
        MagicMemberKind::Relationship,
        "account",
        "App\\Models\\User",
        Confidence::High,
        Some("public function account()\n{\n    return $this->belongsTo(Account::class);\n}"),
        None,
        None,
    );
    // Definition renders as a php fence (render prepends the <?php opener) and
    // sits between the detail line and any source link.
    assert!(out.contains("```php\n<?php\npublic function account()"), "got: {out}");
    assert!(out.contains("belongsTo(Account::class)"), "got: {out}");
}

#[test]
fn magic_member_card_labels_each_kind() {
    let label = |k| {
        magic_member_card(k, "x", "App\\Models\\User", Confidence::High, None, None, None)
            .lines()
            .next()
            .unwrap()
            .to_string()
    };
    assert_eq!(label(MagicMemberKind::Scope), "**Eloquent scope**");
    assert_eq!(label(MagicMemberKind::Accessor), "**Eloquent accessor**");
    assert_eq!(label(MagicMemberKind::Column), "**Database column**");
    assert_eq!(label(MagicMemberKind::DynamicFinder), "**Dynamic finder**");
}

#[test]
fn magic_member_card_plain_member_is_empty() {
    // Generic properties are Intelephense's job — no card, no duplication.
    let out = magic_member_card(
        MagicMemberKind::PlainMember,
        "name",
        "App\\Models\\User",
        Confidence::High,
        None,
        None,
        None,
    );
    assert_eq!(out, "");
}

#[test]
fn magic_member_card_medium_confidence_adds_inferred_trailer() {
    let out = magic_member_card(
        MagicMemberKind::Scope,
        "active",
        "App\\Models\\User",
        Confidence::Medium,
        None,
        None,
        None,
    );
    assert!(out.ends_with("*receiver type inferred*"), "got: {out}");
}

#[test]
fn magic_member_card_high_confidence_has_no_trailer() {
    let out = magic_member_card(
        MagicMemberKind::Scope,
        "active",
        "App\\Models\\User",
        Confidence::High,
        None,
        None,
        None,
    );
    assert!(!out.contains("inferred"), "got: {out}");
}

#[test]
fn candidate_method_names_by_kind() {
    // Relationship / finder: accessed verbatim.
    assert_eq!(
        candidate_method_names(MagicMemberKind::Relationship, "account"),
        vec!["account".to_string()]
    );
    // Scope: scope{Pascal}.
    assert_eq!(
        candidate_method_names(MagicMemberKind::Scope, "active"),
        vec!["scopeActive".to_string()]
    );
    // Accessor: old-style get{Pascal}Attribute + new-style camelCase.
    assert_eq!(
        candidate_method_names(MagicMemberKind::Accessor, "full_name"),
        vec!["getFullNameAttribute".to_string(), "fullName".to_string()]
    );
}

#[test]
fn extract_member_snippet_dedents_and_slices() {
    let src = "<?php\nclass User {\n    public function account()\n    {\n        return $this->belongsTo(Account::class);\n    }\n}\n";
    // Method spans lines 2..=5 (0-based): signature, brace, body, close.
    let snippet = extract_member_snippet(src, 2, 5);
    assert_eq!(
        snippet,
        "public function account()\n{\n    return $this->belongsTo(Account::class);\n}"
    );
}

#[test]
fn extract_member_snippet_out_of_bounds_is_empty() {
    assert_eq!(extract_member_snippet("a\nb\n", 9, 12), "");
}

#[test]
fn extract_member_snippet_caps_long_bodies() {
    let body: String = (0..40).map(|i| format!("    line{i}\n")).collect();
    let src = format!("<?php\nclass X {{\n{body}}}\n");
    let snippet = extract_member_snippet(&src, 2, 41);
    assert!(snippet.lines().count() <= 21, "should cap at MAX_LINES + marker");
    assert!(snippet.ends_with("// …"));
}

#[test]
fn magic_member_card_without_link_omits_source_section() {
    let out = magic_member_card(
        MagicMemberKind::Column,
        "email",
        "App\\Models\\User",
        Confidence::High,
        None,
        None,
        None,
    );
    assert_eq!(out, "**Database column**\n\n`email` on `App\\Models\\User`");

    // With a resolved type (M6.2), a "Type" line appears under the detail.
    let typed = magic_member_card(
        MagicMemberKind::Column,
        "email",
        "App\\Models\\User",
        Confidence::High,
        None,
        Some("string"),
        None,
    );
    assert_eq!(
        typed,
        "**Database column**\n\n`email` on `App\\Models\\User`\n\nType `string`"
    );
}
