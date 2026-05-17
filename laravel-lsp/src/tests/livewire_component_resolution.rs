use laravel_lsp::livewire_resolver::{blade_contains_inline_class, mfc_sibling};
use laravel_lsp::php_class::{
    detect_inline_livewire_class, find_all_mount_param_types, find_mount_param_type,
    find_property_type_in_content,
};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

// ============================================================================
// detect_inline_livewire_class
// ============================================================================

#[test]
fn detects_bare_component_extends() {
    let src = r#"<?php
        use Livewire\Component;
        new class extends Component { public string $foo = "bar"; };
    "#;
    assert!(detect_inline_livewire_class(src));
}

#[test]
fn detects_fully_qualified_livewire_component() {
    let src = r#"<?php new class extends \Livewire\Component { }; "#;
    assert!(detect_inline_livewire_class(src));
}

#[test]
fn detects_with_layout_attribute() {
    let src = r#"<?php
        new #[Layout('layouts.app')] class extends Component { };
    "#;
    assert!(detect_inline_livewire_class(src));
}

#[test]
fn detects_with_multiple_attributes() {
    let src = r#"<?php
        new #[Layout('x')] #[Title('y')] class extends Component { };
    "#;
    assert!(detect_inline_livewire_class(src));
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
    assert!(detect_inline_livewire_class(src));
}

#[test]
fn rejects_plain_blade_without_class() {
    let src = r#"
        <div>{{ $foo }}</div>
        @if ($bar) baz @endif
    "#;
    assert!(!detect_inline_livewire_class(src));
}

#[test]
fn rejects_anonymous_class_extending_unrelated_base() {
    let src = r#"<?php new class extends OtherBase { }; "#;
    assert!(!detect_inline_livewire_class(src));
}

#[test]
fn rejects_named_class_definition() {
    let src = r#"<?php class FooComponent extends Component { } "#;
    assert!(!detect_inline_livewire_class(src));
}

// ============================================================================
// find_mount_param_type — type *refinement* for declared but untyped properties.
// Returns None for untyped params; this helper never synthesizes properties.
// ============================================================================

#[test]
fn mount_param_type_is_class() {
    let src = r#"<?php
        new class extends Component {
            public function mount(User $user) { }
        };
    "#;
    assert_eq!(find_mount_param_type(src, "user"), Some("User".into()));
}

#[test]
fn mount_param_type_simplifies_fully_qualified_class() {
    let src = r#"<?php
        new class extends Component {
            public function mount(\App\Models\User $user) { }
        };
    "#;
    assert_eq!(find_mount_param_type(src, "user"), Some("User".into()));
}

#[test]
fn mount_param_type_is_primitive() {
    let src = r#"<?php
        new class extends Component {
            public function mount(string $name) { }
        };
    "#;
    assert_eq!(find_mount_param_type(src, "name"), Some("string".into()));
}

#[test]
fn mount_param_type_is_nullable() {
    let src = r#"<?php
        new class extends Component {
            public function mount(?User $user) { }
        };
    "#;
    assert_eq!(find_mount_param_type(src, "user"), Some("?User".into()));
}

#[test]
fn mount_param_type_untyped_returns_none() {
    // Untyped mount params do not refine — refinement requires type info to add.
    let src = r#"<?php
        new class extends Component {
            public function mount($foo) { }
        };
    "#;
    assert_eq!(find_mount_param_type(src, "foo"), None);
}

#[test]
fn mount_param_type_picks_correct_param_among_many() {
    let src = r#"<?php
        new class extends Component {
            public function mount(string $first, User $second, int $third = 5) { }
        };
    "#;
    assert_eq!(find_mount_param_type(src, "second"), Some("User".into()));
    assert_eq!(find_mount_param_type(src, "third"), Some("int".into()));
}

#[test]
fn mount_param_type_missing_param_returns_none() {
    let src = r#"<?php
        new class extends Component {
            public function mount(User $user) { }
        };
    "#;
    assert_eq!(find_mount_param_type(src, "missing"), None);
}

#[test]
fn mount_param_type_no_mount_function_returns_none() {
    let src = r#"<?php
        new class extends Component {
            public User $user;
        };
    "#;
    assert_eq!(find_mount_param_type(src, "user"), None);
}

