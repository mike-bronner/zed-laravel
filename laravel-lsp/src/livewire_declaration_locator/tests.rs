use super::*;
use crate::livewire_config::LivewireConfig;
use crate::livewire_version::LivewireVersion;
use std::fs;
use tempfile::TempDir;

fn config_for(root: &Path) -> LivewireConfig {
    LivewireConfig::defaults(root)
}

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

// ---------- validate_livewire_name ----------

#[test]
fn validates_simple_name() {
    assert!(validate_livewire_name("counter").is_ok());
}

#[test]
fn validates_nested_name() {
    assert!(validate_livewire_name("admin.user-list").is_ok());
}

#[test]
fn rejects_empty() {
    assert_eq!(validate_livewire_name(""), Err(LivewireNameError::Empty));
}

#[test]
fn rejects_namespaced() {
    assert_eq!(
        validate_livewire_name("billing::invoice"),
        Err(LivewireNameError::NamespacedNotSupported)
    );
}

#[test]
fn rejects_slash() {
    assert_eq!(
        validate_livewire_name("admin/counter"),
        Err(LivewireNameError::ContainsSlash)
    );
}

#[test]
fn rejects_extension() {
    assert_eq!(
        validate_livewire_name("counter.blade.php"),
        Err(LivewireNameError::HasExtension)
    );
}

#[test]
fn rejects_invalid_character() {
    assert_eq!(
        validate_livewire_name("admin@counter"),
        Err(LivewireNameError::InvalidCharacter('@'))
    );
}

#[test]
fn rejects_empty_segment() {
    assert_eq!(
        validate_livewire_name(".counter"),
        Err(LivewireNameError::EmptySegment)
    );
}

// ---------- locate (V4 SFC) ----------

#[test]
fn locate_v4_sfc_returns_blade_path() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let blade = root.join(format!(
        "resources/views/livewire/{}counter.blade.php",
        naming::LIVEWIRE_EMOJI
    ));
    write(
        &blade,
        "<?php new class extends Component {}; ?><div></div>",
    );

    let found = locate("counter", &cfg, LivewireVersion::V4).expect("locates");
    assert_eq!(found.kind, LivewireComponentKind::V4Sfc);
    assert_eq!(found.paths, vec![blade]);
    assert!(found.class_declaration.is_none());
    assert!(found.namespace_declaration.is_none());
}

// ---------- locate (V4 MFC) ----------

#[test]
fn locate_v4_mfc_returns_dir_and_children() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let dir = root.join(format!(
        "resources/views/components/{}counter",
        naming::LIVEWIRE_EMOJI
    ));
    write(&dir.join("counter.php"), "<?php new class {}; ?>");
    write(&dir.join("counter.blade.php"), "<div></div>");

    let found = locate("counter", &cfg, LivewireVersion::V4).expect("locates");
    assert_eq!(found.kind, LivewireComponentKind::V4Mfc);
    assert_eq!(found.paths[0], dir);
    assert!(found.paths.iter().any(|p| p.ends_with("counter.php")));
    assert!(found.paths.iter().any(|p| p.ends_with("counter.blade.php")));
}

// ---------- locate (V3 Class) ----------

#[test]
fn locate_v3_class_loads_decl_positions() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let class = root.join("app/Livewire/Counter.php");
    write(
        &class,
        "<?php\n\nnamespace App\\Livewire;\n\nclass Counter extends Component {}\n",
    );

    let found = locate("counter", &cfg, LivewireVersion::V3).expect("locates");
    assert_eq!(found.kind, LivewireComponentKind::V3Class);
    let decl = found.class_declaration.expect("class decl");
    assert_eq!(decl.current_text, "Counter");
    let ns = found.namespace_declaration.expect("namespace decl");
    assert_eq!(ns.current_text, "App\\Livewire");
}

// ---------- locate (Volt) ----------

#[test]
fn locate_volt_returns_blade_path() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let blade = root.join("resources/views/livewire/counter.blade.php");
    write(
        &blade,
        "<?php\nuse function Livewire\\Volt\\state;\nstate(['count' => 0]);\n?>",
    );

    let found = locate("counter", &cfg, LivewireVersion::V4).expect("locates");
    assert_eq!(found.kind, LivewireComponentKind::Volt);
    assert_eq!(found.paths, vec![blade]);
}

#[test]
fn locate_returns_none_for_unknown() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);
    assert!(locate("does.not.exist", &cfg, LivewireVersion::V4).is_none());
}

#[test]
fn locate_returns_none_for_namespaced() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);
    assert!(locate("billing::invoice", &cfg, LivewireVersion::V4).is_none());
}

// ---------- compute_target_paths (V4 SFC) ----------

