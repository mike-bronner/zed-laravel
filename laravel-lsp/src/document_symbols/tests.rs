//! Tests for document symbol extraction.

use super::*;
use std::path::PathBuf;

// ============================================================================
// File classification
// ============================================================================

#[test]
fn classify_blade_file_by_extension() {
    let path = PathBuf::from("/app/resources/views/users/show.blade.php");
    assert_eq!(classify_file(&path, ""), FileKind::Blade);
}

#[test]
fn classify_route_file_by_directory() {
    let path = PathBuf::from("/app/routes/web.php");
    assert_eq!(classify_file(&path, "<?php\n"), FileKind::RouteFile);
}

#[test]
fn classify_livewire_component_by_content() {
    let path = PathBuf::from("/app/app/Livewire/Counter.php");
    let content = "<?php\nclass Counter extends Component {}\n";
    assert_eq!(classify_file(&path, content), FileKind::LivewireComponent);
}

#[test]
fn classify_eloquent_model_by_content() {
    let path = PathBuf::from("/app/app/Models/User.php");
    let content = "<?php\nclass User extends Model {}\n";
    assert_eq!(classify_file(&path, content), FileKind::EloquentModel);
}

#[test]
fn classify_user_model_with_authenticatable_base() {
    let path = PathBuf::from("/app/app/Models/User.php");
    let content = "<?php\nclass User extends Authenticatable {}\n";
    assert_eq!(classify_file(&path, content), FileKind::EloquentModel);
}

#[test]
fn classify_plain_php_file_as_other() {
    let path = PathBuf::from("/app/app/Helpers/Util.php");
    let content = "<?php\nclass Util {}\n";
    assert_eq!(classify_file(&path, content), FileKind::Other);
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
    assert!(symbols[1].detail.is_none()); // unnamed POST
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
    // Roots: @extends, @section
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
    // @yield landed inside the open section before flush
    assert_eq!(symbols[0].children.len(), 1);
}

// ============================================================================
// Livewire extraction
// ============================================================================

#[test]
fn extracts_livewire_class_with_props_and_methods() {
    let content = r#"<?php

namespace App\Livewire;

use Livewire\Component;

class Counter extends Component
{
    public int $count = 0;
    public string $label = 'clicks';

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
    let symbols = extract_livewire_symbols(content);
    assert_eq!(symbols.len(), 1);
    let class = &symbols[0];
    assert_eq!(class.name, "Counter");
    assert_eq!(class.detail.as_deref(), Some("extends Component"));

    // Only the *public* members should appear.
    let names: Vec<&str> = class.children.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"$count"));
    assert!(names.contains(&"$label"));
    assert!(names.contains(&"increment"));
    assert!(names.contains(&"render"));
    assert!(
        !names.contains(&"helper"),
        "private methods stay off the outline"
    );
}

// ============================================================================
// Model extraction
// ============================================================================

#[test]
fn extracts_model_relationships_and_scopes() {
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
    let symbols = extract_model_symbols(content);
    assert_eq!(symbols.len(), 1);
    let class = &symbols[0];
    assert_eq!(class.name, "Post");

    let names: Vec<&str> = class.children.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"author"), "BelongsTo relationship surfaces");
    assert!(names.contains(&"comments"), "HasMany relationship surfaces");
    assert!(names.contains(&"scopePublished"), "scope method surfaces");
    assert!(
        !names.contains(&"getTitleAttribute"),
        "accessor without relationship return type is filtered"
    );
    assert!(
        !names.contains(&"someUnrelatedMethod"),
        "non-scope, non-relationship methods are filtered"
    );
}

// ============================================================================
// Dispatch via extract_symbols
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
