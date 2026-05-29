use super::*;

// ---- CompletionDoc::render ---------------------------------------------

#[test]
fn renders_full_panel_in_intelephense_order() {
    let doc = CompletionDoc::new()
        .header("Illuminate\\Database\\Eloquent\\Builder::with")
        .summary("Begin querying a model with eager loading.")
        .code(CodeBlock::new(
            "php",
            "public function with($relations, $callback = null)",
        ))
        .section("@param array|string $relations")
        .section("@return $this");

    let rendered = doc.render();
    let expected = "**Illuminate\\Database\\Eloquent\\Builder::with**\n\n\
                    Begin querying a model with eager loading.\n\n\
                    ```php\n\
                    <?php\n\
                    public function with($relations, $callback = null)\n\
                    ```\n\n\
                    @param `array|string` $relations\n\n\
                    @return `$this`";
    assert_eq!(rendered, expected);
}

#[test]
fn skips_absent_parts_without_blank_lines() {
    // Only header + summary — no code, no sections. Must not leave a
    // dangling blank line where the code block would have been.
    let doc = CompletionDoc::new()
        .header("users.email")
        .summary("varchar(255), nullable");

    assert_eq!(doc.render(), "**users.email**\n\nvarchar(255), nullable");
}

#[test]
fn header_only_renders_just_header() {
    let doc = CompletionDoc::new().header("app.name");
    assert_eq!(doc.render(), "**app.name**");
}

#[test]
fn php_code_block_is_seeded_with_open_tag() {
    // Zed only triggers PHP syntax highlighting in fenced markdown when
    // the block contains the `<?php` open tag.
    let doc = CompletionDoc::new().code(CodeBlock::new("php", "public function get()"));
    assert_eq!(doc.render(), "```php\n<?php\npublic function get()\n```");
}

#[test]
fn php_code_block_does_not_double_seed_open_tag() {
    // If the caller already supplied `<?php`, leave it alone — don't
    // produce `<?php\n\n<?php\n…`.
    let doc = CompletionDoc::new().code(CodeBlock::new("php", "<?php\npublic function get()"));
    assert_eq!(doc.render(), "```php\n<?php\npublic function get()\n```");
}

#[test]
fn non_php_code_block_carries_language_tag_only() {
    // Other languages don't need an open-tag seed.
    let doc = CompletionDoc::new().code(CodeBlock::new("sql", "SELECT * FROM users"));
    assert_eq!(doc.render(), "```sql\nSELECT * FROM users\n```");
}

#[test]
fn empty_strings_are_ignored() {
    let doc = CompletionDoc::new().header("").summary("").section("");
    assert!(doc.is_empty());
    assert_eq!(doc.render(), "");
}

#[test]
fn summary_opt_handles_none() {
    let doc = CompletionDoc::new().header("X").summary_opt(None);
    assert_eq!(doc.render(), "**X**");
}

#[test]
fn summary_opt_handles_some() {
    let doc = CompletionDoc::new()
        .header("X")
        .summary_opt(Some("hello".to_string()));
    assert_eq!(doc.render(), "**X**\n\nhello");
}

#[test]
fn code_opt_handles_none() {
    let doc = CompletionDoc::new().header("X").code_opt(None);
    assert_eq!(doc.render(), "**X**");
}

#[test]
fn sections_plural_appends_all_nonempty() {
    let doc = CompletionDoc::new().header("X").sections(["a", "", "b"]);
    assert_eq!(doc.render(), "**X**\n\na\n\nb");
}

#[test]
fn into_documentation_produces_markdown_markup() {
    let doc = CompletionDoc::new().header("X").summary("y");
    match doc.into_documentation() {
        Documentation::MarkupContent(MarkupContent { kind, value }) => {
            assert_eq!(kind, MarkupKind::Markdown);
            assert_eq!(value, "**X**\n\ny");
        }
        other => panic!("expected MarkupContent, got {other:?}"),
    }
}

