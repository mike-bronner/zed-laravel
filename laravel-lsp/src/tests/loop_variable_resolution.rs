use laravel_lsp::blade_loops::{
    BladeLoopType, find_loop_blocks, get_enclosing_loops, parse_foreach_iterable,
    parse_foreach_variables, parse_for_variables,
};

#[test]
fn test_parse_foreach_single_variable() {
    let vars = parse_foreach_variables("($users as $user)");
    assert_eq!(vars, vec![("user".to_string(), "mixed".to_string())]);
}

#[test]
fn test_parse_foreach_key_value() {
    let vars = parse_foreach_variables("($items as $key => $value)");
    assert_eq!(vars, vec![
        ("key".to_string(), "mixed".to_string()),
        ("value".to_string(), "mixed".to_string()),
    ]);
}

#[test]
fn test_parse_foreach_with_spaces() {
    let vars = parse_foreach_variables("( $users as $user )");
    assert_eq!(vars, vec![("user".to_string(), "mixed".to_string())]);
}

#[test]
fn test_parse_for_variable() {
    let vars = parse_for_variables("($i = 0; $i < 10; $i++)");
    assert_eq!(vars, vec![("i".to_string(), "int".to_string())]);
}

#[test]
fn test_parse_for_variable_no_spaces() {
    let vars = parse_for_variables("($i=0;$i<10;$i++)");
    assert_eq!(vars, vec![("i".to_string(), "int".to_string())]);
}

#[test]
fn test_find_loop_blocks_single_foreach() {
    let content = r#"
@foreach($users as $user)
    {{ $user->name }}
@endforeach
"#;
    let blocks = find_loop_blocks(content);
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].loop_type, BladeLoopType::Foreach);
    assert_eq!(blocks[0].variables, vec![("user".to_string(), "mixed".to_string())]);
    assert_eq!(blocks[0].start_line, 1);
    assert_eq!(blocks[0].end_line, Some(3));
}

#[test]
fn test_find_loop_blocks_nested() {
    let content = r#"
@foreach($categories as $category)
    @foreach($category->items as $item)
        {{ $item->name }}
    @endforeach
@endforeach
"#;
    let blocks = find_loop_blocks(content);
    assert_eq!(blocks.len(), 2);

    // Inner loop ends first
    let inner = blocks.iter().find(|b| b.variables.iter().any(|(n, _)| n == "item")).unwrap();
    assert_eq!(inner.start_line, 2);
    assert_eq!(inner.end_line, Some(4));

    // Outer loop
    let outer = blocks.iter().find(|b| b.variables.iter().any(|(n, _)| n == "category")).unwrap();
    assert_eq!(outer.start_line, 1);
    assert_eq!(outer.end_line, Some(5));
}

#[test]
fn test_find_loop_blocks_for() {
    let content = r#"
@for($i = 0; $i < 10; $i++)
    {{ $i }}
@endfor
"#;
    let blocks = find_loop_blocks(content);
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].loop_type, BladeLoopType::For);
    assert_eq!(blocks[0].variables, vec![("i".to_string(), "int".to_string())]);
}

#[test]
fn test_find_loop_blocks_forelse() {
    let content = r#"
@forelse($users as $user)
    {{ $user->name }}
@empty
    No users
@endforelse
"#;
    let blocks = find_loop_blocks(content);
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].loop_type, BladeLoopType::Forelse);
    assert_eq!(blocks[0].variables, vec![("user".to_string(), "mixed".to_string())]);
}

#[test]
fn test_get_enclosing_loops_inside() {
    let content = r#"
@foreach($users as $user)
    {{ $user->name }}
@endforeach
"#;
    // Line 2 (0-indexed) is inside the loop
    let enclosing = get_enclosing_loops(content, 2);
    assert_eq!(enclosing.len(), 1);
    assert_eq!(enclosing[0].variables, vec![("user".to_string(), "mixed".to_string())]);
}

#[test]
fn test_get_enclosing_loops_outside() {
    let content = r#"
@foreach($users as $user)
    {{ $user->name }}
@endforeach
"#;
    // Line 0 and 4 are outside the loop
    let before = get_enclosing_loops(content, 0);
    assert_eq!(before.len(), 0);

    let after = get_enclosing_loops(content, 4);
    assert_eq!(after.len(), 0);
}

#[test]
fn test_get_enclosing_loops_nested() {
    let content = r#"
@foreach($categories as $category)
    @foreach($category->items as $item)
        {{ $item->name }}
    @endforeach
@endforeach
"#;
    // Line 3 is inside both loops
    let enclosing = get_enclosing_loops(content, 3);
    assert_eq!(enclosing.len(), 2);

    // Innermost first
    assert!(enclosing[0].variables.iter().any(|(n, _)| n == "item"));
    assert!(enclosing[1].variables.iter().any(|(n, _)| n == "category"));
}

#[test]
fn test_get_enclosing_loops_between_nested() {
    let content = r#"
@foreach($categories as $category)
    @foreach($category->items as $item)
        {{ $item->name }}
    @endforeach
    {{ $category->name }}
@endforeach
"#;
    // Line 5 is only inside outer loop (between inner endforeach and outer endforeach)
    let enclosing = get_enclosing_loops(content, 5);
    assert_eq!(enclosing.len(), 1);
    assert!(enclosing[0].variables.iter().any(|(n, _)| n == "category"));
}

#[test]
fn test_while_loop() {
    let content = r#"
@while($condition)
    something
@endwhile
"#;
    let blocks = find_loop_blocks(content);
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].loop_type, BladeLoopType::While);
    assert!(blocks[0].variables.is_empty()); // @while doesn't introduce variables

    // But cursor inside should still get $loop
    let enclosing = get_enclosing_loops(content, 2);
    assert_eq!(enclosing.len(), 1);
}

#[test]
fn test_parse_foreach_iterable_simple() {
    assert_eq!(
        parse_foreach_iterable("($users as $user)"),
        Some("$users".to_string())
    );
}

#[test]
fn test_parse_foreach_iterable_this_member() {
    assert_eq!(
        parse_foreach_iterable("($this->audits as $audit)"),
        Some("$this->audits".to_string())
    );
}

#[test]
fn test_parse_foreach_iterable_with_key() {
    assert_eq!(
        parse_foreach_iterable("($items as $key => $value)"),
        Some("$items".to_string())
    );
}

#[test]
fn test_parse_foreach_iterable_chained_expr() {
    assert_eq!(
        parse_foreach_iterable("($category->items as $item)"),
        Some("$category->items".to_string())
    );
}

#[test]
fn test_find_loop_blocks_captures_iterable() {
    let content = r#"
@foreach($this->audits as $audit)
    {{ $audit->id }}
@endforeach
"#;
    let blocks = find_loop_blocks(content);
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].iterable, Some("$this->audits".to_string()));
}
