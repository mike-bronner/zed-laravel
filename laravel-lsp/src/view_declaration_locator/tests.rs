use super::*;
use crate::salsa_impl::LaravelConfigData;
use std::collections::HashMap;
use std::fs;
use tempfile::TempDir;

fn config_for(root: &Path) -> LaravelConfigData {
    LaravelConfigData {
        root: root.to_path_buf(),
        view_paths: vec![root.join("resources/views")],
        component_paths: vec![],
        livewire_path: None,
        has_livewire: false,
        view_namespaces: HashMap::new(),
        component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
    }
}

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

// ---------- validate_view_name ----------

#[test]
fn validates_simple_name() {
    assert!(validate_view_name("welcome").is_ok());
}

#[test]
fn validates_nested_dotted_name() {
    assert!(validate_view_name("users.profile.edit").is_ok());
}

#[test]
fn validates_kebab_segments() {
    assert!(validate_view_name("admin.user-list").is_ok());
}

#[test]
fn validates_snake_segments() {
    assert!(validate_view_name("admin.user_list").is_ok());
}

#[test]
fn rejects_empty() {
    assert_eq!(validate_view_name(""), Err(ViewNameError::Empty));
    assert_eq!(validate_view_name("   "), Err(ViewNameError::Empty));
}

#[test]
fn rejects_forward_slash() {
    assert_eq!(
        validate_view_name("users/profile"),
        Err(ViewNameError::ContainsSlash)
    );
}

#[test]
fn rejects_backslash() {
    assert_eq!(
        validate_view_name("users\\profile"),
        Err(ViewNameError::ContainsSlash)
    );
}

#[test]
fn rejects_leading_dot() {
    assert_eq!(
        validate_view_name(".users"),
        Err(ViewNameError::EmptySegment)
    );
}

#[test]
fn rejects_trailing_dot() {
    assert_eq!(
        validate_view_name("users."),
        Err(ViewNameError::EmptySegment)
    );
}

#[test]
fn rejects_double_dot_segment_break() {
    // "users..profile" splits to ["users", "", "profile"] — empty middle
    // segment, caught before we ever look at the chars.
    assert_eq!(
        validate_view_name("users..profile"),
        Err(ViewNameError::EmptySegment)
    );
}

#[test]
fn rejects_blade_php_extension() {
    assert_eq!(
        validate_view_name("users.profile.blade.php"),
        Err(ViewNameError::HasExtension)
    );
}

#[test]
fn rejects_blade_only_extension() {
    assert_eq!(
        validate_view_name("users.profile.blade"),
        Err(ViewNameError::HasExtension)
    );
}

#[test]
fn rejects_php_only_extension() {
    assert_eq!(
        validate_view_name("users.profile.php"),
        Err(ViewNameError::HasExtension)
    );
}

#[test]
fn rejects_space_in_segment() {
    assert_eq!(
        validate_view_name("users profile"),
        Err(ViewNameError::InvalidCharacter(' '))
    );
}

#[test]
fn rejects_special_character_in_segment() {
    assert_eq!(
        validate_view_name("users@profile"),
        Err(ViewNameError::InvalidCharacter('@'))
    );
}

#[test]
fn rejects_colon_segment() {
    assert_eq!(
        validate_view_name("users:profile"),
        Err(ViewNameError::InvalidCharacter(':'))
    );
}

#[test]
fn trims_whitespace_before_validation() {
    assert!(validate_view_name("  users.profile  ").is_ok());
}

#[test]
fn error_messages_are_user_friendly() {
    // Sanity check the actual text the user will see.
    assert!(ViewNameError::Empty.message().contains("cannot be empty"));
    assert!(ViewNameError::ContainsSlash.message().contains("dots"));
    assert!(ViewNameError::HasExtension.message().contains("extension"));
    assert!(ViewNameError::InvalidCharacter('@').message().contains('@'));
}

// ---------- locate_view_file ----------

#[test]
fn locates_top_level_view() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let path = root.join("resources/views/welcome.blade.php");
    write(&path, "<div>welcome</div>");

    assert_eq!(locate_view_file("welcome", &cfg), Some(path));
}

#[test]
fn locates_nested_view() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let path = root.join("resources/views/users/profile.blade.php");
    write(&path, "<div>profile</div>");

    assert_eq!(locate_view_file("users.profile", &cfg), Some(path));
}

#[test]
fn returns_none_when_view_missing_on_disk() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    assert_eq!(locate_view_file("nonexistent", &cfg), None);
}

// ---------- compute_target_path ----------

#[test]
fn target_path_same_directory_rename() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let current = root.join("resources/views/users/index.blade.php");
    write(&current, "");

    let target =
        compute_target_path("users.index", "users.profile", &current, &cfg).expect("computes");
    assert_eq!(target, root.join("resources/views/users/profile.blade.php"));
}

#[test]
fn target_path_cross_directory_rename() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let current = root.join("resources/views/users/index.blade.php");
    write(&current, "");

    let target =
        compute_target_path("users.index", "admin.users.index", &current, &cfg).expect("computes");
    assert_eq!(
        target,
        root.join("resources/views/admin/users/index.blade.php")
    );
}

#[test]
fn target_path_returns_none_for_unknown_current_path() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    // current_path doesn't match any candidate the config would emit for
    // the old name — we refuse rather than guess where to put the target.
    let current = root.join("some/random/place.blade.php");

    assert_eq!(
        compute_target_path("users.index", "users.profile", &current, &cfg),
        None
    );
}

// ---------- is_under_vendor ----------

#[test]
fn detects_vendor_path() {
    let root = Path::new("/project");
    assert!(is_under_vendor(
        Path::new("/project/vendor/laravel/framework/views/x.blade.php"),
        root
    ));
}

#[test]
fn rejects_non_vendor_path() {
    let root = Path::new("/project");
    assert!(!is_under_vendor(
        Path::new("/project/resources/views/x.blade.php"),
        root
    ));
}

#[test]
fn rejects_vendor_in_different_root() {
    // A `vendor/` directory that isn't the *project's* vendor dir
    // shouldn't trip the check.
    let root = Path::new("/project");
    assert!(!is_under_vendor(Path::new("/other/vendor/x"), root));
}
