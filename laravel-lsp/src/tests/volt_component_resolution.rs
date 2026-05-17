use laravel_lsp::livewire_resolver::{
    blade_contains_inline_volt_class, volt_mfc_sibling, ComponentKind,
};
use laravel_lsp::php_class::{
    detect_inline_volt_class, find_all_mount_promoted_params, find_mount_promoted_type,
    find_property_type_in_content,
};
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

// ============================================================================
// detect_inline_volt_class
// ============================================================================

#[test]
fn detects_bare_component_extends() {
    let src = r#"<?php
        use Livewire\Volt\Component;
        new class extends Component { public string $foo = "bar"; };
    "#;
    assert!(detect_inline_volt_class(src));
}

#[test]
fn detects_fully_qualified_volt_component() {
    let src = r#"<?php new class extends \Livewire\Volt\Component { }; "#;
    assert!(detect_inline_volt_class(src));
}

#[test]
fn detects_fully_qualified_livewire_component() {
    let src = r#"<?php new class extends \Livewire\Component { }; "#;
    assert!(detect_inline_volt_class(src));
}

#[test]
fn detects_with_layout_attribute() {
    let src = r#"<?php
        new #[Layout('layouts.app')] class extends Component { };
    "#;
    assert!(detect_inline_volt_class(src));
}

#[test]
fn detects_with_multiple_attributes() {
    let src = r#"<?php
        new #[Layout('x')] #[Title('y')] class extends Component { };
    "#;
    assert!(detect_inline_volt_class(src));
}

#[test]
fn detects_when_embedded_in_blade_content() {
    let src = r#"
        @props(['foo'])
        <div>
            <?php new class extends Component { public string $bar = ''; }; ?>
        </div>
        <p>{{ $bar }}</p>
    "#;
    assert!(detect_inline_volt_class(src));
}

#[test]
fn rejects_plain_blade_without_class() {
    let src = r#"
        <div>{{ $foo }}</div>
        @if ($bar) baz @endif
    "#;
    assert!(!detect_inline_volt_class(src));
}

#[test]
fn rejects_anonymous_class_extending_unrelated_base() {
    let src = r#"<?php new class extends OtherBase { }; "#;
    assert!(!detect_inline_volt_class(src));
}

#[test]
fn rejects_named_class_definition() {
    let src = r#"<?php class FooComponent extends Component { } "#;
    assert!(!detect_inline_volt_class(src));
}

// ============================================================================
// find_mount_promoted_type
// ============================================================================

#[test]
fn mount_promotes_class_typed_param() {
    let src = r#"<?php
        new class extends Component {
            public function mount(User $user) { }
        };
    "#;
    assert_eq!(find_mount_promoted_type(src, "user"), Some("User".into()));
}

#[test]
fn mount_promotes_fully_qualified_class_param() {
    let src = r#"<?php
        new class extends Component {
            public function mount(\App\Models\User $user) { }
        };
    "#;
    assert_eq!(find_mount_promoted_type(src, "user"), Some("User".into()));
}

#[test]
fn mount_promotes_primitive_param() {
    let src = r#"<?php
        new class extends Component {
            public function mount(string $name) { }
        };
    "#;
    assert_eq!(find_mount_promoted_type(src, "name"), Some("string".into()));
}

#[test]
fn mount_promotes_nullable_class_param() {
    let src = r#"<?php
        new class extends Component {
            public function mount(?User $user) { }
        };
    "#;
    assert_eq!(find_mount_promoted_type(src, "user"), Some("?User".into()));
}

#[test]
fn mount_promotes_untyped_param_to_mixed() {
    let src = r#"<?php
        new class extends Component {
            public function mount($foo) { }
        };
    "#;
    assert_eq!(find_mount_promoted_type(src, "foo"), Some("mixed".into()));
}

#[test]
fn mount_finds_param_among_many() {
    let src = r#"<?php
        new class extends Component {
            public function mount(string $first, User $second, int $third = 5) { }
        };
    "#;
    assert_eq!(find_mount_promoted_type(src, "second"), Some("User".into()));
    assert_eq!(find_mount_promoted_type(src, "third"), Some("int".into()));
}

#[test]
fn mount_returns_none_for_missing_param() {
    let src = r#"<?php
        new class extends Component {
            public function mount(User $user) { }
        };
    "#;
    assert_eq!(find_mount_promoted_type(src, "missing"), None);
}

#[test]
fn mount_returns_none_when_no_mount_function() {
    let src = r#"<?php
        new class extends Component {
            public User $user;
        };
    "#;
    assert_eq!(find_mount_promoted_type(src, "user"), None);
}

