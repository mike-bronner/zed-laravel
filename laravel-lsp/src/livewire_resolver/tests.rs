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

#[test]
fn resolves_v4_sfc_at_top_level() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let path = root.join(format!(
        "resources/views/livewire/{}counter.blade.php",
        naming::LIVEWIRE_EMOJI
    ));
    write(&path, "<?php new class extends Component {}; ?><div></div>");

    let component = resolve_component("counter", &cfg, LivewireVersion::V4).expect("resolves");
    assert_eq!(component.kind, LivewireComponentKind::V4Sfc);
    assert_eq!(component.paths, vec![path]);
}

#[test]
fn resolves_v4_sfc_in_nested_path() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let path = root.join(format!(
        "resources/views/components/admin/{}user-list.blade.php",
        naming::LIVEWIRE_EMOJI
    ));
    write(&path, "<?php new class extends Component {}; ?><div></div>");

    let component =
        resolve_component("admin.user-list", &cfg, LivewireVersion::V4).expect("resolves");
    assert_eq!(component.kind, LivewireComponentKind::V4Sfc);
    assert_eq!(component.paths, vec![path]);
}

#[test]
fn resolves_v4_mfc_with_all_optional_children() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let dir = root.join(format!(
        "resources/views/components/{}counter",
        naming::LIVEWIRE_EMOJI
    ));
    write(&dir.join("counter.php"), "<?php new class {}; ?>");
    write(&dir.join("counter.blade.php"), "<div></div>");
    write(&dir.join("counter.js"), "export {};");
    write(&dir.join("counter.css"), ".counter {}");

    let component = resolve_component("counter", &cfg, LivewireVersion::V4).expect("resolves");
    assert_eq!(component.kind, LivewireComponentKind::V4Mfc);
    // Directory first, then child files in MFC_CHILD_EXTENSIONS order.
    assert_eq!(component.paths[0], dir);
    assert!(component
        .paths
        .iter()
        .any(|p| p.file_name().unwrap() == "counter.php"));
    assert!(component
        .paths
        .iter()
        .any(|p| p.file_name().unwrap() == "counter.blade.php"));
    assert!(component
        .paths
        .iter()
        .any(|p| p.file_name().unwrap() == "counter.js"));
    assert!(component
        .paths
        .iter()
        .any(|p| p.file_name().unwrap() == "counter.css"));
}

#[test]
fn v4_mfc_rejects_directory_without_required_class_file() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    // Directory exists with the view file but NO {leaf}.php — Livewire's
    // MultiFileParser throws on this. The resolver must not classify it as
    // MFC.
    let dir = root.join(format!(
        "resources/views/components/{}counter",
        naming::LIVEWIRE_EMOJI
    ));
    write(&dir.join("counter.blade.php"), "<div></div>");

    assert!(resolve_component("counter", &cfg, LivewireVersion::V4).is_none());
}

#[test]
fn resolves_volt_via_state_call() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let path = root.join("resources/views/livewire/counter.blade.php");
    write(
        &path,
        "<?php\nuse function Livewire\\Volt\\state;\nstate(['count' => 0]);\n?>\n<div></div>",
    );

    let component = resolve_component("counter", &cfg, LivewireVersion::V4).expect("resolves");
    assert_eq!(component.kind, LivewireComponentKind::Volt);
    assert_eq!(component.paths, vec![path]);
}

#[test]
fn resolves_volt_via_class_extends() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let path = root.join("resources/views/livewire/counter.blade.php");
    write(
        &path,
        "<?php\nuse Livewire\\Volt\\Component;\nnew class extends Component {};\n?>\n<div></div>",
    );

    let component = resolve_component("counter", &cfg, LivewireVersion::V4).expect("resolves");
    assert_eq!(component.kind, LivewireComponentKind::Volt);
}

#[test]
fn plain_blade_file_without_volt_signature_isnt_volt() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    // Bare .blade.php with no PHP front-matter at all — just template. The
    // resolver should NOT classify this as Volt (no signature) and should
    // fall through past every check, returning None.
    let path = root.join("resources/views/livewire/counter.blade.php");
    write(&path, "<div>no php here</div>");

    assert!(resolve_component("counter", &cfg, LivewireVersion::V4).is_none());
}

