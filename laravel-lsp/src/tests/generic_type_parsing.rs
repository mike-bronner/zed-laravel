use crate::LaravelLanguageServer;
use laravel_lsp::blade_php_block::extract_php_block_assignments;
use laravel_lsp::php_class::{
    extract_method_return_type, iterable_element_type, normalize_generic_type, parse_generic_args,
};

#[test]
fn test_parse_generic_args_single() {
    let result = parse_generic_args("Collection<Audit>");
    assert_eq!(
        result,
        Some(("Collection".to_string(), vec!["Audit".to_string()]))
    );
}

#[test]
fn test_parse_generic_args_pair() {
    let result = parse_generic_args("LengthAwarePaginator<int, Audit>");
    assert_eq!(
        result,
        Some((
            "LengthAwarePaginator".to_string(),
            vec!["int".to_string(), "Audit".to_string()]
        ))
    );
}

#[test]
fn test_parse_generic_args_nested() {
    let result = parse_generic_args("Foo<Bar<Baz>, Qux>");
    assert_eq!(
        result,
        Some((
            "Foo".to_string(),
            vec!["Bar<Baz>".to_string(), "Qux".to_string()]
        ))
    );
}

#[test]
fn test_parse_generic_args_none() {
    assert_eq!(parse_generic_args("Collection"), None);
}

#[test]
fn test_iterable_element_type_collection() {
    assert_eq!(
        iterable_element_type("Collection<Audit>"),
        Some("Audit".to_string())
    );
}

#[test]
fn test_iterable_element_type_paginator() {
    assert_eq!(
        iterable_element_type("LengthAwarePaginator<int, Audit>"),
        Some("Audit".to_string())
    );
}

#[test]
fn test_iterable_element_type_no_generics() {
    assert_eq!(iterable_element_type("Collection"), None);
}

#[test]
fn test_normalize_generic_type_strips_namespaces() {
    assert_eq!(
        normalize_generic_type("\\Illuminate\\Pagination\\LengthAwarePaginator<int, \\App\\Audit>"),
        "LengthAwarePaginator<int, Audit>"
    );
}

#[test]
fn test_extract_method_return_type_declared() {
    let php = r#"<?php
class Foo {
    public function audits(): LengthAwarePaginator
    {
        return $this->paginate();
    }
}
"#;
    assert_eq!(
        extract_method_return_type(php, "audits"),
        Some("LengthAwarePaginator".to_string())
    );
}

#[test]
fn test_loop_var_properties_has_standard_members() {
    let props = LaravelLanguageServer::loop_var_properties();
    let names: Vec<&str> = props.iter().map(|p| p.name.as_str()).collect();
    for expected in &[
        "index",
        "iteration",
        "first",
        "last",
        "even",
        "odd",
        "count",
        "depth",
        "parent",
    ] {
        assert!(
            names.contains(expected),
            "loop var props missing: {}",
            expected
        );
    }
}

#[test]
fn test_extract_php_block_assignment_loop_alias() {
    let content = r#"
@foreach($items as $item)
    @php
        $outerLoop = $loop;
    @endphp
    {{ $outerLoop->index }}
@endforeach
"#;
    let vars = extract_php_block_assignments(content);
    assert!(vars.contains(&("outerLoop".to_string(), "Loop".to_string())));
}

#[test]
fn test_extract_php_block_assignment_arbitrary_rhs() {
    let content = r#"
@php
    $count = count($items);
@endphp
"#;
    let vars = extract_php_block_assignments(content);
    assert!(vars.contains(&("count".to_string(), "mixed".to_string())));
}

#[test]
fn test_extract_php_block_assignment_multiline_ternary() {
    // Verifies that complex multiline RHS expressions still register the variable
    // (with "mixed" type) so the diagnostic doesn't false-positive.
    let content = r#"
@php
    $modified = $this->field
        ? array_filter($audit->getModified(), fn ($k) => $k === $this->field, ARRAY_FILTER_USE_KEY)
        : $audit->getModified();
@endphp
"#;
    let vars = extract_php_block_assignments(content);
    assert!(vars.iter().any(|(n, _)| n == "modified"));
}

#[test]
fn test_extract_php_block_assignment_multiple_in_one_block() {
    let content = r#"
@php
    $a = $loop;
    $b = 42;
@endphp
"#;
    let vars = extract_php_block_assignments(content);
    assert!(vars.contains(&("a".to_string(), "Loop".to_string())));
    assert!(vars.contains(&("b".to_string(), "mixed".to_string())));
}

#[test]
fn test_extract_method_return_type_phpdoc_preferred() {
    let php = r#"<?php
class Foo {
    /**
     * @return LengthAwarePaginator<int, Audit>
     */
    #[Computed]
    public function audits(): LengthAwarePaginator
    {
        return $this->paginate();
    }
}
"#;
    assert_eq!(
        extract_method_return_type(php, "audits"),
        Some("LengthAwarePaginator<int, Audit>".to_string())
    );
}
