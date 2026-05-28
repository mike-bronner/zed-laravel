use super::*;
use crate::laravel_introspector::{BuilderMethodIndex, ParsedMethod};
use tower_lsp::lsp_types::{Documentation, MarkupContent, MarkupKind};

// ---- detect_method_name_position: static `::` --------------------------

#[test]
fn detects_static_position_at_double_colon() {
    let line = "        User::";
    let ctx = detect_method_name_position(line, line.len()).expect("should detect");
    assert_eq!(
        ctx,
        MethodNameContext::Static {
            receiver: "User".to_string()
        }
    );
}

#[test]
fn detects_static_position_with_partial_method() {
    // The user has typed `User::wher` and the LSP fires re-completion at
    // each keystroke. We strip the trailing identifier chars to get back
    // to the `::` and report the receiver.
    let line = "        User::wher";
    let ctx = detect_method_name_position(line, line.len()).expect("should detect");
    assert_eq!(
        ctx,
        MethodNameContext::Static {
            receiver: "User".to_string()
        }
    );
}

#[test]
fn detects_static_position_with_namespaced_receiver() {
    let line = "        App\\Models\\User::";
    let ctx = detect_method_name_position(line, line.len()).expect("should detect");
    assert_eq!(
        ctx,
        MethodNameContext::Static {
            receiver: "App\\Models\\User".to_string()
        }
    );
}

#[test]
fn detects_static_position_with_leading_backslash() {
    let line = "        \\App\\Models\\User::";
    let ctx = detect_method_name_position(line, line.len()).expect("should detect");
    assert_eq!(
        ctx,
        MethodNameContext::Static {
            receiver: "\\App\\Models\\User".to_string()
        }
    );
}

#[test]
fn detects_static_position_in_assignment_context() {
    let line = "        $users = User::";
    let ctx = detect_method_name_position(line, line.len()).expect("should detect");
    assert_eq!(
        ctx,
        MethodNameContext::Static {
            receiver: "User".to_string()
        }
    );
}

#[test]
fn detects_static_position_inside_function_call() {
    let line = "        return User::";
    let ctx = detect_method_name_position(line, line.len()).expect("should detect");
    assert_eq!(
        ctx,
        MethodNameContext::Static {
            receiver: "User".to_string()
        }
    );
}

// ---- detect_method_name_position: instance `->` ------------------------

#[test]
fn detects_instance_position_at_arrow() {
    let line = "        $user->";
    let ctx = detect_method_name_position(line, line.len()).expect("should detect");
    assert_eq!(ctx, MethodNameContext::Instance);
}

#[test]
fn detects_instance_position_with_partial_method() {
    let line = "        $user->wher";
    let ctx = detect_method_name_position(line, line.len()).expect("should detect");
    assert_eq!(ctx, MethodNameContext::Instance);
}

#[test]
fn detects_instance_position_after_chained_call() {
    let line = "        User::query()->";
    let ctx = detect_method_name_position(line, line.len()).expect("should detect");
    assert_eq!(ctx, MethodNameContext::Instance);
}

// ---- detect_method_name_position: non-matches --------------------------

#[test]
fn rejects_position_with_no_operator_before_cursor() {
    let line = "        $users = User";
    assert_eq!(detect_method_name_position(line, line.len()), None);
}

#[test]
fn rejects_position_inside_method_call_args() {
    // Cursor between parens — argument-completion territory, owned by the
    // chain extractor. Method-name detection stays out.
    let line = "        User::where(";
    assert_eq!(detect_method_name_position(line, line.len()), None);
}

#[test]
fn rejects_empty_line() {
    let line = "";
    assert_eq!(detect_method_name_position(line, 0), None);
}

#[test]
fn rejects_cursor_past_end_of_line() {
    let line = "User::";
    assert_eq!(detect_method_name_position(line, 999), None);
}

