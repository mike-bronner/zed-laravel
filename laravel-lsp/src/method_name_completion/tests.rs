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
        value.contains("```php\n<?php\npublic function where($column, $operator = null)\n```"),
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
    assert!(value.contains("```php\n<?php\npublic function find($id, $columns = ['*'])\n```"));
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

// ---- Phase 3: dynamic_where_to_items ----------------------------------

use crate::laravel_introspector::{
    AccessorInfo, ClassView, ColumnInfo, ColumnSource, LaravelClassKind, PhpStructure,
    PhpStructureKind,
};

fn col(name: &str, php_type: &str, source: ColumnSource) -> ColumnInfo {
    ColumnInfo {
        name: name.to_string(),
        php_type: php_type.to_string(),
        source,
    }
}

/// Minimal ClassView with no real-method surface — every synthetic
/// passes through. Specific tests populate `callstatic_surface` /
/// `scopes` when exercising the "PHP magic methods don't fire when a
/// real method exists" rule.
fn empty_view() -> ClassView {
    ClassView {
        file_path: std::path::PathBuf::new(),
        fqcn: "App\\Models\\Portfolio".to_string(),
        namespace: Some("App\\Models".to_string()),
        class_name: "Portfolio".to_string(),
        kind: LaravelClassKind::Model,
        direct: PhpStructure {
            kind: PhpStructureKind::Class,
            name: "Portfolio".to_string(),
            extends: None,
            extends_raw: None,
            trait_uses: Vec::new(),
            implements_raw: Vec::new(),
            start_line: 0,
            start_column: 0,
            end_line: 0,
            end_column: 0,
            methods: Vec::new(),
            properties: Vec::new(),
        },
        all_methods: Vec::new(),
        all_properties: Vec::new(),
        scopes: Vec::new(),
        accessors: Vec::<AccessorInfo>::new(),
        relationships: Vec::new(),
        casts: std::collections::HashMap::new(),
        table_name: None,
        column_surface: Vec::new(),
        callstatic_surface: Vec::new(),
    }
}

fn empty_index() -> BuilderMethodIndex {
    BuilderMethodIndex {
        eloquent_builder: Vec::new(),
        query_builder: Vec::new(),
        model_static_method_names: std::collections::HashSet::new(),
    }
}

#[test]
fn dynamic_where_emits_where_and_or_where_pair_per_column() {
    let view = empty_view();
    let index = empty_index();
    let cols = vec![col("email", "string", ColumnSource::Fillable)];
    let items = dynamic_where_to_items(&view, &index, &cols);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        labels.contains(&"whereEmail"),
        "should emit whereEmail; got {labels:?}"
    );
    assert!(
        labels.contains(&"orWhereEmail"),
        "should emit orWhereEmail; got {labels:?}"
    );
}

#[test]
fn dynamic_where_snake_to_studly_handles_multi_segment_columns() {
    let view = empty_view();
    let index = empty_index();
    let cols = vec![
        col("email_verified_at", "Carbon", ColumnSource::ParentClass),
        col("user_id", "int", ColumnSource::Convention),
    ];
    let items = dynamic_where_to_items(&view, &index, &cols);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(labels.contains(&"whereEmailVerifiedAt"));
    assert!(labels.contains(&"orWhereEmailVerifiedAt"));
    assert!(labels.contains(&"whereUserId"));
}

#[test]
fn dynamic_where_skips_when_real_builder_method_exists() {
    // PHP magic methods only fire when no real method exists. Builder
    // defines `whereDate(column, op, value)` — a column called `date`
    // would synthesize `whereDate(...)` which collides. PHP routes to
    // Builder's real method; emitting our synthetic would be misleading.
    let view = empty_view();
    let mut index = empty_index();
    index
        .eloquent_builder
        .push(crate::laravel_introspector::ParsedMethod {
            name: "whereDate".to_string(),
            source_class: "Illuminate\\Database\\Eloquent\\Builder".to_string(),
            signature: "public function whereDate($column, $op, $value)".to_string(),
            return_type: None,
            summary: None,
            doc_body: None,
        });
    let cols = vec![col("date", "Carbon", ColumnSource::Fillable)];
    let items = dynamic_where_to_items(&view, &index, &cols);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        !labels.contains(&"whereDate"),
        "whereDate must be suppressed when Builder defines a real method; got {labels:?}"
    );
    // orWhereDate is NOT a real Builder method, so the synthetic survives.
    assert!(
        labels.contains(&"orWhereDate"),
        "orWhereDate should still be emitted (no real method collision); got {labels:?}"
    );
}

