//! Tests for `middleware_binding_locator`.

use super::*;

// ---- validate_alias_name -------------------------------------------------

#[test]
fn validate_rejects_empty() {
    assert_eq!(validate_alias_name(""), Err(AliasNameError::Empty));
}

#[test]
fn validate_rejects_quotes() {
    assert_eq!(
        validate_alias_name("auth's"),
        Err(AliasNameError::ContainsQuote)
    );
    assert_eq!(
        validate_alias_name("\"auth\""),
        Err(AliasNameError::ContainsQuote)
    );
}

#[test]
fn validate_rejects_whitespace() {
    assert_eq!(
        validate_alias_name("auth user"),
        Err(AliasNameError::ContainsWhitespace)
    );
    assert_eq!(
        validate_alias_name("auth\tuser"),
        Err(AliasNameError::ContainsWhitespace)
    );
}

#[test]
fn validate_accepts_typical_aliases() {
    // Laravel core ships these forms.
    assert!(validate_alias_name("auth").is_ok());
    assert!(validate_alias_name("auth.api").is_ok());
    assert!(validate_alias_name("cache-store").is_ok());
    assert!(validate_alias_name("verified").is_ok());
    assert!(validate_alias_name("can:create-posts").is_ok());
}

// ---- find_alias_in_line --------------------------------------------------

#[test]
fn finds_single_quoted_alias() {
    let line = "    'auth' => \\App\\Http\\Middleware\\Authenticate::class,";
    let (start, end) = find_alias_in_line(line, "auth").expect("should find alias");
    // ' is at col 4, 'a' (first char of alias) is at col 5, alias ends at 9.
    assert_eq!(start, 5);
    assert_eq!(end, 9);
    // Sanity: the slice of the line at that span equals the alias.
    let slice: String = line.chars().skip(start).take(end - start).collect();
    assert_eq!(slice, "auth");
}

#[test]
fn finds_double_quoted_alias_when_single_not_present() {
    let line = "    \"auth\" => Authenticate::class,";
    let (start, end) = find_alias_in_line(line, "auth").expect("should find alias");
    let slice: String = line.chars().skip(start).take(end - start).collect();
    assert_eq!(slice, "auth");
}

#[test]
fn prefers_single_over_double_when_both_present() {
    // Pathological but possible — make sure the deterministic preference
    // holds. Both spans would be valid rewrite targets in isolation; we
    // pick the single-quoted one so behavior is predictable.
    let line = "['auth' => X, \"auth\" => Y]";
    let (start, end) = find_alias_in_line(line, "auth").expect("should find alias");
    let slice: String = line.chars().skip(start).take(end - start).collect();
    assert_eq!(slice, "auth");
    assert_eq!(start, 2, "should match the single-quoted occurrence first");
}

#[test]
fn ignores_substring_matches() {
    // `auth` should NOT match `'authenticated'`. Without whole-token
    // quoting we'd return the wrong span and rename would corrupt
    // unrelated entries.
    let line = "    'authenticated' => Authenticate::class,";
    assert_eq!(find_alias_in_line(line, "auth"), None);
}

#[test]
fn ignores_unquoted_occurrences() {
    // The class name `Authenticate` shouldn't match alias `Authenticate`
    // because it isn't in quotes. (Real-world: rename never tries this
    // shape, but defensive against accidental wide matches.)
    let line = "    bind => Authenticate::class,";
    assert_eq!(find_alias_in_line(line, "Authenticate"), None);
}

#[test]
fn handles_binding_call_form() {
    // Typical service-provider register() body.
    let line = "        $this->app->singleton('config', function () {";
    let (start, end) = find_alias_in_line(line, "config").expect("should find binding");
    let slice: String = line.chars().skip(start).take(end - start).collect();
    assert_eq!(slice, "config");
}

#[test]
fn handles_kebab_dotted_aliases() {
    let line = "    'cache-store.redis' => CacheStoreRedis::class,";
    let (start, end) =
        find_alias_in_line(line, "cache-store.redis").expect("should find dotted alias");
    let slice: String = line.chars().skip(start).take(end - start).collect();
    assert_eq!(slice, "cache-store.redis");
}

#[test]
fn empty_alias_returns_none() {
    let line = "    'auth' => Authenticate::class,";
    assert_eq!(find_alias_in_line(line, ""), None);
}

// ---- locate_alias_on_line (file I/O) ------------------------------------

#[test]
fn locate_reads_correct_line() {
    use std::io::Write;
    let mut tmp = tempfile::NamedTempFile::new().expect("temp file");
    writeln!(tmp, "<?php").unwrap();
    writeln!(tmp, "return [").unwrap();
    writeln!(tmp, "    'auth' => Authenticate::class,").unwrap();
    writeln!(tmp, "    'verified' => Verified::class,").unwrap();
    writeln!(tmp, "];").unwrap();
    tmp.flush().unwrap();

    // source_line is 1-based: line 3 holds 'auth'.
    let span = locate_alias_on_line(tmp.path(), 3, "auth").expect("should locate auth");
    assert_eq!(span.line, 2, "0-based line index for line 3");
    // '    ' (4 spaces) then quote, then alias starts at col 5.
    assert_eq!(span.start_column, 5);
    assert_eq!(span.end_column, 9);
}