#[test]
fn resolves_v3_class_based() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let class_file = root.join("app/Livewire/Counter.php");
    write(&class_file, "<?php class Counter extends Component {}");

    let component = resolve_component("counter", &cfg, LivewireVersion::V3).expect("resolves");
    assert_eq!(component.kind, LivewireComponentKind::V3Class);
    assert_eq!(component.paths, vec![class_file]);
}

#[test]
fn resolves_v3_class_with_companion_view() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let class_file = root.join("app/Livewire/Admin/UserList.php");
    write(&class_file, "<?php class UserList extends Component {}");
    let view_file = root.join("resources/views/livewire/admin/user-list.blade.php");
    write(&view_file, "<div></div>");

    let component =
        resolve_component("admin.user-list", &cfg, LivewireVersion::V3).expect("resolves");
    assert_eq!(component.kind, LivewireComponentKind::V3Class);
    assert_eq!(component.paths.len(), 2);
    assert_eq!(component.paths[0], class_file);
    assert_eq!(component.paths[1], view_file);
}

#[test]
fn v3_project_skips_v4_formats() {
    // Even if v4-shaped files exist on disk, a v3 project only resolves
    // via class-based lookup. Documents the behavior so a v4 fixture
    // sneaking into a v3 project doesn't get a false-positive resolution.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let v4_sfc_path = root.join(format!(
        "resources/views/livewire/{}counter.blade.php",
        naming::LIVEWIRE_EMOJI
    ));
    write(&v4_sfc_path, "<?php new class extends Component {}; ?>");

    assert!(resolve_component("counter", &cfg, LivewireVersion::V3).is_none());
}

#[test]
fn resolves_namespaced_component() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let path = root.join(format!(
        "resources/views/pages/{}dashboard.blade.php",
        naming::LIVEWIRE_EMOJI
    ));
    write(&path, "<?php new class extends Component {}; ?>");

    let component =
        resolve_component("pages::dashboard", &cfg, LivewireVersion::V4).expect("resolves");
    assert_eq!(component.kind, LivewireComponentKind::V4Sfc);
    assert_eq!(component.paths, vec![path]);
}

#[test]
fn namespaced_lookup_against_unknown_namespace_returns_none() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    // 'billing' namespace isn't in the defaults map, and we don't fall
    // through to component_locations for namespaced names.
    assert!(resolve_component("billing::invoice", &cfg, LivewireVersion::V4).is_none());
}

#[test]
fn sfc_preferred_over_v3_class_when_both_exist() {
    // V4-first projects sometimes have stale v3-style class files lying
    // around. The resolver picks the v4 SFC because that's what Livewire
    // discovery does at runtime.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let sfc_path = root.join(format!(
        "resources/views/livewire/{}counter.blade.php",
        naming::LIVEWIRE_EMOJI
    ));
    write(&sfc_path, "<?php new class extends Component {}; ?>");
    let class_file = root.join("app/Livewire/Counter.php");
    write(&class_file, "<?php class Counter extends Component {}");

    let component = resolve_component("counter", &cfg, LivewireVersion::V4).expect("resolves");
    assert_eq!(component.kind, LivewireComponentKind::V4Sfc);
}

#[test]
fn returns_none_for_missing_component() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    assert!(resolve_component("does.not.exist", &cfg, LivewireVersion::V4).is_none());
}

#[test]
fn empty_leaf_returns_none() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    // Trailing dot → empty leaf segment. Defensive: don't crash, return None.
    assert!(resolve_component("admin.", &cfg, LivewireVersion::V4).is_none());
}

#[test]
fn empty_name_returns_none() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    assert!(resolve_component("", &cfg, LivewireVersion::V4).is_none());
}

#[test]
fn unknown_version_tries_v4_first() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    // Unknown is conservative — tries all formats. A v4 SFC on disk should
    // still resolve as V4Sfc, not fall through to v3 class lookup.
    let path = root.join(format!(
        "resources/views/livewire/{}counter.blade.php",
        naming::LIVEWIRE_EMOJI
    ));
    write(&path, "<?php new class extends Component {}; ?>");

    let component = resolve_component("counter", &cfg, LivewireVersion::Unknown).expect("resolves");
    assert_eq!(component.kind, LivewireComponentKind::V4Sfc);
}
