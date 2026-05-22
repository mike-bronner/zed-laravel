use super::*;

#[test]
fn test_camel_to_snake() {
    assert_eq!(LaravelRulesParser::camel_to_snake("Required"), "required");
    assert_eq!(
        LaravelRulesParser::camel_to_snake("AfterOrEqual"),
        "after_or_equal"
    );
    assert_eq!(
        LaravelRulesParser::camel_to_snake("RequiredIf"),
        "required_if"
    );
    assert_eq!(LaravelRulesParser::camel_to_snake("Gt"), "gt");
}

#[test]
fn test_default_dimension_options() {
    let options = LaravelRulesParser::default_dimension_options();
    assert!(options.contains(&"min_width".to_string()));
    assert!(options.contains(&"max_height".to_string()));
    assert!(options.contains(&"ratio".to_string()));
}

#[test]
fn test_default_mime_extensions() {
    let extensions = LaravelRulesParser::default_mime_extensions();
    assert!(extensions.contains(&"jpg".to_string()));
    assert!(extensions.contains(&"pdf".to_string()));
    assert!(extensions.contains(&"png".to_string()));
}