#[test]
fn rejects_bare_double_colon_with_no_receiver() {
    let line = "        ::";
    assert_eq!(detect_method_name_position(line, line.len()), None);
}

#[test]
fn rejects_single_colon() {
    let line = "        User:";
    assert_eq!(detect_method_name_position(line, line.len()), None);
}

// ---- build_items_from_index --------------------------------------------

/// Construct a small synthetic index for shape-of-output tests.
fn synth_index() -> BuilderMethodIndex {
    BuilderMethodIndex {
        eloquent_builder: vec![
            ParsedMethod {
                name: "where".to_string(),
                source_class: "Illuminate\\Database\\Eloquent\\Builder".to_string(),
                signature: "public function where($column, $operator = null)".to_string(),
                return_type: Some("Builder<static>".to_string()),
                summary: Some("Add a basic where clause.".to_string()),
                doc_body: Some(
                    "Add a basic where clause.\n\n@param  string  $column\n@return $this"
                        .to_string(),
                ),
            },
            ParsedMethod {
                name: "find".to_string(),
                source_class: "Illuminate\\Database\\Eloquent\\Builder".to_string(),
                signature: "public function find($id, $columns = ['*'])".to_string(),
                return_type: None,
                summary: None,
                doc_body: None,
            },
        ],
        query_builder: vec![ParsedMethod {
            name: "select".to_string(),
            source_class: "Illuminate\\Database\\Query\\Builder".to_string(),
            signature: "public function select($columns = ['*'])".to_string(),
            return_type: Some("Builder<static>".to_string()),
            summary: Some("Set the columns to be selected.".to_string()),
            doc_body: Some("Set the columns to be selected.".to_string()),
        }],
        // Tests start with an empty collision set; specific tests that
        // exercise the suppression path populate this explicitly.
        model_static_method_names: std::collections::HashSet::new(),
    }
}

#[test]
fn items_carry_method_kind() {
    let items = build_items_from_index(&synth_index());
    for item in &items {
        assert_eq!(
            item.kind,
            Some(CompletionItemKind::METHOD),
            "item `{}` should be METHOD kind",
            item.label
        );
    }
}

#[test]
fn items_carry_return_type_in_top_level_detail() {
    // Zed merges `label` + " " + `detail` into the row's main bold label
    // (verified by source-dive of `crates/language/src/language.rs:1346`).
    // So the return type goes in `detail`: the row shows `where $this`.
    let items = build_items_from_index(&synth_index());
    let where_item = items
        .iter()
        .find(|i| i.label == "where")
        .expect("where item");
    assert_eq!(where_item.detail.as_deref(), Some("Builder<static>"));

    // Methods with no return type info omit `detail` — the row just
    // shows the bare method name.
    let find_item = items.iter().find(|i| i.label == "find").expect("find item");
    assert_eq!(find_item.detail, None);
}

#[test]
fn items_carry_rich_markdown_documentation() {
    // Initial response ships the full Intelephense-style markdown panel
    // — Zed's design makes end-slot summary and rich aside mutually
    // exclusive, so we pick the rich aside (matches Intelephense).
    let items = build_items_from_index(&synth_index());
    let where_item = items
        .iter()
        .find(|i| i.label == "where")
        .expect("where item");
    match where_item.documentation.as_ref() {
        Some(Documentation::MarkupContent(MarkupContent { kind, value })) => {
            assert_eq!(*kind, MarkupKind::Markdown);
            assert!(value.contains("**Illuminate\\Database\\Eloquent\\Builder::where**"));
            assert!(value.contains("```php"));
        }
        other => panic!("expected MarkupContent documentation, got {other:?}"),
    }
}

