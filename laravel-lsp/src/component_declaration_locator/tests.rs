use super::*;
use crate::salsa_impl::LaravelConfigData;
use std::collections::HashMap;
use std::fs;
use tempfile::TempDir;

fn config_for(root: &Path) -> LaravelConfigData {
    LaravelConfigData {
        root: root.to_path_buf(),
        view_paths: vec![root.join("resources/views")],
        component_paths: vec![(String::new(), root.join("resources/views/components"))],
        livewire_path: None,
        has_livewire: false,
        view_namespaces: HashMap::new(),
        component_namespaces: HashMap::new(),
        anonymous_component_paths: HashMap::new(),
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
        class_component_files: HashMap::new(),
    }
}

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

// ---------- validate_component_name ----------

#[test]
fn validates_simple_name() {
    assert!(validate_component_name("button").is_ok());
}

#[test]
fn validates_nested_dotted_name() {
    assert!(validate_component_name("forms.input").is_ok());
}

#[test]
fn validates_kebab_segments() {
    assert!(validate_component_name("forms.user-input").is_ok());
}

#[test]
fn rejects_empty() {
    assert_eq!(validate_component_name(""), Err(ComponentNameError::Empty));
}

#[test]
fn rejects_namespaced_with_friendly_error() {
    assert_eq!(
        validate_component_name("courier::alert"),
        Err(ComponentNameError::NamespacedNotSupported)
    );
}

#[test]
fn rejects_slash() {
    assert_eq!(
        validate_component_name("forms/input"),
        Err(ComponentNameError::ContainsSlash)
    );
}

#[test]
fn rejects_extension() {
    assert_eq!(
        validate_component_name("button.blade.php"),
        Err(ComponentNameError::HasExtension)
    );
}

#[test]
fn rejects_invalid_character() {
    assert_eq!(
        validate_component_name("forms@input"),
        Err(ComponentNameError::InvalidCharacter('@'))
    );
}

#[test]
fn rejects_empty_segment() {
    assert_eq!(
        validate_component_name("forms..input"),
        Err(ComponentNameError::EmptySegment)
    );
}

// ---------- class_name_for ----------

#[test]
fn class_name_for_simple_tag() {
    assert_eq!(class_name_for("button"), "Button");
}

#[test]
fn class_name_for_kebab_tag() {
    assert_eq!(class_name_for("user-profile"), "UserProfile");
}

#[test]
fn class_name_for_nested_takes_leaf() {
    // Only the leaf segment becomes the class name. Parent segments
    // affect the file path / namespace but not the class name itself.
    assert_eq!(class_name_for("forms.user-input"), "UserInput");
}

// ---------- conventional_namespace_for ----------

#[test]
fn namespace_for_top_level_component() {
    let root = Path::new("/project");
    let ns = conventional_namespace_for(Path::new("/project/app/View/Components/Button.php"), root);
    assert_eq!(ns, "App\\View\\Components");
}

#[test]
fn namespace_for_nested_component() {
    let root = Path::new("/project");
    let ns = conventional_namespace_for(
        Path::new("/project/app/View/Components/Forms/Input.php"),
        root,
    );
    assert_eq!(ns, "App\\View\\Components\\Forms");
}

#[test]
fn namespace_returns_empty_for_non_app_path() {
    let root = Path::new("/project");
    let ns = conventional_namespace_for(Path::new("/project/resources/views/x.blade.php"), root);
    assert_eq!(ns, "");
}

// ---------- find_class_declaration ----------

#[test]
fn finds_simple_class_declaration() {
    let src = "<?php\n\nnamespace App\\View\\Components;\n\nclass Button extends Component {}\n";
    let span =
        find_class_declaration(src, Path::new("/tmp/Button.php")).expect("finds declaration");
    assert_eq!(span.current_text, "Button");
    assert_eq!(span.line, 4);
}

#[test]
fn finds_class_declaration_with_final_keyword() {
    let src = "<?php\nfinal class Button extends Component {}\n";
    let span =
        find_class_declaration(src, Path::new("/tmp/Button.php")).expect("finds declaration");
    assert_eq!(span.current_text, "Button");
}

#[test]
fn finds_class_declaration_with_abstract_keyword() {
    let src = "<?php\nabstract class BaseComponent {}\n";
    let span = find_class_declaration(src, Path::new("/tmp/X.php")).expect("finds declaration");
    assert_eq!(span.current_text, "BaseComponent");
}

#[test]
fn returns_none_for_anonymous_class_only() {
    // `new class extends Component {}` shouldn't match — it has no name.
    let src = "<?php\nreturn new class extends Component {};\n";
    assert!(find_class_declaration(src, Path::new("/tmp/x.php")).is_none());
}

