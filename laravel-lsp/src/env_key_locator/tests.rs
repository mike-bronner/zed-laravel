//! Tests for the env-key locator. Two layers: pure source-string
//! matching (`locate_in_source`), and the project-root walk over
//! every `.env*` file (`locate_keys_across_env_files`).

use super::*;
use tempfile::TempDir;

#[test]
fn locates_simple_key() {
    let src = "APP_NAME=Laravel\n";
    let pos = locate_in_source(src, "APP_NAME").unwrap();
    assert_eq!(pos.line, 0);
    assert_eq!(pos.start_column, 0);
    assert_eq!(pos.end_column, 8); // "APP_NAME".len()
}

#[test]
fn skips_comment_lines() {
    // A comment that mentions the key shape must NOT count as a
    // declaration — only real `KEY=value` lines do.
    let src = "# APP_NAME=this is a comment, not a decl\nAPP_NAME=Laravel\n";
    let pos = locate_in_source(src, "APP_NAME").unwrap();
    assert_eq!(pos.line, 1);
}

#[test]
fn skips_blank_lines() {
    let src = "\n\n\nAPP_NAME=Laravel\n";
    let pos = locate_in_source(src, "APP_NAME").unwrap();
    assert_eq!(pos.line, 3);
}

#[test]
fn handles_leading_whitespace_in_key() {
    // Some `.env` files indent (rare but legal in some parser
    // implementations). Column should reflect the actual key
    // position, not zero.
    let src = "  APP_NAME=Laravel\n";
    let pos = locate_in_source(src, "APP_NAME").unwrap();
    assert_eq!(pos.start_column, 2);
    assert_eq!(pos.end_column, 10);
}

#[test]
fn returns_none_when_key_missing() {
    let src = "OTHER_KEY=foo\nANOTHER=bar\n";
    assert!(locate_in_source(src, "APP_NAME").is_none());
}

#[test]
fn line_without_equals_is_not_a_declaration() {
    // A bare `APP_NAME` (no `=`) doesn't declare the variable. We
    // should skip it and continue looking for a real declaration.
    let src = "APP_NAME\nOTHER=foo\nAPP_NAME=Laravel\n";
    let pos = locate_in_source(src, "APP_NAME").unwrap();
    assert_eq!(pos.line, 2);
}

#[test]
fn exact_match_not_prefix() {
    // `APP_NAM` must NOT match `APP_NAME` — string equality on the
    // trimmed key, never substring matching.
    let src = "APP_NAME=Laravel\n";
    assert!(locate_in_source(src, "APP_NAM").is_none());
}

#[test]
fn exact_match_not_suffix() {
    // Similarly, `NAME` must NOT match `APP_NAME`.
    let src = "APP_NAME=Laravel\n";
    assert!(locate_in_source(src, "NAME").is_none());
}

#[test]
fn first_match_wins_on_duplicates() {
    // Real `.env` files don't duplicate keys, but if a malformed one
    // does, we should rewrite only the first occurrence so the
    // rename is deterministic.
    let src = "APP_NAME=first\nAPP_NAME=second\n";
    let pos = locate_in_source(src, "APP_NAME").unwrap();
    assert_eq!(pos.line, 0);
}

#[test]
fn value_can_contain_equals_sign() {
    // The `find('=')` returns the FIRST `=`, so a value like
    // `key=value=more` parses with key=`key` and value=`value=more`.
    // We only care about the key side here.
    let src = "DATABASE_URL=postgres://user:pass=word@host/db\n";
    let pos = locate_in_source(src, "DATABASE_URL").unwrap();
    assert_eq!(pos.line, 0);
    assert_eq!(pos.start_column, 0);
    assert_eq!(pos.end_column, 12); // "DATABASE_URL".len()
}

#[test]
fn locates_across_every_env_variant_laravel_supports() {
    // Every `.env*` variant a Laravel project might have. The matcher
    // is `.env` exact OR `.env.<anything>` prefix, so we exhaustively
    // verify each named variant lands in the result. New variants
    // (custom suffixes like `.env.qa`, `.env.docker`) are picked up
    // automatically by the prefix rule — no per-variant whitelist.
    let tmp = TempDir::new().unwrap();
    let variants = [
        ".env",
        ".env.local",
        ".env.testing",
        ".env.production",
        ".env.staging",
        ".env.example",
        ".env.qa", // a custom non-canonical variant — should still match
    ];
    for name in &variants {
        std::fs::write(tmp.path().join(name), "APP_NAME=Laravel\n").unwrap();
    }

    let locs = locate_keys_across_env_files(tmp.path(), "APP_NAME");
    assert_eq!(
        locs.len(),
        variants.len(),
        "should match every .env variant, got: {:?}",
        locs.iter()
            .map(|l| l
                .file_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned())
            .collect::<Vec<_>>()
    );

    // Sanity: confirm each named variant actually appears in the
    // results, not just that the count matches.
    let found_names: std::collections::HashSet<String> = locs
        .iter()
        .map(|l| {
            l.file_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    for v in &variants {
        assert!(
            found_names.contains(*v),
            "missing variant `{}` in results: {:?}",
            v,
            found_names
        );
    }
}

#[test]
fn skips_env_files_without_the_key() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join(".env"), "APP_NAME=Laravel\n").unwrap();
    std::fs::write(tmp.path().join(".env.example"), "OTHER=value\n").unwrap();

    let locs = locate_keys_across_env_files(tmp.path(), "APP_NAME");
    assert_eq!(locs.len(), 1);
    assert_eq!(
        locs[0].file_path.file_name().unwrap().to_string_lossy(),
        ".env"
    );
}

#[test]
fn ignores_files_that_dont_look_like_env() {
    // `.envrc` (direnv config), `env.txt`, `config.env` — none of
    // these are real Laravel env files and we shouldn't touch them.
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join(".env"), "APP_NAME=Laravel\n").unwrap();
    std::fs::write(tmp.path().join(".envrc"), "export APP_NAME=Laravel\n").unwrap();
    std::fs::write(tmp.path().join("env.txt"), "APP_NAME=Laravel\n").unwrap();
    std::fs::write(tmp.path().join("config.env"), "APP_NAME=Laravel\n").unwrap();

    let locs = locate_keys_across_env_files(tmp.path(), "APP_NAME");
    assert_eq!(locs.len(), 1, "only `.env` should match");
}

#[test]
fn empty_root_returns_empty() {
    let tmp = TempDir::new().unwrap();
    let locs = locate_keys_across_env_files(tmp.path(), "APP_NAME");
    assert!(locs.is_empty());
}

#[test]
fn nonexistent_root_returns_empty() {
    let locs = locate_keys_across_env_files(Path::new("/totally/made/up/path"), "APP_NAME");
    assert!(locs.is_empty());
}