#[test]
fn is_empty_true_for_fresh_doc() {
    assert!(CompletionDoc::new().is_empty());
}

#[test]
fn is_empty_false_once_any_part_set() {
    assert!(!CompletionDoc::new().header("X").is_empty());
    assert!(!CompletionDoc::new().summary("y").is_empty());
    assert!(!CompletionDoc::new()
        .code(CodeBlock::new("php", "z"))
        .is_empty());
    assert!(!CompletionDoc::new().section("s").is_empty());
}

// ---- split_phpdoc ------------------------------------------------------

#[test]
fn split_phpdoc_separates_summary_and_tags() {
    let doc = "Add a basic where clause to the query.\n\n\
               @param string $column\n\
               @param mixed $operator\n\
               @return $this";
    let (summary, tags) = split_phpdoc(doc);
    assert_eq!(
        summary.as_deref(),
        Some("Add a basic where clause to the query.")
    );
    assert_eq!(
        tags,
        vec![
            "@param string $column".to_string(),
            "@param mixed $operator".to_string(),
            "@return $this".to_string(),
        ]
    );
}

#[test]
fn split_phpdoc_joins_multiline_summary() {
    let doc = "Execute the query and get the first result\n\
               or throw an exception.\n\n\
               @return TModel";
    let (summary, tags) = split_phpdoc(doc);
    assert_eq!(
        summary.as_deref(),
        Some("Execute the query and get the first result or throw an exception.")
    );
    assert_eq!(tags, vec!["@return TModel".to_string()]);
}

#[test]
fn split_phpdoc_attaches_wrapped_tag_continuation() {
    // A @param whose description wraps onto the next (non-@) line should
    // stay attached to that tag, not get dropped.
    let doc = "Eager load relations.\n\n\
               @param array<array-key, array|Closure>\n\
               |string $relations\n\
               @return $this";
    let (_summary, tags) = split_phpdoc(doc);
    assert_eq!(
        tags,
        vec![
            "@param array<array-key, array|Closure> |string $relations".to_string(),
            "@return $this".to_string(),
        ]
    );
}

#[test]
fn split_phpdoc_summary_only_no_tags() {
    let (summary, tags) = split_phpdoc("Just a description.");
    assert_eq!(summary.as_deref(), Some("Just a description."));
    assert!(tags.is_empty());
}

#[test]
fn split_phpdoc_tags_only_no_summary() {
    let (summary, tags) = split_phpdoc("@return void");
    assert_eq!(summary, None);
    assert_eq!(tags, vec!["@return void".to_string()]);
}

#[test]
fn split_phpdoc_empty_input() {
    let (summary, tags) = split_phpdoc("");
    assert_eq!(summary, None);
    assert!(tags.is_empty());
}

// ---- format_phpdoc_tag -------------------------------------------------

#[test]
fn format_phpdoc_tag_param_wraps_type_before_var() {
    assert_eq!(
        format_phpdoc_tag("@param array|string $relations"),
        "@param `array|string` $relations"
    );
}

#[test]
fn format_phpdoc_tag_param_preserves_description_after_var() {
    assert_eq!(
        format_phpdoc_tag("@param int $id The primary key"),
        "@param `int` $id The primary key"
    );
}

#[test]
fn format_phpdoc_tag_return_wraps_only_type() {
    assert_eq!(format_phpdoc_tag("@return $this"), "@return `$this`");
    assert_eq!(
        format_phpdoc_tag("@return \\Illuminate\\Database\\Eloquent\\Builder"),
        "@return `\\Illuminate\\Database\\Eloquent\\Builder`"
    );
}

#[test]
fn format_phpdoc_tag_return_preserves_description() {
    assert_eq!(
        format_phpdoc_tag("@return Collection<int, User> the matching users"),
        "@return `Collection<int,` User> the matching users"
    );
    // ^ Imperfect: types with literal spaces (rare) get split. Laravel's
    // PHPDoc rarely uses spaces inside generics; if it becomes a problem
    // we can teach the helper to balance angle brackets.
}

