use laravel_lsp::class_locator::find_php_class_file;
use laravel_lsp::livewire_resolver::extract_blade_variable_at_cursor;
use laravel_lsp::php_class::{
    extract_class_fqn, extract_class_properties, extract_class_signature,
    extract_property_declaration, find_property_declaration_position,
    find_property_definition_line, read_line_from_file,
};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn write(path: &PathBuf, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

// ============================================================================
// class_locator::find_php_class_file
// ============================================================================

#[test]
fn locates_class_in_app_models() {
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("app/Models/Post.php");
    write(&target, "<?php class Post extends Model { }");

    let found = find_php_class_file("Post", dir.path()).unwrap();
    assert_eq!(found, target);
}

#[test]
fn locates_class_in_app_livewire_forms() {
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("app/Livewire/Forms/ContactForm.php");
    write(
        &target,
        "<?php class ContactForm extends Form { public string $name = ''; }",
    );

    let found = find_php_class_file("ContactForm", dir.path()).unwrap();
    assert_eq!(found, target);
}

#[test]
fn locates_class_in_arbitrary_app_subdirectory() {
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("app/Services/Payments/StripeGateway.php");
    write(&target, "<?php class StripeGateway { }");

    let found = find_php_class_file("StripeGateway", dir.path()).unwrap();
    assert_eq!(found, target);
}

#[test]
fn locates_class_in_src_when_app_missing() {
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("src/Domain/User.php");
    write(&target, "<?php class User { }");

    let found = find_php_class_file("User", dir.path()).unwrap();
    assert_eq!(found, target);
}

#[test]
fn returns_none_for_missing_class() {
    let dir = TempDir::new().unwrap();
    write(
        &dir.path().join("app/Models/Post.php"),
        "<?php class Post { }",
    );

    assert_eq!(find_php_class_file("MissingClass", dir.path()), None);
}

#[test]
fn skips_vendor_directory() {
    let dir = TempDir::new().unwrap();
    // A vendor file with the target class name — should NOT be returned.
    write(
        &dir.path().join("app/vendor/some-pkg/src/User.php"),
        "<?php class User { }",
    );
    // Real app file — should be returned even though vendor has a same-named one.
    let target = dir.path().join("app/Models/User.php");
    write(&target, "<?php class User { }");

    let found = find_php_class_file("User", dir.path()).unwrap();
    assert_eq!(found, target);
}

#[test]
fn simplifies_fully_qualified_class_name() {
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("app/Livewire/Forms/ContactForm.php");
    write(&target, "<?php class ContactForm { }");

    let found = find_php_class_file("\\App\\Livewire\\Forms\\ContactForm", dir.path()).unwrap();
    assert_eq!(found, target);
}

#[test]
fn returns_none_for_empty_class_name() {
    let dir = TempDir::new().unwrap();
    assert_eq!(find_php_class_file("", dir.path()), None);
}

// ============================================================================
// php_class::extract_class_properties
// ============================================================================

#[test]
fn extracts_typed_and_untyped_properties() {
    let src = r#"<?php
        class ContactForm {
            public string $name = '';
            public string $email = '';
            public ?int $age = null;
            public $payload;
            private string $secret = 'hidden';
        }
    "#;
    let props = extract_class_properties(src);
    assert_eq!(props.len(), 4);
    assert!(props.contains(&("name".to_string(), "string".to_string())));
    assert!(props.contains(&("email".to_string(), "string".to_string())));
    assert!(props.contains(&("age".to_string(), "?int".to_string())));
    assert!(props.contains(&("payload".to_string(), "mixed".to_string())));
    // `private` excluded
    assert!(!props.iter().any(|(n, _)| n == "secret"));
}

#[test]
fn extracts_simplifies_fully_qualified_types() {
    let src = r#"<?php
        class Holder {
            public \App\Models\User $owner;
        }
    "#;
    let props = extract_class_properties(src);
    assert_eq!(props, vec![("owner".to_string(), "User".to_string())]);
}

#[test]
fn extracts_dedupes_by_name() {
    // Should never happen in real PHP but the helper shouldn't crash; first wins.
    let src = r#"<?php
        class Twin {
            public string $foo = '';
            public int $foo = 0;
        }
    "#;
    let props = extract_class_properties(src);
    assert_eq!(props.len(), 1);
    assert_eq!(props[0], ("foo".to_string(), "string".to_string()));
}

#[test]
fn extracts_works_on_inline_livewire_class() {
    let src = r#"<?php
        new class extends Component {
            public ContactForm $form;
            public bool $isSubmitted = false;
        };
    "#;
    let props = extract_class_properties(src);
    assert_eq!(props.len(), 2);
    assert!(props.contains(&("form".to_string(), "ContactForm".to_string())));
    assert!(props.contains(&("isSubmitted".to_string(), "bool".to_string())));
}

// ============================================================================
// php_class::find_property_declaration_position
// ============================================================================

#[test]
fn finds_position_of_typed_property() {
    let src = "<?php\nclass Form {\n    public string $name = '';\n}\n";
    let pos = find_property_declaration_position(src, "name").unwrap();
    // Line 2 (0-indexed), `$name` starts at column 19 (after `    public string `)
    assert_eq!(pos.0, 2);
    // Sanity: end - start should equal len("$name")
    assert_eq!(pos.2 - pos.1, "$name".len() as u32);
}

#[test]
fn finds_position_of_untyped_property() {
    let src = "<?php\nclass C {\n    public $foo;\n}";
    let pos = find_property_declaration_position(src, "foo").unwrap();
    assert_eq!(pos.0, 2);
    assert_eq!(pos.2 - pos.1, "$foo".len() as u32);
}

#[test]
fn finds_position_returns_none_for_missing_property() {
    let src = "<?php class C { public string $a = ''; }";
    assert!(find_property_declaration_position(src, "missing").is_none());
}

#[test]
fn finds_position_ignores_non_public() {
    let src = "<?php class C { private string $secret = ''; protected int $count = 0; }";
    assert!(find_property_declaration_position(src, "secret").is_none());
    assert!(find_property_declaration_position(src, "count").is_none());
}

// ============================================================================
// livewire_resolver::extract_blade_variable_at_cursor
// ============================================================================

#[test]
fn cursor_in_middle_of_variable_returns_variable() {
    //          0         1
    //          0123456789012345
    let line = "<p>{{ $form }}</p>";
    let result = extract_blade_variable_at_cursor(line, 9); // mid-"form"
    assert_eq!(result, Some(("form".to_string(), None)));
}

#[test]
fn cursor_at_start_of_variable_returns_variable() {
    let line = "<p>{{ $form }}</p>";
    let result = extract_blade_variable_at_cursor(line, 7); // on 'f' of "form"
    assert_eq!(result, Some(("form".to_string(), None)));
}

#[test]
fn cursor_on_property_returns_var_and_prop() {
    //          0         1         2
    //          0123456789012345678901
    let line = "<p>{{ $form->name }}</p>";
    let result = extract_blade_variable_at_cursor(line, 14); // mid-"name"
    assert_eq!(result, Some(("form".to_string(), Some("name".to_string()))));
}

#[test]
fn cursor_after_bare_dollar_returns_empty_variable() {
    let line = "<p>{{ $ }}</p>";
    let result = extract_blade_variable_at_cursor(line, 7); // right after `$`
    assert_eq!(result, Some((String::new(), None)));
}

#[test]
fn cursor_on_unrelated_word_returns_none() {
    let line = "<p>Hello world</p>";
    let result = extract_blade_variable_at_cursor(line, 5); // on "Hello"
    assert_eq!(result, None);
}

#[test]
fn cursor_beyond_line_length_returns_none() {
    let line = "<p>x</p>";
    let result = extract_blade_variable_at_cursor(line, 100);
    assert_eq!(result, None);
}

// ============================================================================
// End-to-end: hover-style property type lookup using all the new pieces
// ============================================================================

#[test]
fn e2e_resolves_property_type_via_class_locator() {
    let dir = TempDir::new().unwrap();
    write(
        &dir.path().join("app/Livewire/Forms/ContactForm.php"),
        r#"<?php

namespace App\Livewire\Forms;

use Livewire\Form;

class ContactForm extends Form {
    public string $name = '';
    public string $email = '';
    public ?int $age = null;
}
"#,
    );

    // Find the class file
    let class_path: &Path = &find_php_class_file("ContactForm", dir.path()).unwrap();
    assert!(class_path.ends_with("app/Livewire/Forms/ContactForm.php"));

    // Extract all its properties
    let content = fs::read_to_string(class_path).unwrap();
    let props = extract_class_properties(&content);
    assert!(props.contains(&("name".to_string(), "string".to_string())));
    assert!(props.contains(&("email".to_string(), "string".to_string())));
    assert!(props.contains(&("age".to_string(), "?int".to_string())));

    // Find the position of the `name` property declaration
    let pos = find_property_declaration_position(&content, "name").unwrap();
    let lines: Vec<&str> = content.lines().collect();
    let line = lines[pos.0 as usize];
    let col_range = &line[pos.1 as usize..pos.2 as usize];
    assert_eq!(col_range, "$name");
}

// ============================================================================
// php_class::find_property_definition_line
// ============================================================================

#[test]
fn finds_public_property_definition_line() {
    let src = "<?php\nclass User {\n    public string $name;\n    public int $age;\n}\n";
    // `name` is on line 2 (0-based: line 2 because <?php is line 0, blank is 1, no — let me count)
    // Line 0: `<?php`
    // Line 1: `class User {`
    // Line 2: `    public string $name;`
    let line = find_property_definition_line(src, "name").expect("should find name");
    assert_eq!(line, 2);
}

#[test]
fn finds_phpdoc_property_definition_line() {
    let src = r#"<?php
/**
 * @property string $email
 * @property int $age
 */
class User {}
"#;
    let line = find_property_definition_line(src, "email").expect("should find email");
    // Line 0: <?php
    // Line 1: /**
    // Line 2:  * @property string $email
    assert_eq!(line, 2);
}

#[test]
fn finds_property_in_casts_array() {
    let src = r#"<?php
class User extends Model {
    protected $casts = [
        'email_verified_at' => 'datetime',
        'is_admin' => 'boolean',
    ];
}
"#;
    let line = find_property_definition_line(src, "is_admin").expect("should find is_admin");
    // Line 4: `        'is_admin' => 'boolean',`
    assert_eq!(line, 4);
}

#[test]
fn finds_relationship_method() {
    let src = r#"<?php
class User extends Model {
    public function posts()
    {
        return $this->hasMany(Post::class);
    }
}
"#;
    let line = find_property_definition_line(src, "posts").expect("should find posts");
    // Line 2: `    public function posts()`
    assert_eq!(line, 2);
}

#[test]
fn picks_earliest_match_when_multiple_shapes_present() {
    // A model with the same column in BOTH @property AND $casts. The earlier
    // occurrence (@property doc block) wins.
    let src = r#"<?php
/** @property string $email */
class User extends Model {
    protected $casts = [
        'email' => 'string',
    ];
}
"#;
    let line = find_property_definition_line(src, "email").expect("should find email");
    // The @property line (line 1) comes before the $casts entry (line 4).
    assert_eq!(line, 1);
}

#[test]
fn returns_none_when_property_not_declared_anywhere() {
    let src = "<?php\nclass User { public $name; }\n";
    assert_eq!(find_property_definition_line(src, "missing"), None);
}

#[test]
fn does_not_falsely_match_substring_property_names() {
    // `name_with_extra` should not match a search for `name`.
    let src = "<?php\nclass User { public $name_with_extra; }\n";
    assert_eq!(find_property_definition_line(src, "name"), None);
}

// ============================================================================
// php_class::extract_property_declaration
// ============================================================================

#[test]
fn extracts_declaration_text_for_typed_property() {
    let src = "<?php\nclass User {\n    public string $email;\n}\n";
    let decl = extract_property_declaration(src, "email").expect("should find");
    assert_eq!(decl.declaration_text, "public string $email;");
    assert_eq!(decl.line, 2);
    assert!(decl.description.is_none());
    assert!(decl.phpdoc_tags.is_empty());
}

#[test]
fn extracts_phpdoc_description_above_property() {
    let src = r#"<?php
class User {
    /**
     * The user's email address.
     */
    public string $email;
}
"#;
    let decl = extract_property_declaration(src, "email").expect("should find");
    assert_eq!(
        decl.description.as_deref(),
        Some("The user's email address.")
    );
}

#[test]
fn extracts_phpdoc_tags() {
    let src = r#"<?php
class Controller {
    /**
     * Authorize a given action for the current user.
     *
     * @param mixed $ability
     * @param mixed $arguments
     * @return \Illuminate\Auth\Access\Response
     * @throws \Illuminate\Auth\Access\AuthorizationException
     */
    public function authorize($ability, $arguments = []) {}
}
"#;
    let decl = extract_property_declaration(src, "authorize").expect("should find");
    assert_eq!(
        decl.description.as_deref(),
        Some("Authorize a given action for the current user.")
    );
    assert!(
        decl.phpdoc_tags
            .iter()
            .any(|t| t == "@param mixed $ability"),
        "got tags: {:?}",
        decl.phpdoc_tags
    );
    assert!(decl
        .phpdoc_tags
        .iter()
        .any(|t| t == "@return \\Illuminate\\Auth\\Access\\Response"));
    assert!(decl
        .phpdoc_tags
        .iter()
        .any(|t| t == "@throws \\Illuminate\\Auth\\Access\\AuthorizationException"));
}

#[test]
fn description_joins_multiline_summary() {
    let src = r#"<?php
class User {
    /**
     * The user's email address.
     * Used for password recovery and notifications.
     */
    public string $email;
}
"#;
    let decl = extract_property_declaration(src, "email").expect("should find");
    let desc = decl.description.unwrap();
    assert!(desc.contains("The user's email address."));
    assert!(desc.contains("Used for password recovery"));
}

#[test]
fn no_phpdoc_above_returns_none_description() {
    let src = "<?php\nclass User {\n    public string $email;\n}\n";
    let decl = extract_property_declaration(src, "email").expect("should find");
    assert!(decl.description.is_none());
    assert!(decl.phpdoc_tags.is_empty());
}

#[test]
fn ignores_phpdoc_separated_by_other_code() {
    // A PHPDoc block with code BETWEEN it and the property — should not
    // associate the docblock with the property.
    let src = r#"<?php
class User {
    /**
     * This describes something else.
     */
    public string $other;

    public string $email;
}
"#;
    let decl = extract_property_declaration(src, "email").expect("should find");
    assert!(
        decl.description.is_none(),
        "got unexpected description: {:?}",
        decl.description
    );
}

// ============================================================================
// php_class::extract_class_signature
// ============================================================================

#[test]
fn extract_class_signature_returns_class_line() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("Counter.php");
    fs::write(
        &path,
        "<?php\nnamespace App\\Livewire;\n\nuse Livewire\\Component;\n\nclass Counter extends Component\n{\n    public int $count = 0;\n}\n",
    )
    .unwrap();
    let got = extract_class_signature(&path).expect("should find class");
    assert_eq!(got, "class Counter extends Component");
}

