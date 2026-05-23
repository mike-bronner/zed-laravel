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
    assert_eq!(symbols[0].name, "GET /users");
    assert_eq!(symbols[0].detail.as_deref(), Some("[name=users.index]"));
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
    assert_eq!(names, vec!["GET /a", "POST /b", "PUT /c", "DELETE /d"]);
    assert_eq!(symbols[0].detail.as_deref(), Some("[name=a]"));
    assert!(symbols[1].detail.is_none());
    assert_eq!(symbols[2].detail.as_deref(), Some("[name=c]"));
    assert!(symbols[3].detail.is_none());
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
    assert_eq!(symbols[0].name, "layouts.app");
    assert_eq!(symbols[0].detail.as_deref(), Some("@extends"));
    assert_eq!(symbols[1].name, "content");
    assert_eq!(symbols[1].detail.as_deref(), Some("@section"));
    assert_eq!(symbols[1].children.len(), 1);
    assert_eq!(symbols[1].children[0].name, "title");
    assert_eq!(symbols[1].children[0].detail.as_deref(), Some("@yield"));
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
    assert_eq!(section.name, "content");
    assert_eq!(section.children.len(), 1);
    assert_eq!(section.children[0].name, "scripts");
    assert_eq!(section.children[0].detail.as_deref(), Some("@push"));
}

#[test]
fn unclosed_section_still_emits() {
    let content = "@section('content')\n@yield('inner')\n";
    let symbols = extract_blade_symbols(content);
    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0].name, "content");
    assert_eq!(symbols[0].children.len(), 1);
}

// ============================================================================
// PHP — Livewire components (public-only filter)
// ============================================================================

#[test]
fn livewire_component_shows_only_public_members() {
    let content = r#"<?php

namespace App\Livewire;

use Livewire\Component;

class Counter extends Component
{
    public int $count = 0;
    public string $label = 'clicks';
    private $internal;

    public function increment(): void
    {
        $this->count++;
    }

    private function helper(): void
    {
        // private — should NOT appear in outline
    }

    public function render()
    {
        return view('livewire.counter');
    }
}
"#;
    let symbols = extract_php_symbols(content);
    assert_eq!(symbols.len(), 1);
    let class = &symbols[0];
    assert_eq!(class.name, "Counter");
    assert_eq!(class.kind, SymbolEntryKind::Class);
    assert_eq!(class.detail.as_deref(), Some("extends Component"));

    let names: Vec<&str> = class.children.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"$count"));
    assert!(names.contains(&"$label"));
    assert!(
        !names.contains(&"$internal"),
        "private props stay off the outline"
    );
    assert!(names.contains(&"increment"));
    assert!(names.contains(&"render"));
    assert!(
        !names.contains(&"helper"),
        "private methods stay off the outline"
    );
}

// ============================================================================
// PHP — Eloquent models (relationships + scopes only)
// ============================================================================

#[test]
fn model_shows_only_relationships_and_scopes() {
    let content = r#"<?php

namespace App\Models;

use Illuminate\Database\Eloquent\Model;
use Illuminate\Database\Eloquent\Relations\HasMany;
use Illuminate\Database\Eloquent\Relations\BelongsTo;

class Post extends Model
{
    public function author(): BelongsTo
    {
        return $this->belongsTo(User::class);
    }

    public function comments(): HasMany
    {
        return $this->hasMany(Comment::class);
    }

    public function scopePublished($query)
    {
        return $query->whereNotNull('published_at');
    }

    public function getTitleAttribute(): string
    {
        return $this->attributes['title'];
    }

    public function someUnrelatedMethod(): void
    {
        // Not a relationship, not a scope — stays off the outline.
    }
}
"#;
    let symbols = extract_php_symbols(content);
    assert_eq!(symbols.len(), 1);
    let class = &symbols[0];
    assert_eq!(class.name, "Post");

    let names: Vec<&str> = class.children.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"author"));
    assert!(names.contains(&"comments"));
    assert!(names.contains(&"scopePublished"));
    assert!(
        !names.contains(&"getTitleAttribute"),
        "accessor without relationship return type is filtered"
    );
    assert!(
        !names.contains(&"someUnrelatedMethod"),
        "non-scope, non-relationship methods are filtered"
    );
}