#[test]
fn locate_returns_none_for_missing_line() {
    use std::io::Write;
    let mut tmp = tempfile::NamedTempFile::new().expect("temp file");
    writeln!(tmp, "one line").unwrap();
    tmp.flush().unwrap();

    // 1-based line 50 doesn't exist.
    assert_eq!(locate_alias_on_line(tmp.path(), 50, "auth"), None);
}

#[test]
fn locate_returns_none_for_line_zero() {
    use std::io::Write;
    let mut tmp = tempfile::NamedTempFile::new().expect("temp file");
    writeln!(tmp, "x").unwrap();
    tmp.flush().unwrap();

    // 1-based: line 0 is invalid; checked_sub on 0u32 returns None.
    assert_eq!(locate_alias_on_line(tmp.path(), 0, "auth"), None);
}

#[test]
fn locate_returns_none_for_missing_file() {
    let path = std::path::PathBuf::from("/tmp/zed-laravel-test-does-not-exist-xyz123.php");
    assert_eq!(locate_alias_on_line(&path, 1, "auth"), None);
}

#[test]
fn locate_scans_forward_for_bulk_registration() {
    // Real bug from the field: Laravel 11's bootstrap/app.php uses
    // `$middleware->alias([ ... ])` and the parser stamps `source_line`
    // at the line of the `alias([` call, NOT at the line holding the
    // individual quoted alias. The locator must scan forward to find
    // the actual quoted alias.
    use std::io::Write;
    let mut tmp = tempfile::NamedTempFile::new().expect("temp file");
    writeln!(tmp, "<?php").unwrap();
    writeln!(tmp, "return Application::configure()").unwrap();
    writeln!(
        tmp,
        "    ->withMiddleware(function (Middleware $middleware) {{"
    )
    .unwrap();
    writeln!(tmp, "        $middleware->alias([").unwrap(); // line 4 — registration anchor
    writeln!(
        tmp,
        "            'account.active' => VerifyAccountActive::class,"
    )
    .unwrap(); // line 5 — actual alias
    writeln!(
        tmp,
        "            'account.type' => VerifyAccountType::class,"
    )
    .unwrap();
    writeln!(tmp, "        ]);").unwrap();
    tmp.flush().unwrap();

    // Pass source_line=4 (the registration call line); locator must
    // scan to line 5 to find 'account.active'.
    let span =
        locate_alias_on_line(tmp.path(), 4, "account.active").expect("should locate via scan");
    assert_eq!(span.line, 4, "0-based: found on line 5 (index 4)");
    // 12 spaces of indent, then quote, then alias starts at col 13.
    assert_eq!(span.start_column, 13);
    assert_eq!(span.end_column, 13 + "account.active".len() as u32);
}

#[test]
fn locate_finds_alias_within_search_window() {
    // Bulk arrays can be long — confirm the forward scan reaches an
    // alias buried several lines down.
    use std::io::Write;
    let mut tmp = tempfile::NamedTempFile::new().expect("temp file");
    writeln!(tmp, "$middleware->alias([").unwrap(); // line 1
    for i in 0..50 {
        writeln!(tmp, "    'filler{}' => Filler{}::class,", i, i).unwrap();
    }
    // Line 52: 'target' alias
    writeln!(tmp, "    'target' => Target::class,").unwrap();
    writeln!(tmp, "]);").unwrap();
    tmp.flush().unwrap();

    let span = locate_alias_on_line(tmp.path(), 1, "target").expect("should locate buried alias");
    // 0-based line 51 (line 52 in the file, 1-based).
    assert_eq!(span.line, 51);
}

#[test]
fn locate_does_not_search_past_window() {
    // Defensive: if `source_line` is wildly wrong and the alias only
    // appears much later, the scan should bottom out instead of
    // reading the whole file. Build a file where 'target' lives well
    // past SEARCH_LINE_WINDOW from source_line=1.
    use std::io::Write;
    let mut tmp = tempfile::NamedTempFile::new().expect("temp file");
    writeln!(tmp, "$middleware->alias([").unwrap(); // line 1
                                                    // Fill more than SEARCH_LINE_WINDOW lines of unrelated content.
    for i in 0..(SEARCH_LINE_WINDOW + 50) {
        writeln!(tmp, "    'filler{}' => Filler{}::class,", i, i).unwrap();
    }
    writeln!(tmp, "    'target' => Target::class,").unwrap();
    writeln!(tmp, "]);").unwrap();
    tmp.flush().unwrap();

    // 'target' is past the scan window — should NOT find it.
    assert_eq!(locate_alias_on_line(tmp.path(), 1, "target"), None);
}
