//! Tests for tree-sitter PHP outline extraction.

use super::*;

#[test]
fn empty_input_returns_empty_structure() {
    let s = extract_php_structure("");
    assert!(s.structures.is_empty());
    assert!(s.functions.is_empty());
}

#[test]
fn extracts_single_class_with_methods_and_properties() {
    let content = r#"<?php
class User
{
    public int $id;
    protected string $email;
    private $token;

    public function fullName(): string {
        return $this->first . ' ' . $this->last;
    }

    protected function hash(): string {
        return md5($this->token);
    }
}
"#;
    let s = extract_php_structure(content);
    assert_eq!(s.structures.len(), 1);
    let class = &s.structures[0];
    assert_eq!(class.kind, PhpStructureKind::Class);
    assert_eq!(class.name, "User");

    let prop_names: Vec<&str> = class.properties.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(prop_names, vec!["id", "email", "token"]);

    let vis: Vec<PhpVisibility> = class.properties.iter().map(|p| p.visibility).collect();
    assert_eq!(
        vis,
        vec![
            PhpVisibility::Public,
            PhpVisibility::Protected,
            PhpVisibility::Private
        ]
    );

    let method_names: Vec<&str> = class.methods.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(method_names, vec!["fullName", "hash"]);
    assert_eq!(class.methods[0].visibility, PhpVisibility::Public);
    assert_eq!(class.methods[1].visibility, PhpVisibility::Protected);
}

#[test]
fn extracts_extends_clause() {
    let content = r#"<?php
class Counter extends \Livewire\Component {
    public int $count = 0;
}
"#;
    let s = extract_php_structure(content);
    assert_eq!(s.structures.len(), 1);
    assert_eq!(s.structures[0].extends.as_deref(), Some("Component"));
}

#[test]
fn extracts_return_types_with_namespace_stripped() {
    let content = r#"<?php
class Post {
    public function comments(): \Illuminate\Database\Eloquent\Relations\HasMany {
        return $this->hasMany(Comment::class);
    }
}
"#;
    let s = extract_php_structure(content);
    assert_eq!(s.structures.len(), 1);
    let method = &s.structures[0].methods[0];
    assert_eq!(method.name, "comments");
    assert_eq!(method.return_type.as_deref(), Some("HasMany"));
}

#[test]
fn extracts_nullable_return_type() {
    let content = r#"<?php
class Repo {
    public function find(): ?User {
        return null;
    }
}
"#;
    let s = extract_php_structure(content);
    let m = &s.structures[0].methods[0];
    assert_eq!(m.return_type.as_deref(), Some("?User"));
}

#[test]
fn extracts_interface() {
    let content = r#"<?php
interface Repository {
    public function find(int $id);
    public function save($entity): void;
}
"#;
    let s = extract_php_structure(content);
    assert_eq!(s.structures.len(), 1);
    let iface = &s.structures[0];
    assert_eq!(iface.kind, PhpStructureKind::Interface);
    assert_eq!(iface.name, "Repository");
    let names: Vec<&str> = iface.methods.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, vec!["find", "save"]);
}

#[test]
fn extracts_trait() {
    let content = r#"<?php
trait HasUuid {
    protected string $uuid;
    public function uuid(): string {
        return $this->uuid;
    }
}
"#;
    let s = extract_php_structure(content);
    assert_eq!(s.structures.len(), 1);
    assert_eq!(s.structures[0].kind, PhpStructureKind::Trait);
    assert_eq!(s.structures[0].name, "HasUuid");
}

#[test]
fn extracts_enum() {
    let content = r#"<?php
enum Status: string {
    case Active = 'active';
    case Inactive = 'inactive';

    public function label(): string {
        return match ($this) {
            Status::Active => 'Active',
            Status::Inactive => 'Inactive',
        };
    }
}
"#;
    let s = extract_php_structure(content);
    assert_eq!(s.structures.len(), 1);
    let e = &s.structures[0];
    assert_eq!(e.kind, PhpStructureKind::Enum);
    assert_eq!(e.name, "Status");
    let method_names: Vec<&str> = e.methods.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(method_names, vec!["label"]);
}

#[test]
fn extracts_multiple_top_level_structures() {
    let content = r#"<?php
class Foo {}
interface Bar {}
trait Baz {}
"#;
    let s = extract_php_structure(content);
    assert_eq!(s.structures.len(), 3);
    assert_eq!(s.structures[0].name, "Foo");
    assert_eq!(s.structures[0].kind, PhpStructureKind::Class);
    assert_eq!(s.structures[1].name, "Bar");
    assert_eq!(s.structures[1].kind, PhpStructureKind::Interface);
    assert_eq!(s.structures[2].name, "Baz");
    assert_eq!(s.structures[2].kind, PhpStructureKind::Trait);
}

#[test]
fn extracts_free_functions() {
    let content = r#"<?php

function helper_one(int $x): int {
    return $x + 1;
}

function helper_two(): void {}
"#;
    let s = extract_php_structure(content);
    assert!(s.structures.is_empty());
    assert_eq!(s.functions.len(), 2);
    assert_eq!(s.functions[0].name, "helper_one");
    assert_eq!(s.functions[0].return_type.as_deref(), Some("int"));
    assert_eq!(s.functions[1].name, "helper_two");
    assert_eq!(s.functions[1].return_type.as_deref(), Some("void"));
}

#[test]
fn class_inside_namespace_is_extracted() {
    let content = r#"<?php
namespace App\Models;

class User {
    public string $email;
}
"#;
    let s = extract_php_structure(content);
    assert_eq!(s.structures.len(), 1);
    assert_eq!(s.structures[0].name, "User");
}

#[test]
fn default_visibility_is_public() {
    // PHP defaults to public for methods and properties without an explicit modifier.
    let content = r#"<?php
class Implicit {
    function doThing() {}
}
"#;
    let s = extract_php_structure(content);
    let m = &s.structures[0].methods[0];
    assert_eq!(m.visibility, PhpVisibility::Public);
}

#[test]
fn positions_are_zero_based() {
    let content = "<?php\nclass Foo {}\n";
    let s = extract_php_structure(content);
    let class = &s.structures[0];
    // `class Foo {}` starts on line 1 (0-based), column 0.
    assert_eq!(class.start_line, 1);
    assert_eq!(class.start_column, 0);
}