#[test]
fn extract_class_signature_handles_final_and_abstract_modifiers() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("Foo.php");
    fs::write(
        &path,
        "<?php\nfinal class Foo extends Bar implements Baz\n{\n}\n",
    )
    .unwrap();
    let got = extract_class_signature(&path).expect("should find class");
    assert_eq!(got, "final class Foo extends Bar implements Baz");
}

#[test]
fn extract_class_signature_ignores_class_keyword_in_strings() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("Foo.php");
    // The string `'class FakeClass'` must NOT trick the matcher — our
    // regex anchors at start-of-line. Body braces (`{}`) are intentionally
    // excluded from the captured signature — same shape intelephense uses.
    fs::write(
        &path,
        "<?php\n$x = 'class FakeClass';\nclass Real extends Bar {}\n",
    )
    .unwrap();
    let got = extract_class_signature(&path).expect("should find class");
    assert_eq!(got, "class Real extends Bar");
}

#[test]
fn extract_class_signature_returns_none_for_files_without_class() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("helpers.php");
    fs::write(&path, "<?php\nfunction helper() {}\n").unwrap();
    assert_eq!(extract_class_signature(&path), None);
}

// ============================================================================
// php_class::read_line_from_file
// ============================================================================

#[test]
fn read_line_from_file_returns_targeted_line() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("routes.php");
    fs::write(
        &path,
        "<?php\nRoute::get('/users', [UserController::class, 'index'])->name('users.index');\nRoute::post('/users', [UserController::class, 'store'])->name('users.store');\n",
    )
    .unwrap();
    assert_eq!(read_line_from_file(&path, 0).as_deref(), Some("<?php"));
    assert!(read_line_from_file(&path, 1)
        .unwrap()
        .contains("Route::get"));
    assert!(read_line_from_file(&path, 2)
        .unwrap()
        .contains("Route::post"));
}