#[test]
fn dynamic_where_skips_when_local_scope_exists() {
    // A model's `scopeWhereEmail` would surface as `whereEmail()` —
    // synthetic emission would collide. Same skip rule applies.
    let mut view = empty_view();
    view.scopes.push(crate::laravel_introspector::ScopeInfo {
        name: "whereEmail".to_string(),
        source_class: "App\\Models\\Portfolio".to_string(),
        signature: "public function whereEmail($q): Builder".to_string(),
        doc_body: None,
        summary: None,
        style: crate::laravel_introspector::ScopeStyle::Prefix,
    });
    let index = empty_index();
    let cols = vec![col("email", "string", ColumnSource::Fillable)];
    let items = dynamic_where_to_items(&view, &index, &cols);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        !labels.contains(&"whereEmail"),
        "whereEmail must be suppressed when a local scope owns the name; got {labels:?}"
    );
}

#[test]
fn dynamic_where_detail_carries_column_php_type() {
    // The row's `detail` field shows the column's PHP type so users
    // can see at a glance what `where{Column}` expects.
    let view = empty_view();
    let index = empty_index();
    let cols = vec![col("user_id", "int", ColumnSource::Convention)];
    let items = dynamic_where_to_items(&view, &index, &cols);
    let item = items
        .iter()
        .find(|i| i.label == "whereUserId")
        .expect("whereUserId");
    assert_eq!(item.detail.as_deref(), Some("int"));
}

#[test]
fn dynamic_where_doc_panel_includes_provenance_line() {
    use tower_lsp::lsp_types::{Documentation, MarkupContent};
    let view = empty_view();
    let index = empty_index();
    let cols = vec![col("email", "string", ColumnSource::Fillable)];
    let items = dynamic_where_to_items(&view, &index, &cols);
    let item = items
        .iter()
        .find(|i| i.label == "whereEmail")
        .expect("whereEmail");
    let value = match item.documentation.as_ref() {
        Some(Documentation::MarkupContent(MarkupContent { value, .. })) => value.clone(),
        other => panic!("expected MarkupContent, got {other:?}"),
    };
    // Header shows the synthetic method name.
    assert!(value.contains("**whereEmail**"), "panel: {value}");
    // Summary shows the underlying column.
    assert!(
        value.contains("Eloquent dynamic where (email = ?)"),
        "panel: {value}"
    );
    // Signature carries the column's PHP type.
    assert!(value.contains("string $value"), "panel: {value}");
    // Provenance section credits $fillable.
    assert!(value.contains("declared in `$fillable`"), "panel: {value}");
}

#[test]
fn dynamic_where_provenance_distinguishes_all_sources() {
    // Every ColumnSource variant should produce a distinct, human-readable
    // provenance line. Spot-check each.
    let view = empty_view();
    let index = empty_index();
    let cols = vec![
        col("a", "mixed", ColumnSource::Fillable),
        col("b", "array", ColumnSource::Cast),
        col("c", "Carbon", ColumnSource::Dates),
        col("d", "mixed", ColumnSource::Attributes),
        col("e", "int", ColumnSource::Convention),
        col("f", "Carbon", ColumnSource::Trait),
        col("g", "string", ColumnSource::ParentClass),
        col("h", "mixed", ColumnSource::DatabaseSchema),
    ];
    let items = dynamic_where_to_items(&view, &index, &cols);
    use tower_lsp::lsp_types::{Documentation, MarkupContent};
    let panels: Vec<String> = items
        .iter()
        .filter(|i| i.label.starts_with("where") && !i.label.starts_with("orWhere"))
        .map(|i| match i.documentation.as_ref() {
            Some(Documentation::MarkupContent(MarkupContent { value, .. })) => value.clone(),
            _ => String::new(),
        })
        .collect();
    assert!(panels.iter().any(|p| p.contains("declared in `$fillable`")));
    assert!(panels.iter().any(|p| p.contains("declared in `$casts`")));
    assert!(panels.iter().any(|p| p.contains("declared in `$dates`")));
    assert!(panels
        .iter()
        .any(|p| p.contains("declared in `$attributes`")));
    assert!(panels.iter().any(|p| p.contains("Laravel convention")));
    assert!(panels
        .iter()
        .any(|p| p.contains("implied by composed trait")));
    assert!(panels.iter().any(|p| p.contains("implied by parent class")));
    assert!(panels.iter().any(|p| p.contains("from live DB schema")));
}

#[test]
fn dynamic_where_emits_nothing_for_empty_columns() {
    let view = empty_view();
    let index = empty_index();
    let items = dynamic_where_to_items(&view, &index, &[]);
    assert!(items.is_empty());
}

#[test]
fn dynamic_where_items_carry_method_kind() {
    let view = empty_view();
    let index = empty_index();
    let cols = vec![col("email", "string", ColumnSource::Fillable)];
    let items = dynamic_where_to_items(&view, &index, &cols);
    for item in &items {
        assert_eq!(
            item.kind,
            Some(tower_lsp::lsp_types::CompletionItemKind::METHOD)
        );
    }
}
