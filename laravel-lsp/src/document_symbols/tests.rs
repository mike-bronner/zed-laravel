//! Tests for document symbol extraction.

use super::*;
use std::path::PathBuf;

// ============================================================================
// File classification (path-only)
// ============================================================================

#[test]
fn classify_blade_file_by_extension() {
    let path = PathBuf::from("/app/resources/views/users/show.blade.php");
    assert_eq!(classify_file(&path), FileKind::Blade);
}

#[test]
fn classify_route_file_by_directory() {
    let path = PathBuf::from("/app/routes/web.php");
    assert_eq!(classify_file(&path), FileKind::RouteFile);
}

#[test]
fn classify_php_file_as_php_by_extension() {
    let path = PathBuf::from("/app/app/Helpers/Util.php");
    assert_eq!(classify_file(&path), FileKind::Php);
}

#[test]
fn classify_non_php_file_as_other() {
    let path = PathBuf::from("/app/README.md");
    assert_eq!(classify_file(&path), FileKind::Other);
}

// ============================================================================
// Route file extraction
// ============================================================================

#[test]
fn extracts_named_get_route() {
    let content = r#"<?php
Route::get('/users', [UserController::class, 'index'])->name('users.index');
"#;
    let symbols = extract_route_symbols(content);
    assert_eq!(symbols.len(), 1);
    // Route name goes in the label, not detail — Zed's outline panel
    // doesn't render detail (zed-industries/zed#49095).
    assert_eq!(symbols[0].name, "GET /users [name=users.index]");
    assert!(symbols[0].detail.is_none());
}

#[test]
fn extracts_multiple_verbs() {
    let content = r#"<?php
Route::get('/a', fn () => 1)->name('a');
Route::post('/b', fn () => 1);
Route::put('/c', fn () => 1)->name('c');
Route::delete('/d', fn () => 1);
"#;
    let symbols = extract_route_symbols(content);
    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["GET /a [name=a]", "POST /b", "PUT /c [name=c]", "DELETE /d",]
    );
    assert!(symbols.iter().all(|s| s.detail.is_none()));
}

#[test]
fn route_positions_are_zero_based() {
    let content = "<?php\nRoute::get('/x', fn () => 1);\n";
    let symbols = extract_route_symbols(content);
    assert_eq!(symbols[0].start_line, 1, "second line is index 1");
    assert_eq!(symbols[0].start_column, 0, "line starts at column 0");
}

// ============================================================================
// Blade extraction
// ============================================================================

#[test]
fn extracts_section_with_yield_child() {
    let content = r#"@extends('layouts.app')
@section('content')
    @yield('title')
@endsection
"#;
    let symbols = extract_blade_symbols(content);
    assert_eq!(symbols.len(), 2);
    assert_eq!(symbols[0].name, "@extends layouts.app");
    assert_eq!(symbols[1].name, "@section content");
    assert_eq!(symbols[1].children.len(), 1);
    assert_eq!(symbols[1].children[0].name, "@yield title");
}

#[test]
fn extracts_nested_push_inside_section() {
    let content = r#"@section('content')
    @push('scripts')
        <script>console.log('hi');</script>
    @endpush
@endsection
"#;
    let symbols = extract_blade_symbols(content);
    assert_eq!(symbols.len(), 1);
    let section = &symbols[0];
    assert_eq!(section.name, "@section content");
    assert_eq!(section.children.len(), 1);
    assert_eq!(section.children[0].name, "@push scripts");
}

#[test]
fn unclosed_section_still_emits() {
    let content = "@section('content')\n@yield('inner')\n";
    let symbols = extract_blade_symbols(content);
    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0].name, "@section content");
    assert_eq!(symbols[0].children.len(), 1);
}

#[test]
fn extracts_blade_component_tags() {
    let content = r#"<div>
    <x-button>Click me</x-button>
    <x-forms.input name="email" />
    <livewire:counter />
    <flux:icon name="search" />
</div>
"#;
    let symbols = extract_blade_symbols(content);
    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    // `<x-button>` is a container (paired with `</x-button>`); the other
    // three are self-closing leaves. Self-closing tags include the ` />`
    // in their label so the source shape is visible at a glance.
    assert_eq!(
        names,
        vec![
            "<x-button>",
            "<x-forms.input />",
            "<livewire:counter />",
            "<flux:icon />",
        ]
    );
    assert!(
        symbols[0].children.is_empty(),
        "x-button has no nested component children in this test"
    );
}

#[test]
fn paired_component_tag_nests_its_children() {
    let content = r#"<x-card>
    <x-card-header>Title</x-card-header>
    <livewire:card-body />
</x-card>
"#;
    let symbols = extract_blade_symbols(content);
    assert_eq!(symbols.len(), 1);
    let card = &symbols[0];
    assert_eq!(card.name, "<x-card>");
    let child_names: Vec<&str> = card.children.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        child_names,
        vec!["<x-card-header>", "<livewire:card-body />"]
    );
}

#[test]
fn extracts_blade_slot_tags() {
    let content = r#"<x-card>
    <x-slot:header>Title</x-slot:header>
    <x-slot name="footer">Footer</x-slot>
    Body content
</x-card>
"#;
    let symbols = extract_blade_symbols(content);
    // `<x-card>` is now a paired container; the slot tags live inside it
    // as children. Slot tags are rendered as `<x-slot:NAME>` regardless of
    // which syntax form the source used.
    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0].name, "<x-card>");
    let child_names: Vec<&str> = symbols[0]
        .children
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert!(child_names.contains(&"<x-slot:header>"));
    assert!(child_names.contains(&"<x-slot:footer>"));
}