#[test]
fn mount_first_match_wins_across_multiple_classes() {
    // A file with two Volt classes — each defining its own mount() — should resolve
    // the first occurrence by name (first-match-wins, deterministic order).
    let src = r#"<?php
        new class extends Component {
            public function mount(FirstType $shared) { }
        };
        new class extends Component {
            public function mount(SecondType $shared) { }
        };
    "#;
    assert_eq!(
        find_mount_promoted_type(src, "shared"),
        Some("FirstType".into())
    );
}

// ============================================================================
// find_all_mount_promoted_params
// ============================================================================

#[test]
fn all_mount_params_lists_typed_and_untyped() {
    let src = r#"<?php
        new class extends Component {
            public function mount(string $name, $foo, ?Bar $bar) { }
        };
    "#;
    let params = find_all_mount_promoted_params(src);
    assert_eq!(params.len(), 3);
    assert!(params.contains(&("name".to_string(), "string".to_string())));
    assert!(params.contains(&("foo".to_string(), "mixed".to_string())));
    assert!(params.contains(&("bar".to_string(), "?Bar".to_string())));
}

#[test]
fn all_mount_params_dedupes_by_name_first_wins() {
    let src = r#"<?php
        new class extends Component {
            public function mount(FirstType $x) { }
        };
        new class extends Component {
            public function mount(SecondType $x, OtherType $y) { }
        };
    "#;
    let params = find_all_mount_promoted_params(src);
    assert!(params.contains(&("x".to_string(), "FirstType".to_string())));
    assert!(params.contains(&("y".to_string(), "OtherType".to_string())));
    assert!(!params.contains(&("x".to_string(), "SecondType".to_string())));
}

// ============================================================================
// volt_mfc_sibling
// ============================================================================

