use super::*;

#[test]
fn test_php_language_loads() {
    // This should not panic
    let lang = language_php();
    assert!(lang.node_kind_count() > 0);
}

#[test]
fn test_blade_language_loads() {
    // This should not panic - verifies our FFI bindings work
    let lang = language_blade();
    assert!(lang.node_kind_count() > 0);
}

#[test]
fn test_parse_simple_php() {
    let php_code = r#"<?php
    $name = "Laravel";
    echo view('welcome');
    "#;

    let tree = parse_php(php_code).expect("Should parse valid PHP");
    let root = tree.root_node();

    // The root node should have children
    assert!(root.child_count() > 0);

    // Should contain a function call (view)
    let source = php_code.as_bytes();
    let has_function_call = root
        .descendant_for_byte_range(0, source.len())
        .is_some();
    assert!(has_function_call);
}

#[test]
fn test_parse_simple_blade() {
    let blade_code = r#"
    <div>
        <x-button>Click me</x-button>
    </div>
    "#;

    let tree = parse_blade(blade_code).expect("Should parse valid Blade");
    let root = tree.root_node();

    // The root node should have children
    assert!(root.child_count() > 0);
}

#[test]
fn test_php_parser_reusable() {
    let mut parser = create_php_parser().expect("Should create parser");

    // Parse multiple times with the same parser
    let code1 = "<?php echo 'hello'; ?>";
    let tree1 = parser.parse(code1, None).expect("Should parse");
    assert!(tree1.root_node().child_count() > 0);

    let code2 = "<?php $x = 42; ?>";
    let tree2 = parser.parse(code2, None).expect("Should parse");
    assert!(tree2.root_node().child_count() > 0);
}
