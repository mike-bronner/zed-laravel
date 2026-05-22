use super::*;

#[test]
fn find_slot_variable_matches_exact_name() {
    let source = "<div>\n    <h2>{{ $title }}</h2>\n    {{ $slot }}\n</div>";
    let result = find_slot_variable_line(source, "title");
    assert!(result.is_some(), "should locate $title");
    let (line, _) = result.unwrap();
    assert_eq!(line, 1);
}

#[test]
fn find_slot_variable_does_not_match_prefix() {
    let source = "<div>\n    {{ $titleSlot }}\n    {{ $title }}\n</div>";
    let (line, _) = find_slot_variable_line(source, "title").unwrap();
    // Must skip $titleSlot on line 1 and find $title on line 2.
    assert_eq!(line, 2);
}

#[test]
fn find_slot_variable_returns_none_when_absent() {
    let source = "<div>{{ $slot }}</div>";
    assert!(find_slot_variable_line(source, "title").is_none());
}

#[test]
fn find_slot_at_position_recognizes_named_slot() {
    let source = "<x-card>\n    <x-slot:title>Hello</x-slot:title>\n</x-card>\n";
    // Cursor inside <x-slot:title — line 1, col 7 (the 's' of 'slot')
    let slot = find_slot_at_position(source, 1, 7);
    assert!(slot.is_some(), "should detect slot at cursor");
    assert_eq!(slot.unwrap().name, "title");
}

#[test]
fn find_slot_at_position_handles_cursor_on_open_bracket() {
    let source = "<x-card>\n    <x-slot:title>Hello</x-slot:title>\n</x-card>\n";
    // Cursor on the opening `<` of <x-slot:title>
    let slot = find_slot_at_position(source, 1, 4).unwrap();
    assert_eq!(slot.name, "title");
}

#[test]
fn find_slot_at_position_handles_cursor_on_close_bracket() {
    let source = "<x-card>\n    <x-slot:title>Hello</x-slot:title>\n</x-card>\n";
    // Cursor on the `>` closing the opening tag — "<x-slot:title>" ends at col 18
    let slot = find_slot_at_position(source, 1, 17).unwrap();
    assert_eq!(slot.name, "title");
}

#[test]
fn find_slot_at_position_handles_cursor_on_slot_name() {
    let source = "<x-card>\n    <x-slot:title>Hello</x-slot:title>\n</x-card>\n";
    // Cursor on the `t` of `title` (col 12)
    let slot = find_slot_at_position(source, 1, 12).unwrap();
    assert_eq!(slot.name, "title");
}

#[test]
fn find_slot_at_position_handles_closing_tag() {
    let source = "<x-card>\n    <x-slot:title>Hello</x-slot:title>\n</x-card>\n";
    // Cursor on the closing </x-slot:title> after "Hello"
    // Line 1: `    <x-slot:title>Hello</x-slot:title>`
    // </x-slot:title> starts at col 23
    let slot = find_slot_at_position(source, 1, 25).unwrap();
    assert_eq!(slot.name, "title");
}

#[test]
fn find_slot_at_position_handles_attribute_form() {
    // <x-slot name="buttons"> — legacy syntax, slot name in the attribute
    let source = "<x-card>\n    <x-slot name=\"buttons\">Footer</x-slot>\n</x-card>\n";
    // Cursor on `s` of "<x-slot " (col 8 from start of <)
    let slot = find_slot_at_position(source, 1, 8).unwrap();
    assert_eq!(slot.name, "buttons");
}

#[test]
fn find_slot_at_position_handles_attribute_form_single_quotes() {
    let source = "<x-card>\n    <x-slot name='header'>x</x-slot>\n</x-card>\n";
    let slot = find_slot_at_position(source, 1, 8).unwrap();
    assert_eq!(slot.name, "header");
}

#[test]
fn find_slot_at_position_handles_bare_default_slot() {
    // <x-slot> with no colon, no name attribute → defaults to "slot"
    let source = "<x-card>\n    <x-slot>Default body</x-slot>\n</x-card>\n";
    let slot = find_slot_at_position(source, 1, 8).unwrap();
    assert_eq!(slot.name, "slot");
}

#[test]
fn find_slot_at_position_handles_self_closing_form() {
    let source = "<x-card>\n    <x-slot:title />\n</x-card>\n";
    let slot = find_slot_at_position(source, 1, 8).unwrap();
    assert_eq!(slot.name, "title");
}

#[test]
fn find_slot_at_position_does_not_match_slotmachine() {
    // Tag prefix collision — <x-slotmachine> shouldn't be treated as a slot
    let source = "<div>\n    <x-slotmachine>spin</x-slotmachine>\n</div>\n";
    let slot = find_slot_at_position(source, 1, 8);
    assert!(slot.is_none(), "<x-slotmachine> is not a slot tag");
}

#[test]
fn extract_name_attribute_rejects_substring_collisions() {
    // `wire:name="..."` shouldn't be misread as `name="..."`
    assert!(extract_name_attribute(" wire:name=\"x\"").is_none());
    // Real name attribute still works
    assert_eq!(extract_name_attribute(" name=\"buttons\"").unwrap(), "buttons");
}

#[test]
fn find_slot_at_position_skips_when_cursor_not_in_any_tag() {
    let source = "<x-card>\n    <x-slot:title>Hello</x-slot:title>\n</x-card>\n";
    // Cursor inside "Hello" body, not in any tag
    let slot = find_slot_at_position(source, 1, 21);
    assert!(slot.is_none());
}

#[test]
fn find_slot_at_position_returns_none_for_regular_component() {
    let source = "<x-card>\n    <x-button>Click</x-button>\n</x-card>\n";
    // Cursor inside <x-button>
    let slot = find_slot_at_position(source, 1, 7);
    assert!(slot.is_none(), "regular component should not be matched as slot");
}

#[test]
fn find_enclosing_parent_component_returns_parent_name() {
    let source = "<x-card>\n    <x-slot:title>Hello</x-slot:title>\n</x-card>\n";
    // Byte position of the slot tag start
    let slot_pos = source.find("<x-slot:title>").unwrap();
    let parent = find_enclosing_parent_component(source, slot_pos);
    assert!(parent.is_some(), "should find x-card parent");
    assert_eq!(parent.unwrap().name, "card");
}

#[test]
fn find_enclosing_parent_component_skips_slot_ancestors() {
    // Slot nested inside another slot — outer parent component must be returned.
    let source = "<x-modal>\n    <x-slot:header>\n        <x-slot:icon>!</x-slot:icon>\n    </x-slot:header>\n</x-modal>\n";
    let slot_pos = source.find("<x-slot:icon>").unwrap();
    let parent = find_enclosing_parent_component(source, slot_pos);
    // Inner <x-slot:icon> bubbles past <x-slot:header> to the real component.
    assert_eq!(parent.unwrap().name, "modal");
}