fn write(path: &PathBuf, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

#[test]
fn mfc_sibling_found_when_volt_class_present() {
    let dir = TempDir::new().unwrap();
    let blade = dir.path().join("pages/contact/contact.blade.php");
    let sibling = dir.path().join("pages/contact/contact.php");
    write(&blade, "<div>{{ $form->name }}</div>");
    write(
        &sibling,
        "<?php new class extends Component { public Form $form; };",
    );

    let resolved = volt_mfc_sibling(&blade).expect("sibling should resolve");
    assert_eq!(resolved, sibling);
}

#[test]
fn mfc_sibling_skipped_when_no_volt_signature() {
    let dir = TempDir::new().unwrap();
    let blade = dir.path().join("pages/about.blade.php");
    let sibling = dir.path().join("pages/about.php");
    write(&blade, "<div>about</div>");
    // Sibling file exists but is not a Volt component (e.g. a config helper)
    write(&sibling, "<?php return ['title' => 'About'];");

    assert!(volt_mfc_sibling(&blade).is_none());
}

#[test]
fn mfc_sibling_skipped_when_no_sibling_file() {
    let dir = TempDir::new().unwrap();
    let blade = dir.path().join("pages/lonely.blade.php");
    write(&blade, "<div>standalone</div>");

    assert!(volt_mfc_sibling(&blade).is_none());
}

// ============================================================================
// blade_contains_inline_volt_class
// ============================================================================

#[test]
fn sfc_detected_when_blade_has_inline_class() {
    let dir = TempDir::new().unwrap();
    let blade = dir.path().join("livewire/foo.blade.php");
    write(
        &blade,
        r#"<?php new class extends Component { public string $msg = 'hi'; }; ?>
           <p>{{ $msg }}</p>"#,
    );

    assert!(blade_contains_inline_volt_class(&blade));
}

#[test]
fn sfc_not_detected_for_plain_blade() {
    let dir = TempDir::new().unwrap();
    let blade = dir.path().join("livewire/bar.blade.php");
    write(&blade, "<div>plain {{ $foo }}</div>");

    assert!(!blade_contains_inline_volt_class(&blade));
}

#[test]
fn sfc_not_detected_for_missing_file() {
    let path = PathBuf::from("/nonexistent/blade/file.blade.php");
    assert!(!blade_contains_inline_volt_class(&path));
}

// ============================================================================
// End-to-end: property type lookup across all three patterns
// ============================================================================
//
// These tests mirror the real LSP flow. They don't construct a Backend (that
// requires LSP client wiring); instead they exercise the same helpers the
// Backend methods compose: the resolver picks a source path + kind, then the
// property-type extractor scans that source.

fn lookup_type(blade_path: &PathBuf, var_name: &str) -> Option<String> {
    // Match the resolver order used inside `Backend::find_livewire_component_php`.
    let (component_path, kind) = if let Some(sibling) = volt_mfc_sibling(blade_path) {
        (sibling, ComponentKind::Volt)
    } else if blade_contains_inline_volt_class(blade_path) {
        (blade_path.clone(), ComponentKind::Volt)
    } else {
        return None;
    };

    let content = fs::read_to_string(&component_path).ok()?;

    if let Some(t) = find_property_type_in_content(&content, var_name) {
        return Some(t);
    }
    if matches!(kind, ComponentKind::Volt) {
        return find_mount_promoted_type(&content, var_name);
    }
    None
}

#[test]
fn e2e_mfc_with_explicit_property_resolves() {
    let dir = TempDir::new().unwrap();
    let blade = dir.path().join("pages/contact/contact.blade.php");
    let sibling = dir.path().join("pages/contact/contact.php");
    write(&blade, "<div>{{ $form->name }}</div>");
    write(
        &sibling,
        r#"<?php
            use App\Livewire\Forms\ContactForm;
            new class extends Component {
                public ContactForm $form;
            };
        "#,
    );

    assert_eq!(lookup_type(&blade, "form"), Some("ContactForm".to_string()));
}

#[test]
fn e2e_mfc_with_mount_promoted_param_resolves() {
    let dir = TempDir::new().unwrap();
    let blade = dir.path().join("pages/profile/profile.blade.php");
    let sibling = dir.path().join("pages/profile/profile.php");
    write(&blade, "<div>{{ $user->name }}</div>");
    write(
        &sibling,
        r#"<?php
            new class extends Component {
                public function mount(\App\Models\User $user) { }
            };
        "#,
    );

    assert_eq!(lookup_type(&blade, "user"), Some("User".to_string()));
}

#[test]
fn e2e_sfc_with_explicit_property_resolves() {
    let dir = TempDir::new().unwrap();
    let blade = dir.path().join("livewire/counter.blade.php");
    write(
        &blade,
        r#"<?php
            new class extends Component {
                public Counter $widget;
            };
            ?>
            <div>{{ $widget->total }}</div>
        "#,
    );

    assert_eq!(lookup_type(&blade, "widget"), Some("Counter".to_string()));
}

#[test]
fn e2e_sfc_with_mount_promoted_param_resolves() {
    let dir = TempDir::new().unwrap();
    let blade = dir.path().join("livewire/welcome.blade.php");
    write(
        &blade,
        r#"<?php
            new class extends Component {
                public function mount(string $heading) { }
            };
            ?>
            <h1>{{ $heading }}</h1>
        "#,
    );

    assert_eq!(lookup_type(&blade, "heading"), Some("string".to_string()));
}

#[test]
fn e2e_sfc_with_untyped_mount_param_resolves_to_mixed() {
    let dir = TempDir::new().unwrap();
    let blade = dir.path().join("livewire/bag.blade.php");
    write(
        &blade,
        r#"<?php
            new class extends Component {
                public function mount($payload) { }
            };
        "#,
    );

    assert_eq!(lookup_type(&blade, "payload"), Some("mixed".to_string()));
}

#[test]
fn e2e_multi_class_blade_resolves_properties_from_any_class() {
    // Two Volt classes in one file (malformed per Volt's compiler, but the
    // resolver shouldn't break or pretend the file is empty). The regex scans
    // top-to-bottom and finds the first matching declaration.
    let dir = TempDir::new().unwrap();
    let blade = dir.path().join("livewire/twin.blade.php");
    write(
        &blade,
        r#"<?php
            new class extends Component {
                public Alpha $first;
            };
            new class extends Component {
                public Beta $second;
            };
        "#,
    );

    assert_eq!(lookup_type(&blade, "first"), Some("Alpha".to_string()));
    assert_eq!(lookup_type(&blade, "second"), Some("Beta".to_string()));
}

#[test]
fn e2e_plain_blade_returns_none() {
    let dir = TempDir::new().unwrap();
    let blade = dir.path().join("pages/about.blade.php");
    write(&blade, "<div>{{ $foo }}</div>");

    assert_eq!(lookup_type(&blade, "foo"), None);
}

#[test]
fn e2e_mfc_takes_priority_over_sfc() {
    // If a blade has BOTH a sibling Volt class AND an inline Volt class, the
    // MFC sibling wins (matches the resolver's documented ordering).
    let dir = TempDir::new().unwrap();
    let blade = dir.path().join("pages/dual/dual.blade.php");
    let sibling = dir.path().join("pages/dual/dual.php");
    write(
        &blade,
        r#"<?php
            new class extends Component {
                public InlineType $shared;
            };
        "#,
    );
    write(
        &sibling,
        r#"<?php
            new class extends Component {
                public SiblingType $shared;
            };
        "#,
    );

    assert_eq!(
        lookup_type(&blade, "shared"),
        Some("SiblingType".to_string())
    );
}