#[test]
fn read_line_from_file_preserves_leading_whitespace() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("x.php");
    fs::write(&path, "first\n    indented line\n").unwrap();
    assert_eq!(
        read_line_from_file(&path, 1).as_deref(),
        Some("    indented line"),
        "leading whitespace should survive — hover snippets care about indentation context"
    );
}

#[test]
fn read_line_from_file_returns_none_for_out_of_range_line() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("short.php");
    fs::write(&path, "one\ntwo\n").unwrap();
    assert_eq!(read_line_from_file(&path, 99), None);
}

#[test]
fn read_line_from_file_returns_none_for_missing_file() {
    let nonexistent = std::path::PathBuf::from("/nonexistent/file.php");
    assert_eq!(read_line_from_file(&nonexistent, 0), None);
}

// ============================================================================
// php_class::extract_class_fqn
// ============================================================================

#[test]
fn extract_class_fqn_combines_namespace_and_class() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("Counter.php");
    fs::write(
        &path,
        "<?php\nnamespace App\\Livewire;\n\nuse Livewire\\Component;\n\nclass Counter extends Component\n{\n}\n",
    )
    .unwrap();
    assert_eq!(
        extract_class_fqn(&path).as_deref(),
        Some("App\\Livewire\\Counter")
    );
}

#[test]
fn extract_class_fqn_handles_namespaceless_class() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("Plain.php");
    fs::write(&path, "<?php\nclass Plain\n{\n}\n").unwrap();
    assert_eq!(extract_class_fqn(&path).as_deref(), Some("Plain"));
}