#[test]
fn user_model_with_authenticatable_base_is_recognised() {
    let content = r#"<?php
class User extends Authenticatable
{
    public function posts(): HasMany
    {
        return $this->hasMany(Post::class);
    }
}
"#;
    let symbols = extract_php_symbols(content);
    assert_eq!(symbols.len(), 1);
    let names: Vec<&str> = symbols[0]
        .children
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(names, vec!["posts"]);
}

// ============================================================================
// PHP — Generic (controllers, jobs, helpers, interfaces, etc.)
// ============================================================================

#[test]
fn generic_php_class_shows_all_visibilities() {
    let content = r#"<?php

namespace App\Http\Controllers;

class UserController
{
    public function __construct(private UserRepo $repo) {}

    public function index() {
        return view('users.index');
    }

    public function store(Request $request) {
        // ...
    }

    protected function authorize(): void {}

    private function buildQuery() {}
}
"#;
    let symbols = extract_php_symbols(content);
    assert_eq!(symbols.len(), 1);
    let class = &symbols[0];
    assert_eq!(class.name, "UserController");

    let names: Vec<&str> = class.children.iter().map(|c| c.name.as_str()).collect();
    // All methods regardless of visibility — strict-upgrade behaviour vs.
    // tree-sitter outline.
    assert!(names.contains(&"__construct"));
    assert!(names.contains(&"index"));
    assert!(names.contains(&"store"));
    assert!(names.contains(&"authorize"));
    assert!(names.contains(&"buildQuery"));
}

#[test]
fn generic_php_emits_multiple_top_level_structures() {
    let content = r#"<?php
class Foo {}
interface Bar {}
trait Baz {}
enum Qux {}
"#;
    let symbols = extract_php_symbols(content);
    assert_eq!(symbols.len(), 4);
    assert_eq!(symbols[0].name, "Foo");
    assert_eq!(symbols[0].kind, SymbolEntryKind::Class);
    assert_eq!(symbols[1].name, "Bar");
    assert_eq!(symbols[1].kind, SymbolEntryKind::Interface);
    assert_eq!(symbols[2].name, "Baz");
    assert_eq!(symbols[2].kind, SymbolEntryKind::Trait);
    assert_eq!(symbols[3].name, "Qux");
    assert_eq!(symbols[3].kind, SymbolEntryKind::Enum);
}

#[test]
fn generic_php_emits_free_functions() {
    let content = r#"<?php

if (!function_exists('format_money')) {
    function format_money(int $cents): string {
        return '$' . number_format($cents / 100, 2);
    }
}

function legacy_helper(): void {}
"#;
    let symbols = extract_php_symbols(content);
    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"format_money"));
    assert!(names.contains(&"legacy_helper"));
}

#[test]
fn generic_php_class_without_extends_uses_generic_path() {
    // No `extends Component` / `extends Model` — should go through generic
    // path, not be misclassified as Livewire or Eloquent.
    let content = r#"<?php
class PlainOldClass {
    public function hello(): string {
        return 'hi';
    }
    private function secret() {}
}
"#;
    let symbols = extract_php_symbols(content);
    assert_eq!(symbols.len(), 1);
    let names: Vec<&str> = symbols[0]
        .children
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert!(names.contains(&"hello"));
    assert!(
        names.contains(&"secret"),
        "private members visible in generic outline"
    );
}

// ============================================================================
// Dispatch
// ============================================================================

#[test]
fn extract_symbols_dispatches_to_route_extractor() {
    let content = "<?php\nRoute::get('/x', fn () => 1)->name('x');\n";
    let symbols = extract_symbols(content, FileKind::RouteFile);
    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0].name, "GET /x");
}

#[test]
fn extract_symbols_other_returns_empty() {
    let symbols = extract_symbols("any content", FileKind::Other);
    assert!(symbols.is_empty());
}

#[test]
fn extract_symbols_php_dispatches_through_subclassification() {
    // PHP file with Livewire component — should produce a class symbol with
    // the public-only filter applied.
    let content =
        "<?php\nclass Foo extends Component {\n    public int $a = 0;\n    private $b;\n}\n";
    let symbols = extract_symbols(content, FileKind::Php);
    assert_eq!(symbols.len(), 1);
    let names: Vec<&str> = symbols[0]
        .children
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(
        names,
        vec!["$a"],
        "private $b filtered out by Livewire path"
    );
}
