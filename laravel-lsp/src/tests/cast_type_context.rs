use crate::LaravelLanguageServer;

#[test]
fn test_extract_partial_cast_type_basic() {
    // After typing => ' we should get empty string
    let before = "        'email_verified_at' => '";
    assert_eq!(
        LaravelLanguageServer::extract_partial_cast_type(before),
        Some("".to_string())
    );
}

#[test]
fn test_extract_partial_cast_type_with_prefix() {
    // After typing => 'date we should get "date"
    let before = "        'email_verified_at' => 'date";
    assert_eq!(
        LaravelLanguageServer::extract_partial_cast_type(before),
        Some("date".to_string())
    );
}

#[test]
fn test_extract_partial_cast_type_double_quotes() {
    let before = "        'field' => \"int";
    assert_eq!(
        LaravelLanguageServer::extract_partial_cast_type(before),
        Some("int".to_string())
    );
}

#[test]
fn test_extract_partial_cast_type_no_arrow() {
    // Key position, not value - should return None
    let before = "        'field";
    assert_eq!(
        LaravelLanguageServer::extract_partial_cast_type(before),
        None
    );
}

#[test]
fn test_extract_partial_cast_type_closed_string() {
    // Already closed string - cursor is outside
    let before = "        'field' => 'datetime',";
    assert_eq!(
        LaravelLanguageServer::extract_partial_cast_type(before),
        None
    );
}

#[test]
fn test_get_cast_type_context_in_casts_array() {
    let line = "        'email_verified_at' => 'date";
    let surrounding = vec!["    protected $casts = ["];
    // Character position should be at end of line (after 'date')
    assert_eq!(
        LaravelLanguageServer::get_cast_type_context(line, line.len() as u32, &surrounding),
        Some("date".to_string())
    );
}

#[test]
fn test_get_cast_type_context_in_casts_method() {
    let line = "        'is_admin' => 'bool";
    let surrounding = vec![
        "    protected function casts(): array",
        "    {",
        "        return [",
    ];
    assert_eq!(
        LaravelLanguageServer::get_cast_type_context(line, line.len() as u32, &surrounding),
        Some("bool".to_string())
    );
}

#[test]
fn test_get_cast_type_context_not_in_casts() {
    // In validation context, not casts
    let line = "        'email' => 'req";
    let surrounding = vec!["    protected $rules = ["];
    assert_eq!(
        LaravelLanguageServer::get_cast_type_context(line, line.len() as u32, &surrounding),
        None
    );
}

#[test]
fn test_get_cast_type_context_empty_prefix() {
    let line = "        'field' => '";
    let surrounding = vec!["    protected $casts = ["];
    assert_eq!(
        LaravelLanguageServer::get_cast_type_context(line, line.len() as u32, &surrounding),
        Some("".to_string())
    );
}