#[test]
fn extract_class_fqn_handles_modifiers_and_interfaces() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("Repo.php");
    fs::write(
        &path,
        "<?php\nnamespace App\\Services;\n\nfinal class UserRepo implements Repo\n{\n}\n",
    )
    .unwrap();
    assert_eq!(
        extract_class_fqn(&path).as_deref(),
        Some("App\\Services\\UserRepo")
    );
}

#[test]
fn extract_class_fqn_works_for_interfaces_and_traits() {
    let dir = TempDir::new().unwrap();
    let interface_path = dir.path().join("Lookup.php");
    fs::write(
        &interface_path,
        "<?php\nnamespace App\\Contracts;\n\ninterface Lookup {}\n",
    )
    .unwrap();
    assert_eq!(
        extract_class_fqn(&interface_path).as_deref(),
        Some("App\\Contracts\\Lookup")
    );

    let trait_path = dir.path().join("Findable.php");
    fs::write(
        &trait_path,
        "<?php\nnamespace App\\Concerns;\n\ntrait Findable {}\n",
    )
    .unwrap();
    assert_eq!(
        extract_class_fqn(&trait_path).as_deref(),
        Some("App\\Concerns\\Findable")
    );
}

#[test]
fn extract_class_fqn_returns_none_for_no_class() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("helpers.php");
    fs::write(&path, "<?php\nfunction helper() {}\n").unwrap();
    assert_eq!(extract_class_fqn(&path), None);
}

#[test]
fn extract_class_fqn_returns_none_for_missing_file() {
    let nonexistent = std::path::PathBuf::from("/nonexistent/Class.php");
    assert_eq!(extract_class_fqn(&nonexistent), None);
}
