//! Tests for property-form member-access capture (`$user->email`,
//! `$this->profile`, `$user?->name`) in `extract_all_php_patterns`.
//!
//! This is the raw-capture half of the magic-member semantic-index work (M2).
//! Resolution/classification of the receiver is a later milestone, so these
//! tests assert only what the capture layer knows: the member name, the raw
//! receiver text + byte range, nullsafe-ness, and the member-name position.

use super::super::*;
use crate::parser::{language_php, parse_php};

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
fn ignores_method_calls() {
    // `$user->posts()` is a member_call_expression, not a member_access_expression.
    // It is covered by builder-chain extraction, never by this capture.
    let php = r#"<?php
$posts = $user->posts();
$active = User::query()->active();
"#;
    let matches = extract(php);
    assert!(
        matches.is_empty(),
        "method calls must not be captured as property accesses, got: {:?}",
        matches.iter().map(|m| m.member).collect::<Vec<_>>()
    );
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