#[test]
fn format_phpdoc_tag_throws_wraps_type() {
    assert_eq!(
        format_phpdoc_tag("@throws \\RuntimeException"),
        "@throws `\\RuntimeException`"
    );
}

#[test]
fn format_phpdoc_tag_var_wraps_type_before_var() {
    // `@var` looks for the `$variable` to find the type boundary, so
    // types with literal spaces (e.g. `array<string, int>`) wrap
    // correctly — better than the @return / @throws case which has no
    // such anchor.
    assert_eq!(
        format_phpdoc_tag("@var array<string, int> $counts"),
        "@var `array<string, int>` $counts"
    );
}

#[test]
fn format_phpdoc_tag_unknown_tag_passes_through() {
    // Tags without a type slot (@deprecated, @internal, @see, etc.)
    // shouldn't have anything wrapped.
    assert_eq!(format_phpdoc_tag("@deprecated"), "@deprecated");
    assert_eq!(format_phpdoc_tag("@see Foo::bar()"), "@see Foo::bar()");
    assert_eq!(format_phpdoc_tag("@since 9.0.0"), "@since 9.0.0");
}

#[test]
fn format_phpdoc_tag_already_backticked_type_not_double_wrapped() {
    assert_eq!(format_phpdoc_tag("@return `Builder`"), "@return `Builder`");
}

#[test]
fn format_phpdoc_tag_handles_just_keyword() {
    // `@param` with nothing after it shouldn't crash or invent a type.
    assert_eq!(format_phpdoc_tag("@param"), "@param");
}

// ---- render() integration with auto-tag-formatting --------------------

#[test]
fn render_auto_formats_at_tag_sections() {
    let doc = CompletionDoc::new()
        .header("Foo::bar")
        .section("@param int $id")
        .section("@return $this");
    let rendered = doc.render();
    assert!(rendered.contains("@param `int` $id"));
    assert!(rendered.contains("@return `$this`"));
}

#[test]
fn render_resolves_self_types_in_tags_when_class_provided() {
    // `$this` in a @return tag should be rewritten to `<basename><static>`
    // when the doc carries a resolution class — matches how the row's
    // detail field is displayed.
    let doc = CompletionDoc::new()
        .header("Builder::where")
        .resolve_self_for("Illuminate\\Database\\Eloquent\\Builder")
        .section("@return $this")
        .section("@param string $column");
    let rendered = doc.render();
    assert!(
        rendered.contains("@return `Builder<static>`"),
        "expected `$this` to resolve in @return, got:\n{rendered}"
    );
    // Tags without self-references are untouched.
    assert!(rendered.contains("@param `string` $column"));
}

#[test]
fn render_leaves_self_keyword_alone_without_resolution_class() {
    // No `.resolve_self_for(...)` call → tags preserve `$this` verbatim.
    let doc = CompletionDoc::new().header("x").section("@return $this");
    let rendered = doc.render();
    assert!(rendered.contains("@return `$this`"));
}

#[test]
fn render_resolves_self_inside_union_type_in_tag() {
    let doc = CompletionDoc::new()
        .header("Foo::maybe")
        .resolve_self_for("Illuminate\\Database\\Eloquent\\Builder")
        .section("@return $this|null");
    let rendered = doc.render();
    assert!(
        rendered.contains("@return `Builder<static>|null`"),
        "expected union resolved, got:\n{rendered}"
    );
}

#[test]
fn render_leaves_non_tag_sections_untouched() {
    let doc = CompletionDoc::new()
        .header("users.email")
        .section("Defined in: app/Models/User.php");
    let rendered = doc.render();
    assert!(rendered.contains("Defined in: app/Models/User.php"));
    // No backticks were spuriously added.
    assert!(!rendered.contains('`'));
}
