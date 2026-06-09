//! The code-lens feature (#59) is opt-in while it matures: a single
//! `codeLens.enabled` master switch (default `false`) gates both the
//! reference-count lenses and the unused-symbol diagnostic. These tests lock in
//! that the default is OFF and that the setting parses from Zed's settings
//! object.

use crate::LspSettings;

#[test]
fn code_lens_defaults_off() {
    // No settings provided at all → feature off (opt-in).
    let settings = LspSettings::default();
    assert!(!settings.code_lens.enabled);
}

#[test]
fn empty_settings_object_leaves_code_lens_off() {
    // An explicit but empty settings object still means off.
    let settings: LspSettings = serde_json::from_value(serde_json::json!({})).unwrap();
    assert!(!settings.code_lens.enabled);

    // A present-but-empty codeLens object also stays off (enabled defaults false).
    let settings: LspSettings =
        serde_json::from_value(serde_json::json!({ "codeLens": {} })).unwrap();
    assert!(!settings.code_lens.enabled);
}

#[test]
fn code_lens_enabled_parses_from_camel_case() {
    let settings: LspSettings =
        serde_json::from_value(serde_json::json!({ "codeLens": { "enabled": true } })).unwrap();
    assert!(settings.code_lens.enabled);
}

#[test]
fn code_lens_opt_in_is_independent_of_other_settings() {
    // Turning the feature on doesn't disturb unrelated settings, and unrelated
    // settings don't turn the feature on.
    let settings: LspSettings = serde_json::from_value(serde_json::json!({
        "autoCompleteDebounce": 350,
        "codeLens": { "enabled": true },
    }))
    .unwrap();
    assert!(settings.code_lens.enabled);
    assert_eq!(settings.auto_complete_debounce, 350);
}
