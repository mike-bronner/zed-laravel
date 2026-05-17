use laravel_lsp::class_locator::find_php_class_file;
use laravel_lsp::livewire_resolver::extract_blade_variable_at_cursor;
use laravel_lsp::php_class::{extract_class_properties, find_property_declaration_position};
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
    write(&dir.path().join("app/Models/Post.php"), "<?php class Post { }");

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

    let found =
        find_php_class_file("\\App\\Livewire\\Forms\\ContactForm", dir.path()).unwrap();
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