#[test]
fn builder_methods_with_real_model_static_namesakes_are_suppressed() {
    // Mirror the real-world `with` collision: Model has its own static
    // `with($relations)`, Builder has `with($relations, $callback)`.
    // PHP routes `Portfolio::with(...)` directly to Model's version
    // (no __callStatic), so Builder's signature would mislead — suppress.
    use std::collections::HashSet;
    let mut index = synth_index();
    let mut collisions = HashSet::new();
    collisions.insert("where".to_string());
    index.model_static_method_names = collisions;

    let items = build_items_from_index(&index);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        !labels.contains(&"where"),
        "Builder method `where` should be suppressed when Model has a real static of the same name, got: {labels:?}"
    );
    // Other methods unaffected.
    assert!(labels.contains(&"find"));
    assert!(labels.contains(&"select"));
}

#[test]
fn items_do_not_stash_data_payload() {
    // We dropped the resolve dance — there's no `data` payload to send
    // through completionItem/resolve. Items should be plain.
    let items = build_items_from_index(&synth_index());
    for item in &items {
        assert!(
            item.data.is_none(),
            "item `{}` should not carry a `data` payload",
            item.label
        );
    }
}

#[test]
fn items_do_not_set_sort_text() {
    // We deliberately don't push our items up or down. At the static
    // position the PHP LSP doesn't emit Builder methods, so there's
    // nothing to compete with — sortText would only fight against the
    // alphabetical default.
    let items = build_items_from_index(&synth_index());
    for item in &items {
        assert_eq!(
            item.sort_text, None,
            "item `{}` should not override sort_text (alphabetical default is right)",
            item.label
        );
    }
}

#[test]
fn documentation_panel_carries_intelephense_structure() {
    let items = build_items_from_index(&synth_index());
    let where_item = items
        .iter()
        .find(|i| i.label == "where")
        .expect("where item");
    let value = match where_item.documentation.as_ref() {
        Some(Documentation::MarkupContent(MarkupContent { kind, value })) => {
            assert_eq!(*kind, MarkupKind::Markdown, "panel should be markdown");
            value.clone()
        }
        other => panic!("expected MarkupContent, got {other:?}"),
    };

    // Header: bolded FQCN::method (Intelephense-style)
    assert!(
        value.starts_with("**Illuminate\\Database\\Eloquent\\Builder::where**"),
        "panel should lead with a bolded qualified identifier:\n{value}"
    );
    // Summary prose
    assert!(
        value.contains("Add a basic where clause."),
        "panel should include the summary:\n{value}"
    );
    // Fenced PHP signature block, seeded with `<?php` so Zed highlights it
    assert!(
        value.contains(
            "```php\n<?php\npublic function where($column, $operator = null)\n```"
        ),
        "panel should include the fenced signature with PHP open tag:\n{value}"
    );
    // @param tag with its type wrapped in backticks
    assert!(
        value.contains("@param `string` $column"),
        "panel should include the @param tag with type in backticks:\n{value}"
    );
    // @return tag should have `$this` resolved to `Builder<static>`,
    // matching the row's detail field.
    assert!(
        value.contains("@return `Builder<static>`"),
        "panel should resolve `$this` to `Builder<static>` in @return:\n{value}"
    );
}

#[test]
fn documentation_panel_for_method_without_docblock_still_has_signature() {
    // `find` has no doc_body, but we still build a panel from header
    // + signature so the user sees the call shape on hover.
    let items = build_items_from_index(&synth_index());
    let find_item = items.iter().find(|i| i.label == "find").expect("find item");
    let value = match find_item.documentation.as_ref() {
        Some(Documentation::MarkupContent(MarkupContent { value, .. })) => value.clone(),
        other => panic!("expected MarkupContent, got {other:?}"),
    };
    assert!(value.contains("**Illuminate\\Database\\Eloquent\\Builder::find**"));
    assert!(value.contains(
        "```php\n<?php\npublic function find($id, $columns = ['*'])\n```"
    ));
}

#[test]
fn merged_surface_includes_query_only_methods() {
    let items = build_items_from_index(&synth_index());
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        labels.contains(&"select"),
        "select (Query-only) should appear in items: {labels:?}"
    );
}
