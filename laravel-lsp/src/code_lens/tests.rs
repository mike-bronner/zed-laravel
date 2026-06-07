//! Tests for code-lens target extraction (#59).

use super::*;
use crate::salsa_impl::SymbolRefData;
use std::path::Path;

fn keys(targets: &[CodeLensTarget]) -> Vec<(&str, &str)> {
    targets
        .iter()
        .filter_map(|t| match &t.symbol {
            SymbolRefData::MagicMember { fqcn, member } => Some((fqcn.as_str(), member.as_str())),
            _ => None,
        })
        .collect()
}

#[test]
fn model_relationships_scopes_and_properties() {
    let src = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public string $nickname = '';
    public function posts() { return $this->hasMany(Post::class); }
    public function scopeActive($q) { return $q->where('active', true); }
    public function plainHelper() { return 1; }
}
"#;
    let targets = code_lens_targets(Path::new("/proj/app/Models/User.php"), src);
    let k = keys(&targets);
    // Relationship + scope (usage name) + public property — keyed to the FQCN.
    assert!(k.contains(&("App\\Models\\User", "posts")), "got {k:?}");
    assert!(
        k.contains(&("App\\Models\\User", "active")),
        "scope usage name"
    );
    assert!(
        k.contains(&("App\\Models\\User", "nickname")),
        "public property"
    );
    // Plain method (we don't count plain calls) gets no lens.
    assert!(!k.iter().any(|(_, m)| *m == "plainHelper"));
    assert!(!k.iter().any(|(_, m)| *m == "scopeActive"));
}

#[test]
fn scope_lens_anchors_on_the_method_name() {
    let src = r#"<?php
namespace App\Models;
class User extends \Illuminate\Database\Eloquent\Model {
    public function scopeActive($q) {}
}
"#;
    let targets = code_lens_targets(Path::new("/proj/app/Models/User.php"), src);
    let scope = targets
        .iter()
        .find(|t| matches!(&t.symbol, SymbolRefData::MagicMember { member, .. } if member == "active"))
        .expect("scope target");
    // `scopeActive` is on line 3 (0-based), at the method-name column.
    assert_eq!(scope.line, 3);
    let line = src.lines().nth(3).unwrap();
    assert_eq!(scope.column, line.find("scopeActive").unwrap() as u32);
}

#[test]
fn volt_component_computed_and_props() {
    let src = r#"<?php
use Illuminate\Database\Eloquent\Collection;
use Livewire\Volt\Component;
new class extends Component {
    public string $userName = '';
    #[Computed]
    public function entities(): Collection { return Entity::all(); }
    public function saveRole(): void {}
};
"#;
    let path = Path::new("/proj/resources/views/pages/permissions.php");
    let targets = code_lens_targets(path, src);
    let key = format!("volt::{}", path.display());
    let k = keys(&targets);
    // Computed method + public property → component key. Action method skipped.
    assert!(k.contains(&(key.as_str(), "entities")), "got {k:?}");
    assert!(k.contains(&(key.as_str(), "userName")), "public prop");
    assert!(
        !k.iter().any(|(_, m)| *m == "saveRole"),
        "action method skipped"
    );
}

#[test]
fn mfc_template_has_no_lenses() {
    // A plain Blade template (no Volt front-matter, no class) yields no lenses;
    // the component members are lensed on the sibling `.php`.
    let src = "<div>\n    @foreach ($this->entities as $e)\n        {{ $e->name }}\n    @endforeach\n</div>\n";
    let targets = code_lens_targets(Path::new("/proj/x/permissions.blade.php"), src);
    assert!(targets.is_empty(), "got {targets:?}");
}

#[test]
fn plain_controller_yields_no_magic_lenses() {
    let src = r#"<?php
namespace App\Http\Controllers;
class UserController {
    public function index() { return 1; }
}
"#;
    let targets = code_lens_targets(
        Path::new("/proj/app/Http/Controllers/UserController.php"),
        src,
    );
    // No relationships/scopes/public-props → no lenses (plain method skipped).
    assert!(targets.is_empty(), "got {targets:?}");
}

#[test]
fn model_accessors_old_and_new_style() {
    let src = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Casts\Attribute;
class User extends \Illuminate\Database\Eloquent\Model {
    public function getFullNameAttribute() { return $this->first.' '.$this->last; }
    public function displayName(): Attribute { return Attribute::make(fn () => $this->name); }
}
"#;
    let targets = code_lens_targets(Path::new("/proj/app/Models/User.php"), src);
    let k = keys(&targets);
    // Old-style getFullNameAttribute → `full_name`; new-style displayName(): Attribute → `display_name`.
    assert!(
        k.contains(&("App\\Models\\User", "full_name")),
        "old-style accessor, got {k:?}"
    );
    assert!(
        k.contains(&("App\\Models\\User", "display_name")),
        "new-style accessor, got {k:?}"
    );
    // The lens anchors on the method name, not the derived attribute.
    let acc = targets
        .iter()
        .find(|t| matches!(&t.symbol, SymbolRefData::MagicMember { member, .. } if member == "full_name"))
        .unwrap();
    let line = src.lines().nth(acc.line as usize).unwrap();
    assert_eq!(
        acc.column,
        line.find("getFullNameAttribute").unwrap() as u32
    );
}
