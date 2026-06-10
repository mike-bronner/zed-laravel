//! Tests for member-access capture in `extract_all_php_patterns` —
//! property-form (`$user->email`, `$this->profile`, `$user?->name`, M2) and
//! call-form (`$user->active()`, `User::whereEmail()`, #77).
//!
//! This is the raw-capture half of the magic-member semantic-index work.
//! Resolution/classification of the receiver happens later, so these tests
//! assert only what the capture layer knows: the member name, the raw
//! receiver text + byte range, nullsafe-ness, the access form, and the
//! member-name position.

use super::super::*;
use crate::parser::{language_php, parse_php};
use crate::salsa_impl::AccessForm;

/// Extract member accesses from PHP source. The returned matches borrow
/// `php` (not the tree), so dropping the tree here is fine.
fn extract(php: &str) -> Vec<MemberAccessMatch<'_>> {
    let tree = parse_php(php).expect("Should parse PHP");
    let lang = language_php();
    extract_all_php_patterns(&tree, php, &lang)
        .expect("Should extract patterns")
        .member_accesses
}

#[test]
fn captures_simple_property_access() {
    let matches = extract("<?php\n$email = $user->email;\n");
    assert_eq!(matches.len(), 1);
    let m = &matches[0];
    assert_eq!(m.member, "email");
    assert_eq!(m.receiver, "$user");
    assert!(!m.is_nullsafe);
}

#[test]
fn captures_this_property_access() {
    let php = r#"<?php
class Foo {
    public function bar() {
        return $this->profile;
    }
}
"#;
    let matches = extract(php);
    let profile = matches
        .iter()
        .find(|m| m.member == "profile")
        .expect("profile access");
    assert_eq!(profile.receiver, "$this");
}

#[test]
fn captures_nullsafe_access() {
    let matches = extract("<?php\n$name = $user?->name;\n");
    assert_eq!(matches.len(), 1);
    let m = &matches[0];
    assert_eq!(m.member, "name");
    assert_eq!(m.receiver, "$user");
    assert!(m.is_nullsafe, "?-> should set is_nullsafe");
}

#[test]
fn property_form_is_marked_property() {
    let matches = extract("<?php\n$email = $user->email;\n");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].form, AccessForm::Property);
}

#[test]
fn captures_instance_method_calls() {
    // `$user->posts()` — call-form capture (#77). Raw capture only;
    // classification later prunes non-magic calls.
    let matches = extract("<?php\n$posts = $user->posts();\n");
    assert_eq!(matches.len(), 1);
    let m = &matches[0];
    assert_eq!(m.member, "posts");
    assert_eq!(m.receiver, "$user");
    assert_eq!(m.form, AccessForm::InstanceCall);
    assert!(!m.is_nullsafe);
}

#[test]
fn captures_static_method_calls() {
    let matches = extract("<?php\n$active = User::active();\n");
    assert_eq!(matches.len(), 1);
    let m = &matches[0];
    assert_eq!(m.member, "active");
    assert_eq!(m.receiver, "User");
    assert_eq!(m.form, AccessForm::StaticCall);
}

#[test]
fn captures_chained_call_receivers() {
    // `User::query()->active()` — two call-form sites: `query` (static, on
    // `User`) and `active` (instance, on the `User::query()` expression).
    let matches = extract("<?php\n$active = User::query()->active();\n");
    let query = matches.iter().find(|m| m.member == "query").expect("query");
    assert_eq!(query.form, AccessForm::StaticCall);
    assert_eq!(query.receiver, "User");
    let active = matches
        .iter()
        .find(|m| m.member == "active")
        .expect("active");
    assert_eq!(active.form, AccessForm::InstanceCall);
    assert_eq!(active.receiver, "User::query()");
}

#[test]
fn captures_nullsafe_method_calls() {
    let matches = extract("<?php\n$posts = $user?->posts();\n");
    assert_eq!(matches.len(), 1);
    assert!(matches[0].is_nullsafe, "?->() should set is_nullsafe");
    assert_eq!(matches[0].form, AccessForm::InstanceCall);
}

#[test]
fn ignores_dynamic_member_names() {
    // `$user->$prop` — the name is a variable, not a static identifier.
    let matches = extract("<?php\n$value = $user->$prop;\n");
    assert!(
        matches.is_empty(),
        "dynamic member names must not be captured"
    );
}

#[test]
fn captures_chained_property_reads_at_each_hop() {
    // `$user->profile->name` is two member accesses: `profile` on `$user`,
    // and `name` on `$user->profile`. Both are worth indexing.
    let matches = extract("<?php\n$n = $user->profile->name;\n");
    assert_eq!(matches.len(), 2);

    let profile = matches
        .iter()
        .find(|m| m.member == "profile")
        .expect("profile hop");
    assert_eq!(profile.receiver, "$user");

    let name = matches
        .iter()
        .find(|m| m.member == "name")
        .expect("name hop");
    assert_eq!(name.receiver, "$user->profile");
}

#[test]
fn member_name_position_points_at_member_not_receiver() {
    // Line layout (0-based rows): row 0 is `<?php`, row 1 is the access.
    let php = "<?php\n$x = $user->email;\n";
    let matches = extract(php);
    let m = &matches[0];

    assert_eq!(m.row, 1, "access is on row 1");
    // `$x = $user->` is 12 chars (cols 0..=11); `email` starts at col 12.
    assert_eq!(
        m.column, 12,
        "column should point at the member name 'email'"
    );
    assert_eq!(m.end_column, 17, "end_column should be one past 'email'");

    // Receiver byte range should slice `$user` out of the source.
    assert_eq!(&php[m.receiver_byte_start..m.receiver_byte_end], "$user");
    // Member byte range should slice `email`.
    assert_eq!(&php[m.byte_start..m.byte_end], "email");
}

#[test]
fn captures_multiple_accesses_in_one_file() {
    let php = r#"<?php
$a = $user->email;
$b = $post->title;
$c = $this->config;
"#;
    let matches = extract(php);
    let members: Vec<&str> = matches.iter().map(|m| m.member).collect();
    assert_eq!(members.len(), 3);
    assert!(members.contains(&"email"));
    assert!(members.contains(&"title"));
    assert!(members.contains(&"config"));
}
