//! Tests for controller → Blade view-variable extraction.
//!
//! Variable types here resolve via flow tracking on a typed parameter, so no
//! on-disk model is needed — `view_renders_in_file` returns the view name and
//! the `var → fqcn` map for each render site.

use super::*;
use crate::class_hierarchy_index::ClassHierarchyIndex;
use std::path::Path;

fn renders(controller: &str) -> Vec<ViewRender> {
    view_renders_in_file(
        controller,
        &ClassHierarchyIndex::default(),
        &mut ClassViewCache::new(),
        Path::new("/proj"),
    )
}

const CTRL_HEADER: &str = "<?php
namespace App\\Http\\Controllers;
use App\\Models\\User;
class C {
    public function show(User $user) {
";

#[test]
fn extracts_array_data() {
    let src =
        format!("{CTRL_HEADER}        return view('users.show', ['user' => $user]);\n    }}\n}}\n");
    let r = renders(&src);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].view_name, "users.show");
    assert_eq!(
        r[0].vars.get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn extracts_compact() {
    let src =
        format!("{CTRL_HEADER}        return view('users.show', compact('user'));\n    }}\n}}\n");
    let r = renders(&src);
    assert_eq!(r.len(), 1);
    assert_eq!(
        r[0].vars.get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn extracts_with_key_value() {
    let src = format!(
        "{CTRL_HEADER}        return view('users.show')->with('user', $user);\n    }}\n}}\n"
    );
    let r = renders(&src);
    assert_eq!(r.len(), 1, "got {r:?}");
    assert_eq!(r[0].view_name, "users.show");
    assert_eq!(
        r[0].vars.get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn extracts_with_array() {
    let src = format!(
        "{CTRL_HEADER}        return view('users.show')->with(['user' => $user]);\n    }}\n}}\n"
    );
    let r = renders(&src);
    assert_eq!(r.len(), 1);
    assert_eq!(
        r[0].vars.get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn unresolvable_value_is_omitted() {
    // `$mystery` has no type info → the var simply doesn't appear (vs. a wrong
    // guess). The view render is still recorded.
    let src = "<?php
function show($mystery) {
    return view('x', ['thing' => $mystery]);
}
";
    let r = renders(src);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].view_name, "x");
    assert!(r[0].vars.is_empty(), "got {:?}", r[0].vars);
}

#[test]
fn no_view_calls_yields_empty() {
    let r = renders("<?php\nfunction f() { return 1; }\n");
    assert!(r.is_empty());
}

// ---- ViewVarIndex --------------------------------------------------------

use std::collections::HashMap;
use std::path::PathBuf;

fn render(view: &str, vars: &[(&str, &str)]) -> ViewRender {
    ViewRender {
        view_name: view.to_string(),
        vars: vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<HashMap<_, _>>(),
    }
}

#[test]
fn index_returns_var_type() {
    let mut idx = ViewVarIndex::new();
    idx.insert_file(
        PathBuf::from("/proj/UserController.php"),
        &[render("users.show", &[("user", "App\\Models\\User")])],
    );
    assert_eq!(
        idx.var_types("users.show", "user"),
        vec!["App\\Models\\User"]
    );
    assert!(idx.var_types("users.show", "missing").is_empty());
    assert!(idx.var_types("other.view", "user").is_empty());
}

#[test]
fn index_unions_types_across_files() {
    // Two controllers render the same view with different types for `user`.
    let mut idx = ViewVarIndex::new();
    idx.insert_file(
        PathBuf::from("/proj/UserController.php"),
        &[render("dash", &[("user", "App\\Models\\User")])],
    );
    idx.insert_file(
        PathBuf::from("/proj/AdminController.php"),
        &[render("dash", &[("user", "App\\Models\\Admin")])],
    );
    // Union — both observed types are kept (sorted).
    assert_eq!(
        idx.var_types("dash", "user"),
        vec!["App\\Models\\Admin", "App\\Models\\User"]
    );
}

#[test]
fn index_evicts_on_reinsert() {
    let mut idx = ViewVarIndex::new();
    let path = PathBuf::from("/proj/UserController.php");
    idx.insert_file(path.clone(), &[render("v", &[("a", "App\\A")])]);
    // Re-parse of the same file now renders a different var — old one is gone.
    idx.insert_file(path, &[render("v", &[("b", "App\\B")])]);
    assert!(idx.var_types("v", "a").is_empty());
    assert_eq!(idx.var_types("v", "b"), vec!["App\\B"]);
}

#[test]
fn index_clear_empties() {
    let mut idx = ViewVarIndex::new();
    idx.insert_file(
        PathBuf::from("/proj/C.php"),
        &[render("v", &[("a", "App\\A")])],
    );
    assert!(!idx.is_empty());
    idx.clear();
    assert!(idx.is_empty());
    assert_eq!(idx.view_count(), 0);
}

// ---- view_name_for_path --------------------------------------------------

#[test]
fn view_name_strips_root_and_suffix() {
    let roots = vec![PathBuf::from("/proj/resources/views")];
    assert_eq!(
        view_name_for_path(
            Path::new("/proj/resources/views/users/show.blade.php"),
            &roots
        ),
        Some("users.show".to_string())
    );
    assert_eq!(
        view_name_for_path(Path::new("/proj/resources/views/welcome.blade.php"), &roots),
        Some("welcome".to_string())
    );
}

#[test]
fn view_name_none_outside_roots() {
    let roots = vec![PathBuf::from("/proj/resources/views")];
    assert_eq!(
        view_name_for_path(Path::new("/proj/app/Models/User.php"), &roots),
        None
    );
}

#[test]
fn view_name_longest_root_wins() {
    // A package view root nested under the app's view root should win, yielding
    // the package-relative name rather than the deep app-relative one.
    let roots = vec![
        PathBuf::from("/proj/resources/views"),
        PathBuf::from("/proj/resources/views/vendor/pkg"),
    ];
    assert_eq!(
        view_name_for_path(
            Path::new("/proj/resources/views/vendor/pkg/button.blade.php"),
            &roots
        ),
        Some("button".to_string())
    );
}
