//! Unit tests for Artisan command-string call-site location.

use super::*;

#[test]
fn finds_direct_dispatch() {
    let src = "<?php\nArtisan::call('emails:send', ['--force' => true]);\n";
    let sites = extract_command_call_sites(src);
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].raw, "emails:send");
    assert_eq!(sites[0].command_name(), "emails:send");
    assert_eq!(sites[0].line, 1);
}

#[test]
fn finds_artisan_queue() {
    let src = "<?php\nArtisan::queue('emails:send');\n";
    let sites = extract_command_call_sites(src);
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].command_name(), "emails:send");
}

#[test]
fn finds_scheduler_command() {
    let src = "<?php\n$schedule->command('emails:send')->daily();\n";
    let sites = extract_command_call_sites(src);
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].command_name(), "emails:send");
}

#[test]
fn finds_testing_artisan_helper() {
    let src = "<?php\n$this->artisan('emails:send')->assertExitCode(0);\n";
    let sites = extract_command_call_sites(src);
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].command_name(), "emails:send");
}

#[test]
fn finds_all_four_patterns_in_one_file() {
    let src = "<?php\n\
        Artisan::call('a:one');\n\
        Artisan::queue('b:two');\n\
        $schedule->command('c:three')->daily();\n\
        $this->artisan('d:four');\n";
    let names: Vec<String> = extract_command_call_sites(src)
        .iter()
        .map(|s| s.command_name().to_string())
        .collect();
    assert_eq!(names, vec!["a:one", "b:two", "c:three", "d:four"]);
}

#[test]
fn double_quoted_argument() {
    let src = "<?php\nArtisan::call(\"queue:work\");\n";
    let sites = extract_command_call_sites(src);
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].command_name(), "queue:work");
}

#[test]
fn command_name_strips_inline_options() {
    let src = "<?php\nArtisan::call('emails:send --force');\n";
    let sites = extract_command_call_sites(src);
    assert_eq!(sites[0].raw, "emails:send --force");
    assert_eq!(sites[0].command_name(), "emails:send");
}

#[test]
fn position_brackets_string_content() {
    let src = "<?php\nArtisan::call('emails:send');\n";
    let site = &extract_command_call_sites(src)[0];
    let line_text = src.lines().nth(site.line as usize).unwrap();
    let extracted = &line_text[site.start_column as usize..site.end_column as usize];
    assert_eq!(extracted, "emails:send");
}

#[test]
fn cursor_inside_string_resolves() {
    // Line 1: `Artisan::call('emails:send');` — the string content starts at
    // column 14 (after `Artisan::call('`).
    let src = "<?php\nArtisan::call('emails:send');\n";
    let hit = command_call_at_position(src, 1, 16).expect("cursor is inside the string");
    assert_eq!(hit.command_name(), "emails:send");
}

#[test]
fn cursor_outside_string_returns_none() {
    let src = "<?php\nArtisan::call('emails:send');\n";
    // Column 0 is on `A` of `Artisan`, outside the string content.
    assert!(command_call_at_position(src, 1, 0).is_none());
    // A different line entirely.
    assert!(command_call_at_position(src, 0, 5).is_none());
}

#[test]
fn unrelated_method_calls_are_ignored() {
    let src = "<?php\n$user->name('foo');\nroute('home');\nview('welcome');\n";
    assert!(extract_command_call_sites(src).is_empty());
}

#[test]
fn empty_command_string_is_skipped() {
    let src = "<?php\nArtisan::call('');\n";
    assert!(extract_command_call_sites(src).is_empty());
}
