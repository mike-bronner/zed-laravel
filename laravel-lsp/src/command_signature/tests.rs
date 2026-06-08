//! Unit tests for Artisan command signature extraction.

use super::*;

/// A minimal but realistic Artisan command class. `indent` controls how far the
/// `$signature` line is indented so column assertions stay readable.
fn command_class(signature_line: &str) -> String {
    format!(
        "<?php\n\nnamespace App\\Console\\Commands;\n\nuse Illuminate\\Console\\Command;\n\nclass SendEmails extends Command\n{{\n    {}\n\n    public function handle()\n    {{\n        //\n    }}\n}}\n",
        signature_line
    )
}

#[test]
fn command_name_is_leading_token() {
    assert_eq!(command_name_from_signature("emails:send"), "emails:send");
    assert_eq!(
        command_name_from_signature("emails:send {user} {--force}"),
        "emails:send"
    );
    // Leading whitespace and tabs are tolerated.
    assert_eq!(
        command_name_from_signature("  app:cleanup\t{--dry}"),
        "app:cleanup"
    );
    // All-whitespace yields an empty name (treated as "no command").
    assert_eq!(command_name_from_signature("   "), "");
}

#[test]
fn extends_command_recognises_bare_and_qualified() {
    assert!(extends_console_command("class Foo extends Command {}"));
    assert!(extends_console_command(
        "class Foo extends \\Illuminate\\Console\\Command {}"
    ));
    assert!(extends_console_command(
        "class Foo extends Illuminate\\Console\\Command {}"
    ));
    // A project intermediate base whose name ends in `Command` still counts.
    assert!(extends_console_command("class Foo extends BaseCommand {}"));
    // Unrelated base classes do not.
    assert!(!extends_console_command("class Foo extends Model {}"));
    assert!(!extends_console_command("class Foo extends Controller {}"));
}

#[test]
fn extracts_simple_signature() {
    let src = command_class("protected $signature = 'emails:send';");
    let sig = extract_command_signature(&src).expect("signature should be found");
    assert_eq!(sig.name, "emails:send");
    assert_eq!(sig.raw_signature, "emails:send");
}

#[test]
fn extracts_signature_with_arguments_and_options() {
    let src = command_class("protected $signature = 'emails:send {user} {--force}';");
    let sig = extract_command_signature(&src).expect("signature should be found");
    assert_eq!(sig.name, "emails:send");
    assert_eq!(sig.raw_signature, "emails:send {user} {--force}");
}

#[test]
fn extracts_double_quoted_signature() {
    let src = command_class(r#"protected $signature = "queue:work {--once}";"#);
    let sig = extract_command_signature(&src).expect("signature should be found");
    assert_eq!(sig.name, "queue:work");
    assert_eq!(sig.raw_signature, "queue:work {--once}");
}

#[test]
fn signature_position_brackets_string_content() {
    let src = command_class("protected $signature = 'emails:send';");
    let sig = extract_command_signature(&src).expect("signature should be found");

    // The signature line is line index 8 in the fixture (0-based):
    // 0:<?php 1:blank 2:namespace 3:blank 4:use 5:blank 6:class 7:{ 8:signature
    assert_eq!(sig.line, 8);

    // The content starts right after the opening quote. The line is indented 4
    // spaces; `protected $signature = '` is the prefix.
    let line_text = src.lines().nth(sig.line as usize).unwrap();
    let extracted = &line_text[sig.start_column as usize..sig.end_column as usize];
    assert_eq!(extracted, "emails:send");
}

#[test]
fn missing_signature_returns_none() {
    // Extends Command but declares no $signature (dynamic or inherited).
    let src = command_class("protected $description = 'Send queued emails';");
    assert!(extract_command_signature(&src).is_none());
}

#[test]
fn non_command_class_returns_none() {
    let src =
        "<?php\nclass UserController extends Controller {\n    protected $signature = 'nope';\n}\n";
    assert!(extract_command_signature(src).is_none());
}

#[test]
fn dynamic_signature_is_skipped_gracefully() {
    // Interpolated / concatenated signatures are not statically resolvable;
    // extraction declines rather than guessing a wrong command name.
    let src = command_class("protected $signature = 'app:' . $this->suffix;");
    let sig = extract_command_signature(&src);
    // The leading literal still parses to a name; it must not panic. Either a
    // best-effort `app:` name or None is acceptable — assert no panic and a
    // sane name when present.
    if let Some(sig) = sig {
        assert_eq!(sig.name, "app:");
    }
}

#[test]
fn malformed_class_does_not_panic() {
    // Truncated / malformed source must be handled gracefully.
    let src = "<?php class Broken extends Command { protected $signature = ";
    assert!(extract_command_signature(src).is_none());
}
