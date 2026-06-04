//! Severity follows runtime behavior: a diagnostic is an ERROR only when the
//! bad value makes Laravel *throw* (a 500). Helpers that degrade silently —
//! returning a key, null, or false — are WARNINGs. These tests lock in the
//! cases the severity audit corrected.

use crate::LaravelLanguageServer;
use tower_lsp::lsp_types::DiagnosticSeverity;

#[test]
fn missing_dotted_translation_is_a_warning_not_error() {
    // Translator::get returns the requested key verbatim on a miss — no throw,
    // the page still renders — so a missing translation is a WARNING. It is
    // also the SAME severity for `__()`, `trans()`, and `@lang`, which once
    // disagreed (ERROR for `__()`, WARNING for `@lang`).
    let check = crate::TranslationCheck {
        exists: false,
        is_dotted_key: true,
        expected_path: Some(std::path::PathBuf::from("/p/lang/en/messages.php")),
        file_exists: true,
        nested_key: Some("welcome".to_string()),
    };
    let d =
        LaravelLanguageServer::create_translation_diagnostic("messages.welcome", &check, 0, 0, 5);
    assert_eq!(d.severity, Some(DiagnosticSeverity::WARNING));
}

#[test]
fn missing_non_dotted_translation_stays_information() {
    // A bare string like `__('Welcome')` is usually a literal default, not a
    // key reference — the softest visible level (INFORMATION), left unchanged.
    let check = crate::TranslationCheck {
        exists: false,
        is_dotted_key: false,
        expected_path: None,
        file_exists: false,
        nested_key: None,
    };
    let d = LaravelLanguageServer::create_translation_diagnostic("Welcome", &check, 0, 0, 5);
    assert_eq!(d.severity, Some(DiagnosticSeverity::INFORMATION));
}