#[test]
fn extracts_blade_props_declaration() {
    let content = r#"@props(['title', 'count' => 0])

<div class="card">{{ $title }} — {{ $count }}</div>
"#;
    let symbols = extract_blade_symbols(content);
    assert_eq!(symbols.len(), 1);
    // We capture the first string argument of @props, which for the
    // common `['title', 'count' => 0]` shape is the first key.
    assert_eq!(symbols[0].name, "@props title");
}

#[test]
fn extracts_blade_includes() {
    let content = r#"@include('partials.header')
@includeIf('partials.banner', ['variant' => 'success'])
@includeWhen($user->isAdmin(), 'partials.admin-nav')
@includeUnless($guest, 'partials.user-nav')
@includeFirst(['custom.layout', 'default.layout'])
"#;
    let symbols = extract_blade_symbols(content);
    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "@include partials.header",
            "@includeIf partials.banner",
            "@includeWhen partials.admin-nav",
            "@includeUnless partials.user-nav",
            "@includeFirst custom.layout",
        ]
    );
}

#[test]
fn blade_outline_for_component_file() {
    // A typical Blade component file: props + slot + child components.
    let content = r#"@props(['title'])

<div class="card">
    <x-slot:header>
        {{ $title }}
    </x-slot:header>

    <x-card-body>
        {{ $slot }}
    </x-card-body>

    <livewire:card-footer :title="$title" />
</div>
"#;
    let symbols = extract_blade_symbols(content);

    // Walk the full tree — component containers nest their content as
    // children, so the livewire tag below `<x-card-body>` is a sibling at
    // the top level, while the slot inside `<x-card-body>`'s scope nests
    // under it (because the close tag is delayed).
    fn collect_names(entries: &[SymbolEntry], out: &mut Vec<String>) {
        for e in entries {
            out.push(e.name.clone());
            collect_names(&e.children, out);
        }
    }
    let mut names = Vec::new();
    collect_names(&symbols, &mut names);

    assert!(names.iter().any(|n| n == "@props title"));
    assert!(names.iter().any(|n| n == "<x-slot:header>"));
    assert!(names.iter().any(|n| n == "<x-card-body>"));
    assert!(names.iter().any(|n| n == "<livewire:card-footer />"));
}

#[test]
fn blade_outline_for_layout_file() {
    // A typical Blade layout file: sections nesting yields and pushes.
    let content = r#"<!DOCTYPE html>
<html>
<head>
    <title>@yield('title', 'Default')</title>
    @stack('head')
</head>
<body>
    @include('partials.nav')

    @section('content')
        @yield('inner')
    @show

    @push('scripts')
        <script>console.log('layout');</script>
    @endpush
</body>
</html>
"#;
    let symbols = extract_blade_symbols(content);

    // Collect names from the whole tree (Top + children) since the layout's
    // `@section` is unclosed and ends up containing the `@push scripts`
    // entry as a child.
    fn collect_names(entries: &[SymbolEntry], out: &mut Vec<String>) {
        for e in entries {
            out.push(e.name.clone());
            collect_names(&e.children, out);
        }
    }
    let mut names = Vec::new();
    collect_names(&symbols, &mut names);

    assert!(names.iter().any(|n| n == "@yield title"));
    assert!(names.iter().any(|n| n == "@stack head"));
    assert!(names.iter().any(|n| n == "@include partials.nav"));
    assert!(names.iter().any(|n| n.starts_with("@section")));
    assert!(names.iter().any(|n| n == "@push scripts"));
}

// ============================================================================
// PHP — intentionally returns empty
// ============================================================================
//
// See module-level docs on document_symbols.rs: most users have a PHP LSP
// (Intelephense / Phpactor / PhpTools) installed, and Zed merges document
// symbol responses across LSPs. Anything we emit for `.php` files appears
// twice in the outline panel. We cede PHP class bodies to the dedicated PHP
// LSP and keep our contribution scoped to Laravel-specific shapes: route
// declarations and Blade templates. PHP structural parsing still exists in
// `crate::php_outline` for potential reuse elsewhere (hover, completions),
// but it's not wired into `extract_symbols`.

// ============================================================================
// Dispatch
// ============================================================================

#[test]
fn extract_symbols_dispatches_to_route_extractor() {
    let content = "<?php\nRoute::get('/x', fn () => 1)->name('x');\n";
    let symbols = extract_symbols(content, FileKind::RouteFile);
    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0].name, "GET /x [name=x]");
}

#[test]
fn extract_symbols_other_returns_empty() {
    let symbols = extract_symbols("any content", FileKind::Other);
    assert!(symbols.is_empty());
}

#[test]
fn extract_symbols_php_returns_empty() {
    // We deliberately don't emit document symbols for `.php` files — see
    // the module-level docs on document_symbols.rs. PHP outline is owned
    // by whichever PHP LSP the user has installed (Intelephense / Phpactor
    // / PhpTools) because Zed merges responses across LSPs and any output
    // from us would render twice in the outline panel.
    let content =
        "<?php\nclass Foo extends Component {\n    public int $a = 0;\n    private $b;\n}\n";
    let symbols = extract_symbols(content, FileKind::Php);
    assert!(symbols.is_empty());
}
