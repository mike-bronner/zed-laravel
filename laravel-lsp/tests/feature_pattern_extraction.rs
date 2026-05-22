//! Integration tests for Laravel Pennant feature-flag extraction.
//!
//! Covers `Feature::active/inactive/value/...` call sites plus the
//! `$name` property pattern used by class-based feature definitions.
//! Relocated from the inline `mod tests` block in `src/queries.rs` so
//! business logic and test logic don't share a file.

use laravel_lsp::parser::{language_php, parse_php};
use laravel_lsp::queries::extract_all_php_patterns;

#[test]
fn extract_feature_patterns() {
    let php_code = r#"<?php
    Feature::active('new-api');
    Feature::inactive('beta-mode');
    Feature::for($user)->active('purchase-button');
    Feature::value('experiment');
    Feature::allAreActive(['feature-a', 'feature-b']);
    Feature::active(NewApi::class);
    "#;

    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns = extract_all_php_patterns(&tree, php_code, &lang)
        .expect("Should extract patterns");

    // Check that we found feature calls
    assert!(!patterns.feature_calls.is_empty(), "Should find feature calls");

    // Get all feature names
    let feature_names: Vec<&str> = patterns
        .feature_calls
        .iter()
        .map(|f| f.feature_name)
        .collect();

    // Check for specific features
    assert!(
        feature_names.contains(&"new-api"),
        "Should find 'new-api' feature"
    );
    assert!(
        feature_names.contains(&"beta-mode"),
        "Should find 'beta-mode' feature"
    );

    // Check method names
    let new_api = patterns
        .feature_calls
        .iter()
        .find(|f| f.feature_name == "new-api");
    assert!(new_api.is_some(), "Should find new-api feature");
    assert_eq!(
        new_api.unwrap().method_name,
        "active",
        "Method should be 'active'"
    );

    // Check class-based feature
    let class_feature = patterns
        .feature_calls
        .iter()
        .find(|f| f.feature_name == "NewApi");
    if let Some(feature) = class_feature {
        assert!(feature.is_class_reference, "Should be class reference");
    }
}

#[test]
fn feature_name_property_extraction() {
    // Test typed property with single quotes
    let php_code = r#"<?php

namespace App\Features;

class NewApi
{
    public string $name = 'custom-feature-alias';

    public function resolve(mixed $scope): mixed
    {
        return false;
    }
}
"#;

    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns = extract_all_php_patterns(&tree, php_code, &lang)
        .expect("Should extract patterns");

    assert_eq!(
        patterns.feature_name_properties.len(),
        1,
        "Should find one $name property"
    );
    assert_eq!(
        patterns.feature_name_properties[0].name_value,
        "custom-feature-alias"
    );
}

#[test]
fn feature_name_property_untyped() {
    // Test untyped property with double quotes
    let php_code = r#"<?php

namespace App\Features;

class BetaMode
{
    public $name = "beta-mode-feature";

    public function resolve(mixed $scope): mixed
    {
        return true;
    }
}
"#;

    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns = extract_all_php_patterns(&tree, php_code, &lang)
        .expect("Should extract patterns");

    assert_eq!(
        patterns.feature_name_properties.len(),
        1,
        "Should find one $name property"
    );
    assert_eq!(
        patterns.feature_name_properties[0].name_value,
        "beta-mode-feature"
    );
}

#[test]
fn feature_name_property_not_captured_for_other_names() {
    // Ensure we only capture $name, not other properties
    let php_code = r#"<?php

class SomeClass
{
    public string $description = 'some description';
    public string $title = 'some title';
    protected $value = 'some value';
}
"#;

    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns = extract_all_php_patterns(&tree, php_code, &lang)
        .expect("Should extract patterns");

    assert!(
        patterns.feature_name_properties.is_empty(),
        "Should not capture other properties"
    );
}