#[test]
fn returns_none_when_no_class() {
    let src = "<?php\nreturn ['key' => 'value'];\n";
    assert!(find_class_declaration(src, Path::new("/tmp/x.php")).is_none());
}

// ---------- find_namespace_declaration ----------

#[test]
fn finds_namespace_declaration() {
    let src = "<?php\n\nnamespace App\\View\\Components;\n\nclass Button {}\n";
    let span =
        find_namespace_declaration(src, Path::new("/tmp/Button.php")).expect("finds namespace");
    assert_eq!(span.current_text, "App\\View\\Components");
    assert_eq!(span.line, 2);
    // start_column is after `namespace ` (10 chars).
    assert_eq!(span.start_column, 10);
}

#[test]
fn finds_namespace_with_trailing_space_before_semicolon() {
    let src = "<?php\nnamespace App\\View\\Components ;\n";
    let span = find_namespace_declaration(src, Path::new("/tmp/x.php")).expect("finds namespace");
    assert_eq!(span.current_text, "App\\View\\Components");
}

#[test]
fn returns_none_when_no_namespace() {
    let src = "<?php\nclass Foo {}\n";
    assert!(find_namespace_declaration(src, Path::new("/tmp/x.php")).is_none());
}

// ---------- locate_component (filesystem) ----------

#[test]
fn locates_anonymous_blade_only() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let blade = root.join("resources/views/components/button.blade.php");
    write(&blade, "<button>{{ $slot }}</button>");

    let found = locate_component("button", &cfg).expect("finds anonymous component");
    assert_eq!(found.blade_file, Some(blade));
    assert_eq!(found.class_file, None);
    assert_eq!(found.class_declaration, None);
    assert_eq!(found.namespace_declaration, None);
}

#[test]
fn locates_class_based_with_blade_and_class_file() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let blade = root.join("resources/views/components/button.blade.php");
    write(&blade, "<button>{{ $slot }}</button>");
    let class = root.join("app/View/Components/Button.php");
    write(
        &class,
        "<?php\n\nnamespace App\\View\\Components;\n\nclass Button extends Component {}\n",
    );

    let found = locate_component("button", &cfg).expect("finds class-based component");
    assert_eq!(found.blade_file, Some(blade));
    assert_eq!(found.class_file, Some(class.clone()));

    let decl = found.class_declaration.expect("class declaration found");
    assert_eq!(decl.current_text, "Button");
    assert_eq!(decl.file_path, class);

    let ns = found.namespace_declaration.expect("namespace found");
    assert_eq!(ns.current_text, "App\\View\\Components");
}

#[test]
fn locates_nested_class_based_component() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let blade = root.join("resources/views/components/forms/input.blade.php");
    write(&blade, "<input>");
    let class = root.join("app/View/Components/Forms/Input.php");
    write(
        &class,
        "<?php\n\nnamespace App\\View\\Components\\Forms;\n\nclass Input extends Component {}\n",
    );

    let found = locate_component("forms.input", &cfg).expect("finds nested component");
    assert_eq!(found.blade_file, Some(blade));
    assert_eq!(found.class_file, Some(class));
    let ns = found.namespace_declaration.expect("namespace found");
    assert_eq!(ns.current_text, "App\\View\\Components\\Forms");
}

#[test]
fn returns_none_when_neither_file_exists() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    assert!(locate_component("nonexistent", &cfg).is_none());
}

// ---------- component_name_for_blade_path ----------

#[test]
fn component_name_from_top_level_path() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);
    let path = root.join("resources/views/components/button.blade.php");
    assert_eq!(
        component_name_for_blade_path(&path, &cfg),
        Some("button".to_string())
    );
}

#[test]
fn component_name_from_nested_path() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);
    let path = root.join("resources/views/components/forms/text-input.blade.php");
    assert_eq!(
        component_name_for_blade_path(&path, &cfg),
        Some("forms.text-input".to_string())
    );
}

#[test]
fn component_name_returns_none_for_non_component_path() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);
    let path = root.join("resources/views/users/profile.blade.php");
    assert_eq!(component_name_for_blade_path(&path, &cfg), None);
}

#[test]
fn component_name_returns_none_for_non_blade_extension() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);
    let path = root.join("resources/views/components/button.php");
    assert_eq!(component_name_for_blade_path(&path, &cfg), None);
}

#[test]
fn returns_none_for_namespaced_name() {
    // Defensive — even if validate_component_name was bypassed.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    assert!(locate_component("courier::alert", &cfg).is_none());
}
