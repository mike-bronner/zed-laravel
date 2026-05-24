use super::*;
use std::path::PathBuf;

#[test]
fn parses_php_file_view_calls() {
    let path = PathBuf::from("/fixture/app/Http/Controllers/HomeController.php");
    let src = r#"<?php
class HomeController {
    public function index() {
        return view('home');
    }
}
"#;
    let data = parse_owned(&path, src);
    let names: Vec<String> = data.views.iter().map(|v| v.name.clone()).collect();
    assert!(
        names.contains(&"home".to_string()),
        "expected view 'home', got {:?}",
        names
    );
}

#[test]
fn parses_php_file_route_calls() {
    let path = PathBuf::from("/fixture/routes/web.php");
    let src = r#"<?php
Route::get('/', [HomeController::class, 'index'])->name('home');
$url = route('home');
"#;
    let data = parse_owned(&path, src);
    let names: Vec<String> = data.route_refs.iter().map(|r| r.name.clone()).collect();
    assert!(
        names.contains(&"home".to_string()),
        "expected route 'home' call site, got {:?}",
        names
    );
}

#[test]
fn parses_blade_file_components() {
    let path = PathBuf::from("/fixture/resources/views/layout.blade.php");
    let src = r#"<div>
    <x-button>Click me</x-button>
</div>
"#;
    let data = parse_owned(&path, src);
    let names: Vec<String> = data.components.iter().map(|c| c.name.clone()).collect();
    assert!(
        names.contains(&"button".to_string()),
        "expected component 'button', got {:?}",
        names
    );
}

#[test]
fn parses_blade_file_route_calls_in_echo() {
    let path = PathBuf::from("/fixture/resources/views/nav.blade.php");
    let src = r#"<nav>
    <a href="{{ route('home') }}">Home</a>
    <a href="{{ route('users.index') }}">Users</a>
</nav>
"#;
    let data = parse_owned(&path, src);
    let names: Vec<String> = data.route_refs.iter().map(|r| r.name.clone()).collect();
    assert!(
        names.contains(&"home".to_string()),
        "expected 'home' from {{ route('home') }}, got {:?}",
        names
    );
    assert!(
        names.contains(&"users.index".to_string()),
        "expected 'users.index', got {:?}",
        names
    );
}

#[test]
fn parses_blade_file_php_block() {
    let path = PathBuf::from("/fixture/resources/views/dashboard.blade.php");
    let src = r#"@php
    $url = route('home');
    $title = config('app.name');
@endphp
<h1>{{ $title }}</h1>
"#;
    let data = parse_owned(&path, src);
    let route_names: Vec<String> = data.route_refs.iter().map(|r| r.name.clone()).collect();
    let config_keys: Vec<String> = data.config_refs.iter().map(|c| c.key.clone()).collect();
    assert!(
        route_names.contains(&"home".to_string()),
        "expected 'home' route in @php block, got {:?}",
        route_names
    );
    assert!(
        config_keys.contains(&"app.name".to_string()),
        "expected 'app.name' config in @php block, got {:?}",
        config_keys
    );
}

#[test]
fn builds_position_index_for_find_at_position() {
    let path = PathBuf::from("/fixture/routes/web.php");
    let src = r#"<?php
$url = route('home');
"#;
    let data = parse_owned(&path, src);
    // The route 'home' starts at line 1, after `route('` (which is 7 chars).
    let line_text = src.lines().nth(1).unwrap();
    let start_col = line_text.find("home").unwrap() as u32;
    let pat = data
        .find_at_position(1, start_col + 1)
        .expect("find_at_position should locate the route");
    match pat {
        crate::salsa_impl::PatternAtPosition::Route(r) => {
            assert_eq!(r.name, "home");
        }
        other => panic!("expected Route pattern, got {:?}", other),
    }
}

#[test]
fn returns_empty_for_unparseable_garbage() {
    let path = PathBuf::from("/fixture/garbage.php");
    let data = parse_owned(&path, "this is not valid PHP at all <<>>");
    // tree-sitter is error-tolerant; expect no captured patterns from garbage.
    assert!(data.views.is_empty());
    assert!(data.route_refs.is_empty());
    assert!(data.config_refs.is_empty());
}
