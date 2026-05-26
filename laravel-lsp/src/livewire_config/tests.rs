use super::*;
use std::path::Path;

fn root() -> &'static Path {
    Path::new("/project")
}

#[test]
fn defaults_match_livewire_4_ship_config() {
    let cfg = LivewireConfig::defaults(root());

    assert_eq!(cfg.make_command_type, ComponentFormat::Sfc);
    assert!(cfg.make_command_emoji);
    assert_eq!(cfg.class_namespace, "App\\Livewire");
    assert_eq!(cfg.class_path, Path::new("/project/app/Livewire"));
    assert_eq!(
        cfg.view_path,
        Path::new("/project/resources/views/livewire")
    );

    assert_eq!(
        cfg.component_locations,
        vec![
            PathBuf::from("/project/resources/views/components"),
            PathBuf::from("/project/resources/views/livewire"),
        ]
    );

    let mut ns: Vec<(&String, &PathBuf)> = cfg.component_namespaces.iter().collect();
    ns.sort_by_key(|(k, _)| k.as_str());
    assert_eq!(ns.len(), 2);
    assert_eq!(ns[0].0, "layouts");
    assert_eq!(ns[0].1, &PathBuf::from("/project/resources/views/layouts"));
    assert_eq!(ns[1].0, "pages");
    assert_eq!(ns[1].1, &PathBuf::from("/project/resources/views/pages"));
}

#[test]
fn empty_source_yields_defaults() {
    let cfg = parse("", root());
    assert_eq!(cfg, LivewireConfig::defaults(root()));
}

#[test]
fn parses_make_command_emoji_false() {
    let src = r#"
        return [
            'make_command' => [
                'type' => 'sfc',
                'emoji' => false,
            ],
        ];
    "#;
    let cfg = parse(src, root());
    assert!(!cfg.make_command_emoji);
}

#[test]
fn parses_make_command_type_class() {
    let src = r#"
        'make_command' => [
            'type' => 'class',
            'emoji' => true,
        ],
    "#;
    let cfg = parse(src, root());
    assert_eq!(cfg.make_command_type, ComponentFormat::Class);
    assert!(cfg.make_command_emoji);
}

#[test]
fn parses_make_command_type_mfc() {
    let src = r#"'make_command' => ['type' => 'mfc']"#;
    let cfg = parse(src, root());
    assert_eq!(cfg.make_command_type, ComponentFormat::Mfc);
}

#[test]
fn unrecognized_make_command_type_falls_back_to_default() {
    let src = r#"'make_command' => ['type' => 'inline']"#;
    let cfg = parse(src, root());
    // Default is Sfc; 'inline' isn't recognized so we fall through.
    assert_eq!(cfg.make_command_type, ComponentFormat::Sfc);
}

#[test]
fn parses_class_namespace_with_double_escape() {
    let src = r#"'class_namespace' => 'App\\Http\\Livewire',"#;
    let cfg = parse(src, root());
    assert_eq!(cfg.class_namespace, "App\\Http\\Livewire");
}

#[test]
fn parses_class_namespace_with_single_escape() {
    // Single-quoted PHP strings don't process escapes, so `\` stays literal.
    let src = r#"'class_namespace' => 'App\Livewire',"#;
    let cfg = parse(src, root());
    assert_eq!(cfg.class_namespace, "App\\Livewire");
}

#[test]
fn parses_class_path_app_helper() {
    let src = r#"'class_path' => app_path('Livewire'),"#;
    let cfg = parse(src, root());
    assert_eq!(cfg.class_path, Path::new("/project/app/Livewire"));
}

#[test]
fn parses_view_path_resource_helper() {
    let src = r#"'view_path' => resource_path('views/livewire'),"#;
    let cfg = parse(src, root());
    assert_eq!(
        cfg.view_path,
        Path::new("/project/resources/views/livewire")
    );
}

#[test]
fn parses_class_path_base_helper() {
    let src = r#"'class_path' => base_path('packages/billing/src/Livewire'),"#;
    let cfg = parse(src, root());
    assert_eq!(
        cfg.class_path,
        Path::new("/project/packages/billing/src/Livewire")
    );
}

#[test]
fn parses_class_path_bare_string() {
    let src = r#"'class_path' => '/absolute/path',"#;
    let cfg = parse(src, root());
    assert_eq!(cfg.class_path, Path::new("/absolute/path"));
}

