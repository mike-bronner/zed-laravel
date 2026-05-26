use super::*;

#[test]
fn detects_v4_with_v_prefix() {
    let lock = r#"{
        "packages": [
            { "name": "livewire/livewire", "version": "v4.0.3" }
        ]
    }"#;
    assert_eq!(detect_from_composer_lock(lock), LivewireVersion::V4);
}

#[test]
fn detects_v3_with_v_prefix() {
    let lock = r#"{
        "packages": [
            { "name": "livewire/livewire", "version": "v3.5.18" }
        ]
    }"#;
    assert_eq!(detect_from_composer_lock(lock), LivewireVersion::V3);
}

#[test]
fn detects_v4_without_v_prefix() {
    let lock = r#"{
        "packages": [
            { "name": "livewire/livewire", "version": "4.0.3" }
        ]
    }"#;
    assert_eq!(detect_from_composer_lock(lock), LivewireVersion::V4);
}

#[test]
fn detects_compact_json_spacing() {
    let lock = r#"{"packages":[{"name":"livewire/livewire","version":"v4.0.0"}]}"#;
    assert_eq!(detect_from_composer_lock(lock), LivewireVersion::V4);
}

#[test]
fn unknown_when_package_missing() {
    let lock = r#"{
        "packages": [
            { "name": "laravel/framework", "version": "v12.0.0" }
        ]
    }"#;
    assert_eq!(detect_from_composer_lock(lock), LivewireVersion::Unknown);
}

#[test]
fn unknown_for_malformed_version() {
    let lock = r#"{
        "packages": [
            { "name": "livewire/livewire", "version": "dev-main" }
        ]
    }"#;
    assert_eq!(detect_from_composer_lock(lock), LivewireVersion::Unknown);
}

#[test]
fn picks_livewire_version_not_neighboring_package() {
    // Defensive: when livewire is sandwiched between other packages, the
    // resolver must read the version field belonging to livewire's object,
    // not one of the neighbors. The 500-byte lookahead window keeps us
    // inside the same JSON object as long as `name` and `version` are close
    // — which is the composer.lock convention.
    let lock = r#"{
        "packages": [
            { "name": "laravel/framework", "version": "v12.5.0" },
            { "name": "livewire/livewire", "version": "v4.0.3" },
            { "name": "laravel/prompts", "version": "v0.3.0" }
        ]
    }"#;
    assert_eq!(detect_from_composer_lock(lock), LivewireVersion::V4);
}

#[test]
fn unknown_for_v2_or_unrecognized_major() {
    let lock = r#"{
        "packages": [
            { "name": "livewire/livewire", "version": "v2.12.6" }
        ]
    }"#;
    assert_eq!(detect_from_composer_lock(lock), LivewireVersion::Unknown);
}

#[test]
fn unknown_for_empty_input() {
    assert_eq!(detect_from_composer_lock(""), LivewireVersion::Unknown);
}