#[test]
fn mount_param_type_first_match_wins_across_classes() {
    let src = r#"<?php
        new class extends Component {
            public function mount(FirstType $shared) { }
        };
        new class extends Component {
            public function mount(SecondType $shared) { }
        };
    "#;
    assert_eq!(
        find_mount_param_type(src, "shared"),
        Some("FirstType".into())
    );
}

// ============================================================================
// find_all_mount_param_types — only typed params are listed; untyped skipped.
// ============================================================================

#[test]
fn all_mount_param_types_skips_untyped() {
    let src = r#"<?php
        new class extends Component {
            public function mount(string $name, $foo, ?Bar $bar) { }
        };
    "#;
    let params = find_all_mount_param_types(src);
    assert_eq!(params.len(), 2);
    assert!(params.contains(&("name".to_string(), "string".to_string())));
    assert!(params.contains(&("bar".to_string(), "?Bar".to_string())));
    assert!(!params.iter().any(|(n, _)| n == "foo"));
}

#[test]
fn all_mount_param_types_dedupes_by_name_first_wins() {
    let src = r#"<?php
        new class extends Component {
            public function mount(FirstType $x) { }
        };
        new class extends Component {
            public function mount(SecondType $x, OtherType $y) { }
        };
    "#;
    let params = find_all_mount_param_types(src);
    assert!(params.contains(&("x".to_string(), "FirstType".to_string())));
    assert!(params.contains(&("y".to_string(), "OtherType".to_string())));
    assert!(!params.contains(&("x".to_string(), "SecondType".to_string())));
}

// ============================================================================
// mfc_sibling
// ============================================================================

fn write(path: &PathBuf, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

#[test]
fn mfc_sibling_found_when_livewire_class_present() {
    let dir = TempDir::new().unwrap();
    let blade = dir.path().join("pages/contact/contact.blade.php");
    let sibling = dir.path().join("pages/contact/contact.php");
    write(&blade, "<div>{{ $form->name }}</div>");
    write(
        &sibling,
        "<?php new class extends Component { public Form $form; };",
    );

    let resolved = mfc_sibling(&blade).expect("sibling should resolve");
    assert_eq!(resolved, sibling);
}

#[test]
fn mfc_sibling_skipped_when_no_livewire_signature() {
    let dir = TempDir::new().unwrap();
    let blade = dir.path().join("pages/about.blade.php");
    let sibling = dir.path().join("pages/about.php");
    write(&blade, "<div>about</div>");
    write(&sibling, "<?php return ['title' => 'About'];");

    assert!(mfc_sibling(&blade).is_none());
}

#[test]
fn mfc_sibling_skipped_when_no_sibling_file() {
    let dir = TempDir::new().unwrap();
    let blade = dir.path().join("pages/lonely.blade.php");
    write(&blade, "<div>standalone</div>");

    assert!(mfc_sibling(&blade).is_none());
}

// ============================================================================
// blade_contains_inline_class
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

    assert!(blade_contains_inline_class(&blade));
}

#[test]
fn sfc_not_detected_for_plain_blade() {
    let dir = TempDir::new().unwrap();
    let blade = dir.path().join("livewire/bar.blade.php");
    write(&blade, "<div>plain {{ $foo }}</div>");

    assert!(!blade_contains_inline_class(&blade));
}

#[test]
fn sfc_not_detected_for_missing_file() {
    let path = PathBuf::from("/nonexistent/blade/file.blade.php");
    assert!(!blade_contains_inline_class(&path));
}

// ============================================================================
// End-to-end: property type lookup with refinement semantics
// ============================================================================
//
// `lookup_type` mirrors the Backend's resolution order:
//   1. Pick component source (MFC sibling > SFC inline > classic mapping handled by Backend)
//   2. Verify the property is actually declared (refinement never synthesizes)
//   3. Prefer explicit property type, then refine via matching `mount()` param

fn has_public_property(content: &str, name: &str) -> bool {
    let escaped = regex::escape(name);
    let pattern = format!(
        r"public\s+(?:\??\\?[A-Za-z_][A-Za-z0-9_\\]*\s+)?\${}\b",
        escaped
    );
    regex::Regex::new(&pattern)
        .ok()
        .map(|re| re.is_match(content))
        .unwrap_or(false)
}

