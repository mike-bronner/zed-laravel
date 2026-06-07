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