#[test]
fn parses_component_locations_array() {
    let src = r#"
        'component_locations' => [
            resource_path('views/components'),
            resource_path('views/livewire'),
            base_path('packages/billing/views'),
        ],
    "#;
    let cfg = parse(src, root());
    assert_eq!(
        cfg.component_locations,
        vec![
            PathBuf::from("/project/resources/views/components"),
            PathBuf::from("/project/resources/views/livewire"),
            PathBuf::from("/project/packages/billing/views"),
        ]
    );
}

#[test]
fn parses_component_namespaces_map() {
    let src = r#"
        'component_namespaces' => [
            'layouts' => resource_path('views/layouts'),
            'pages' => resource_path('views/pages'),
            'billing' => base_path('packages/billing/views'),
        ],
    "#;
    let cfg = parse(src, root());
    assert_eq!(cfg.component_namespaces.len(), 3);
    assert_eq!(
        cfg.component_namespaces.get("layouts"),
        Some(&PathBuf::from("/project/resources/views/layouts"))
    );
    assert_eq!(
        cfg.component_namespaces.get("pages"),
        Some(&PathBuf::from("/project/resources/views/pages"))
    );
    assert_eq!(
        cfg.component_namespaces.get("billing"),
        Some(&PathBuf::from("/project/packages/billing/views"))
    );
}

#[test]
fn parses_full_default_config() {
    // The exact body Livewire 4 ships in config/livewire.php (trimmed of
    // comments). Verifies that the parser produces the documented defaults
    // when given the documented input — round-trip sanity.
    let src = r#"<?php
        return [
            'component_locations' => [
                resource_path('views/components'),
                resource_path('views/livewire'),
            ],
            'component_namespaces' => [
                'layouts' => resource_path('views/layouts'),
                'pages' => resource_path('views/pages'),
            ],
            'make_command' => [
                'type' => 'sfc',
                'emoji' => true,
                'with' => [
                    'js' => false,
                    'css' => false,
                    'test' => false,
                ],
            ],
            'class_namespace' => 'App\\Livewire',
            'class_path' => app_path('Livewire'),
            'view_path' => resource_path('views/livewire'),
        ];
    "#;
    let cfg = parse(src, root());
    assert_eq!(cfg, LivewireConfig::defaults(root()));
}

#[test]
fn handles_double_quoted_keys() {
    let src = r#"
        return [
            "class_namespace" => "App\\Livewire",
            "view_path" => resource_path("views/livewire"),
        ];
    "#;
    let cfg = parse(src, root());
    assert_eq!(cfg.class_namespace, "App\\Livewire");
    assert_eq!(
        cfg.view_path,
        Path::new("/project/resources/views/livewire")
    );
}

#[test]
fn ignores_keys_on_comment_lines() {
    // A `//` comment line that mentions the key shouldn't be treated as a
    // real assignment. Without this guard, a stray "// 'emoji' => false"
    // doc snippet could flip the parsed value.
    let src = r#"
        return [
            // 'emoji' => false,
            'make_command' => [
                'emoji' => true,
            ],
        ];
    "#;
    let cfg = parse(src, root());
    assert!(cfg.make_command_emoji);
}

#[test]
fn partial_failure_falls_back_per_key() {
    // class_namespace parses; class_path doesn't (no value). Only the
    // failed key should fall back; the parsed key keeps its parsed value.
    let src = r#"
        'class_namespace' => 'App\\Custom\\Livewire',
        'class_path' => ,
    "#;
    let cfg = parse(src, root());
    assert_eq!(cfg.class_namespace, "App\\Custom\\Livewire");
    // class_path stays at default.
    assert_eq!(cfg.class_path, Path::new("/project/app/Livewire"));
}

#[test]
fn empty_array_overrides_to_default() {
    // An empty `component_locations => []` is an unusual user choice. The
    // parser treats "extracted but empty" as "fall back to defaults" rather
    // than committing to an empty list that would brick all rename lookups.
    let src = r#"'component_locations' => [],"#;
    let cfg = parse(src, root());
    assert_eq!(cfg.component_locations.len(), 2); // defaults preserved
}

#[test]
fn tolerates_path_helpers_with_no_arg() {
    // `app_path()` with no arg points at the app directory itself.
    let src = r#"'class_path' => app_path(),"#;
    let cfg = parse(src, root());
    assert_eq!(cfg.class_path, Path::new("/project/app"));
}