fn lookup_type(blade_path: &Path, var_name: &str) -> Option<String> {
    let component_path = if let Some(sibling) = mfc_sibling(blade_path) {
        sibling
    } else if blade_contains_inline_class(blade_path) {
        blade_path.to_path_buf()
    } else {
        return None;
    };

    let content = fs::read_to_string(&component_path).ok()?;

    if !has_public_property(&content, var_name) {
        return None;
    }

    if let Some(t) = find_property_type_in_content(&content, var_name) {
        return Some(t);
    }
    find_mount_param_type(&content, var_name)
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
fn e2e_mfc_untyped_property_refined_by_mount_param() {
    // Property exists but is untyped; mount() param supplies the type.
    let dir = TempDir::new().unwrap();
    let blade = dir.path().join("pages/profile/profile.blade.php");
    let sibling = dir.path().join("pages/profile/profile.php");
    write(&blade, "<div>{{ $user->name }}</div>");
    write(
        &sibling,
        r#"<?php
            new class extends Component {
                public $user;
                public function mount(\App\Models\User $user) {
                    $this->user = $user;
                }
            };
        "#,
    );

    assert_eq!(lookup_type(&blade, "user"), Some("User".to_string()));
}

#[test]
fn e2e_sfc_untyped_property_refined_by_mount_param() {
    let dir = TempDir::new().unwrap();
    let blade = dir.path().join("livewire/welcome.blade.php");
    write(
        &blade,
        r#"<?php
            new class extends Component {
                public $heading;
                public function mount(string $heading) {
                    $this->heading = $heading;
                }
            };
            ?>
            <h1>{{ $heading }}</h1>
        "#,
    );

    assert_eq!(lookup_type(&blade, "heading"), Some("string".to_string()));
}

#[test]
fn e2e_typed_property_keeps_its_type_even_when_mount_param_differs() {
    // Explicit property type wins over mount() param type. (Wouldn't normally
    // differ in real code, but the property declaration is the source of truth.)
    let dir = TempDir::new().unwrap();
    let blade = dir.path().join("livewire/post.blade.php");
    write(
        &blade,
        r#"<?php
            new class extends Component {
                public Post $post;
                public function mount(SomeOther $post) {
                    $this->post = $post;
                }
            };
        "#,
    );

    assert_eq!(lookup_type(&blade, "post"), Some("Post".to_string()));
}

#[test]
fn e2e_no_property_declaration_returns_none_even_with_mount_param() {
    // Critical regression guard: Livewire never synthesizes properties from
    // mount() params alone. The LSP must not surface a type for `$user` here.
    let dir = TempDir::new().unwrap();
    let blade = dir.path().join("livewire/synth.blade.php");
    write(
        &blade,
        r#"<?php
            new class extends Component {
                public function mount(User $user) {
                    $foo = $user->name;
                }
            };
        "#,
    );

    assert_eq!(lookup_type(&blade, "user"), None);
}

#[test]
fn e2e_multi_class_blade_resolves_properties_from_any_class() {
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

#[test]
fn e2e_regression_emoji_directory_mfc_resolves() {
    // Mirrors the exact layout used by the Crossbible contact-us component:
    //   resources/views/pages/⚡contact-us/contact-us.blade.php
    //   resources/views/pages/⚡contact-us/contact-us.php
    // The directory carries Livewire 4's ⚡ marker (non-ASCII); the sibling .php
    // file holds the anonymous-class component definition.
    let dir = TempDir::new().unwrap();
    let blade = dir
        .path()
        .join("resources/views/pages/\u{26A1}contact-us/contact-us.blade.php");
    let sibling = dir
        .path()
        .join("resources/views/pages/\u{26A1}contact-us/contact-us.php");

    write(&blade, "<div>{{ $form->name }}</div>");
    write(
        &sibling,
        r#"<?php

declare(strict_types=1);

use App\Livewire\Forms\ContactForm;
use Livewire\Component;

new class extends Component
{
    public ContactForm $form;
    public bool $isSubmitted = false;

    public function submit(): void
    {
        // ...
    }
};
"#,
    );

    assert_eq!(lookup_type(&blade, "form"), Some("ContactForm".to_string()));
}
