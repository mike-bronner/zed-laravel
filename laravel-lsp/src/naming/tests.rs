use super::*;

#[test]
fn pascal_to_kebab_single_word() {
    assert_eq!(pascal_to_kebab("Counter"), "counter");
}

#[test]
fn pascal_to_kebab_two_words() {
    assert_eq!(pascal_to_kebab("UserProfile"), "user-profile");
}

#[test]
fn pascal_to_kebab_three_words() {
    assert_eq!(pascal_to_kebab("AdminUserList"), "admin-user-list");
}

#[test]
fn pascal_to_kebab_leading_uppercase_only() {
    // Edge: single capital letter should NOT get a leading dash.
    assert_eq!(pascal_to_kebab("A"), "a");
}

#[test]
fn pascal_to_kebab_empty() {
    assert_eq!(pascal_to_kebab(""), "");
}

#[test]
fn pascal_to_kebab_acronym_simple_form() {
    // Acronyms split per-character. Documented as the simple convention;
    // adequate for Laravel-style class names where acronyms are rare.
    assert_eq!(pascal_to_kebab("HTTPClient"), "h-t-t-p-client");
}

#[test]
fn kebab_to_pascal_roundtrip() {
    let kebab = "admin-user-list";
    assert_eq!(pascal_to_kebab(&kebab_to_pascal(kebab)), kebab);
}

#[test]
fn split_dotted_single() {
    assert_eq!(split_dotted("counter"), vec!["counter"]);
}

#[test]
fn split_dotted_multi() {
    assert_eq!(split_dotted("admin.user-list"), vec!["admin", "user-list"]);
}

#[test]
fn split_dotted_deep() {
    assert_eq!(
        split_dotted("admin.users.show-profile"),
        vec!["admin", "users", "show-profile"]
    );
}

#[test]
fn dotted_to_namespace_single() {
    assert_eq!(dotted_to_namespace("counter"), "Counter");
}

#[test]
fn dotted_to_namespace_kebab_segment() {
    assert_eq!(dotted_to_namespace("user-profile"), "UserProfile");
}

#[test]
fn dotted_to_namespace_nested() {
    assert_eq!(dotted_to_namespace("admin.user-list"), "Admin\\UserList");
}

#[test]
fn dotted_to_class_path_single() {
    assert_eq!(dotted_to_class_path("counter"), "Counter");
}

#[test]
fn dotted_to_class_path_nested() {
    assert_eq!(dotted_to_class_path("admin.user-list"), "Admin/UserList");
}

#[test]
fn dotted_to_class_path_deep() {
    assert_eq!(
        dotted_to_class_path("admin.users.show-profile"),
        "Admin/Users/ShowProfile"
    );
}

#[test]
fn has_emoji_detects_prefix() {
    assert!(has_emoji("\u{26A1}create"));
}

#[test]
fn has_emoji_rejects_plain() {
    assert!(!has_emoji("create"));
}

#[test]
fn has_emoji_rejects_emoji_in_middle() {
    assert!(!has_emoji("create\u{26A1}"));
}

#[test]
fn strip_emoji_removes_bare_prefix() {
    assert_eq!(strip_emoji("\u{26A1}create"), "create");
}

#[test]
fn strip_emoji_removes_prefix_with_text_selector() {
    // U+FE0E forces text presentation. Livewire's PHP regex strips it.
    assert_eq!(strip_emoji("\u{26A1}\u{FE0E}create"), "create");
}

#[test]
fn strip_emoji_removes_prefix_with_emoji_selector() {
    // U+FE0F forces emoji presentation. Livewire's PHP regex also strips it.
    assert_eq!(strip_emoji("\u{26A1}\u{FE0F}create"), "create");
}

#[test]
fn strip_emoji_passes_plain_through() {
    assert_eq!(strip_emoji("create"), "create");
}

#[test]
fn strip_emoji_ignores_emoji_not_at_start() {
    assert_eq!(strip_emoji("create\u{26A1}"), "create\u{26A1}");
}

#[test]
fn with_emoji_adds_when_enabled() {
    assert_eq!(with_emoji("create", true), "\u{26A1}create");
}

#[test]
fn with_emoji_strips_when_disabled() {
    assert_eq!(with_emoji("\u{26A1}create", false), "create");
}

#[test]
fn with_emoji_no_double_prefix() {
    // Idempotent: applying with_emoji(_, true) twice yields the same result.
    let once = with_emoji("create", true);
    let twice = with_emoji(&once, true);
    assert_eq!(once, twice);
}

#[test]
fn with_emoji_disabled_on_plain_is_noop() {
    assert_eq!(with_emoji("create", false), "create");
}
