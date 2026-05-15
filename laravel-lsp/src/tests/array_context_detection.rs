use crate::{ArrayContext, LaravelLanguageServer};

#[test]
fn test_detect_casts_property() {
    let current = "            'email_verified_at' => 'datetime',";
    let surrounding = vec![
        "    protected $casts = [",
    ];
    assert_eq!(
        LaravelLanguageServer::detect_array_context(current, &surrounding),
        ArrayContext::Casts
    );
}

#[test]
fn test_detect_casts_method() {
    let current = "            'email_verified_at' => 'datetime',";
    let surrounding = vec![
        "    protected function casts(): array",
        "    {",
        "        return [",
    ];
    assert_eq!(
        LaravelLanguageServer::detect_array_context(current, &surrounding),
        ArrayContext::Casts
    );
}

#[test]
fn test_detect_rules_property() {
    let current = "            'email' => 'required|email',";
    let surrounding = vec![
        "    protected $rules = [",
    ];
    assert_eq!(
        LaravelLanguageServer::detect_array_context(current, &surrounding),
        ArrayContext::Validation
    );
}

#[test]
fn test_detect_rules_method() {
    let current = "            'email' => 'required|email',";
    let surrounding = vec![
        "    public function rules(): array",
        "    {",
        "        return [",
    ];
    assert_eq!(
        LaravelLanguageServer::detect_array_context(current, &surrounding),
        ArrayContext::Validation
    );
}

#[test]
fn test_detect_validate_call() {
    let current = "            'email' => 'required|email',";
    let surrounding = vec![
        "        $request->validate([",
    ];
    assert_eq!(
        LaravelLanguageServer::detect_array_context(current, &surrounding),
        ArrayContext::Validation
    );
}

#[test]
fn test_detect_validator_make() {
    let current = "            'email' => 'required',";
    let surrounding = vec![
        "        $validator = Validator::make($data, [",
    ];
    assert_eq!(
        LaravelLanguageServer::detect_array_context(current, &surrounding),
        ArrayContext::Validation
    );
}

#[test]
fn test_detect_livewire_rule_attribute() {
    let current = "    #[Rule('required|email')]";
    let surrounding: Vec<&str> = vec![];
    assert_eq!(
        LaravelLanguageServer::detect_array_context(current, &surrounding),
        ArrayContext::Validation
    );
}

#[test]
fn test_detect_fillable() {
    let current = "        'name',";
    let surrounding = vec![
        "    protected $fillable = [",
    ];
    assert_eq!(
        LaravelLanguageServer::detect_array_context(current, &surrounding),
        ArrayContext::MassAssignment
    );
}

#[test]
fn test_detect_hidden() {
    let current = "        'password',";
    let surrounding = vec![
        "    protected $hidden = [",
    ];
    assert_eq!(
        LaravelLanguageServer::detect_array_context(current, &surrounding),
        ArrayContext::Visibility
    );
}

#[test]
fn test_detect_with_property() {
    let current = "        'posts',";
    let surrounding = vec![
        "    protected $with = [",
    ];
    assert_eq!(
        LaravelLanguageServer::detect_array_context(current, &surrounding),
        ArrayContext::Relationships
    );
}

#[test]
fn test_unknown_context() {
    let current = "        'some_value',";
    let surrounding = vec![
        "        $randomArray = [",
    ];
    assert_eq!(
        LaravelLanguageServer::detect_array_context(current, &surrounding),
        ArrayContext::Unknown
    );
}

#[test]
fn test_validation_priority_over_generic() {
    // When on a line that has validation-like content but surrounding is ambiguous,
    // validation patterns should be detected
    let current = "        $request->validate([";
    let surrounding: Vec<&str> = vec![];
    assert_eq!(
        LaravelLanguageServer::detect_array_context(current, &surrounding),
        ArrayContext::Validation
    );
}

#[test]
fn test_casts_blocks_validation_completion() {
    // Even if line contains validation-like words (string, integer, boolean),
    // being in a casts context should return Casts, not Validation
    let current = "            'is_active' => 'boolean',";
    let surrounding = vec![
        "    protected function casts(): array",
        "    {",
        "        return [",
    ];
    // "boolean" is both a cast type and a validation rule,
    // but context should win
    assert_eq!(
        LaravelLanguageServer::detect_array_context(current, &surrounding),
        ArrayContext::Casts
    );
}

#[test]
fn test_casts_with_realistic_line_order() {
    // Test with lines in the order they'd actually be collected
    // (closest line first, furthest last)
    let current = "            'email_verified_at' => 'datetime',";
    // Simulating User model casts() method - lines closest to current first
    let surrounding = vec![
        "        return [",                         // line N-1
        "    {",                                    // line N-2
        "    protected function casts(): array",   // line N-3
        "     */",                                  // line N-4 (docblock end)
        "     * @return array<string, string>",    // line N-5
    ];
    assert_eq!(
        LaravelLanguageServer::detect_array_context(current, &surrounding),
        ArrayContext::Casts
    );
}

#[test]
fn test_casts_with_docblock_beyond_10_lines() {
    // If casts() is more than 10 lines away due to large docblock,
    // context should be Unknown (not incorrectly Validation)
    let current = "            'field' => 'str";
    // No casts() or rules() in surrounding 10 lines
    let surrounding = vec![
        "        return [",
        "    {",
        "     */",
        "     * Line 1 of long docblock",
        "     * Line 2 of long docblock",
        "     * Line 3 of long docblock",
        "     * Line 4 of long docblock",
        "     * Line 5 of long docblock",
        "     * Line 6 of long docblock",
        "    /**",
    ];
    // Should be Unknown, NOT Validation
    assert_eq!(
        LaravelLanguageServer::detect_array_context(current, &surrounding),
        ArrayContext::Unknown
    );
}