#[test]
fn target_v4_sfc_same_dir_with_emoji() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let blade = root.join(format!(
        "resources/views/livewire/{}counter.blade.php",
        naming::LIVEWIRE_EMOJI
    ));
    write(&blade, "<?php new class extends Component {}; ?>");

    let current = locate("counter", &cfg, LivewireVersion::V4).unwrap();
    let target = compute_target_paths("counter", "published", &current, &cfg).expect("computes");
    assert_eq!(
        target.paths,
        vec![root.join(format!(
            "resources/views/livewire/{}published.blade.php",
            naming::LIVEWIRE_EMOJI
        ))]
    );
}

#[test]
fn target_v4_sfc_cross_dir() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let blade = root.join(format!(
        "resources/views/livewire/{}counter.blade.php",
        naming::LIVEWIRE_EMOJI
    ));
    write(&blade, "<?php new class extends Component {}; ?>");

    let current = locate("counter", &cfg, LivewireVersion::V4).unwrap();
    let target =
        compute_target_paths("counter", "admin.user-list", &current, &cfg).expect("computes");
    assert_eq!(
        target.paths,
        vec![root.join(format!(
            "resources/views/livewire/admin/{}user-list.blade.php",
            naming::LIVEWIRE_EMOJI
        ))]
    );
}

// ---------- compute_target_paths (V4 MFC) ----------

#[test]
fn target_v4_mfc_renames_dir_and_children() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let dir = root.join(format!(
        "resources/views/components/{}counter",
        naming::LIVEWIRE_EMOJI
    ));
    write(&dir.join("counter.php"), "<?php new class {}; ?>");
    write(&dir.join("counter.blade.php"), "<div></div>");
    write(&dir.join("counter.js"), "");

    let current = locate("counter", &cfg, LivewireVersion::V4).unwrap();
    let target = compute_target_paths("counter", "published", &current, &cfg).expect("computes");

    let new_dir = root.join(format!(
        "resources/views/components/{}published",
        naming::LIVEWIRE_EMOJI
    ));
    assert_eq!(target.paths[0], new_dir);
    // Children: php, blade.php, js (in MFC_CHILD_EXTENSIONS order). All
    // must use the new leaf name as basename.
    assert!(target
        .paths
        .iter()
        .any(|p| p == &new_dir.join("published.php")));
    assert!(target
        .paths
        .iter()
        .any(|p| p == &new_dir.join("published.blade.php")));
    assert!(target
        .paths
        .iter()
        .any(|p| p == &new_dir.join("published.js")));
}

// ---------- compute_target_paths (V3 Class) ----------

#[test]
fn target_v3_class_same_dir_no_view() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let class = root.join("app/Livewire/Counter.php");
    write(
        &class,
        "<?php\nnamespace App\\Livewire;\nclass Counter {}\n",
    );

    let current = locate("counter", &cfg, LivewireVersion::V3).unwrap();
    let target = compute_target_paths("counter", "published", &current, &cfg).expect("computes");
    assert_eq!(target.paths, vec![root.join("app/Livewire/Published.php")]);
}

#[test]
fn target_v3_class_cross_dir_with_view() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let class = root.join("app/Livewire/Counter.php");
    write(
        &class,
        "<?php\nnamespace App\\Livewire;\nclass Counter {}\n",
    );
    let view = root.join("resources/views/livewire/counter.blade.php");
    write(&view, "<div></div>");

    let current = locate("counter", &cfg, LivewireVersion::V3).unwrap();
    let target =
        compute_target_paths("counter", "admin.user-list", &current, &cfg).expect("computes");
    assert_eq!(target.paths.len(), 2);
    assert_eq!(
        target.paths[0],
        root.join("app/Livewire/Admin/UserList.php")
    );
    assert_eq!(
        target.paths[1],
        root.join("resources/views/livewire/admin/user-list.blade.php")
    );
}

// ---------- compute_target_paths (Volt) ----------

#[test]
fn target_volt_no_emoji() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let blade = root.join("resources/views/livewire/counter.blade.php");
    write(
        &blade,
        "<?php\nuse function Livewire\\Volt\\state;\nstate(['count' => 0]);\n?>",
    );

    let current = locate("counter", &cfg, LivewireVersion::V4).unwrap();
    let target = compute_target_paths("counter", "published", &current, &cfg).expect("computes");
    assert_eq!(
        target.paths,
        vec![root.join("resources/views/livewire/published.blade.php")]
    );
}

// ---------- is_under_vendor ----------

#[test]
fn detects_vendor_path() {
    let root = Path::new("/proj");
    assert!(is_under_vendor(
        Path::new("/proj/vendor/livewire/livewire/src/Counter.php"),
        root
    ));
}

#[test]
fn rejects_non_vendor_path() {
    let root = Path::new("/proj");
    assert!(!is_under_vendor(
        Path::new("/proj/app/Livewire/Counter.php"),
        root
    ));
}
