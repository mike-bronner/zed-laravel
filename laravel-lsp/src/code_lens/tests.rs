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

// ── Route-name declaration lenses ─────────────────────────────────────────

use crate::route_name_locator::RouteNameDeclaration;

fn decl(full_name: &str, line: u32, start: u32, end: u32) -> RouteNameDeclaration {
    RouteNameDeclaration {
        full_name: full_name.to_string(),
        local_segment: full_name
            .rsplit('.')
            .next()
            .unwrap_or(full_name)
            .to_string(),
        line,
        start_column: start,
        end_column: end,
    }
}

fn route_names(targets: &[CodeLensTarget]) -> Vec<&str> {
    targets
        .iter()
        .filter_map(|t| match &t.symbol {
            SymbolRefData::Route(name) => Some(name.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn route_lens_no_external_prefix_uses_bare_full_name() {
    let decls = [
        decl("users.index", 10, 42, 53),
        decl("users.show", 11, 42, 52),
    ];
    let targets = route_lens_targets(&decls, &[]);

    assert_eq!(route_names(&targets), vec!["users.index", "users.show"]);
    // Anchors on the name string content, on the declaration line.
    assert_eq!(
        (targets[0].line, targets[0].column, targets[0].end_column),
        (10, 42, 53)
    );
}

#[test]
fn route_lens_applies_external_prefix() {
    let decls = [decl("dashboard", 3, 30, 39)];
    let targets = route_lens_targets(&decls, &["admin.".to_string()]);

    assert_eq!(route_names(&targets), vec!["admin.dashboard"]);
    assert_eq!(targets[0].line, 3);
}

#[test]
fn route_lens_emits_one_target_per_external_prefix() {
    // A file loaded under two prefixes counts each resulting route separately.
    let decls = [decl("index", 5, 20, 25)];
    let targets = route_lens_targets(&decls, &["admin.".to_string(), "staff.".to_string()]);

    assert_eq!(route_names(&targets), vec!["admin.index", "staff.index"]);
    assert!(targets
        .iter()
        .all(|t| t.line == 5 && t.column == 20 && t.end_column == 25));
}

#[test]
fn route_lens_empty_decls_is_empty() {
    assert!(route_lens_targets(&[], &["admin.".to_string()]).is_empty());
}

// ── Env-var declaration lenses ────────────────────────────────────────────

fn env_keys(targets: &[CodeLensTarget]) -> Vec<&str> {
    targets
        .iter()
        .filter_map(|t| match &t.symbol {
            SymbolRefData::Env(name) => Some(name.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn env_lens_targets_one_per_key_with_positions() {
    let src = "# config\nAPP_NAME=Laravel\nDB_HOST=127.0.0.1\n";
    let targets = env_lens_targets(src);
    assert_eq!(env_keys(&targets), vec!["APP_NAME", "DB_HOST"]);
    // Anchors on the key text.
    assert_eq!(
        (targets[0].line, targets[0].column, targets[0].end_column),
        (1, 0, 8)
    );
}

#[test]
fn env_lens_targets_empty_for_no_declarations() {
    assert!(env_lens_targets("# only comments\n\nJUST_TEXT\n").is_empty());
}

// ── Config / translation key lenses ───────────────────────────────────────

fn config_keys(targets: &[CodeLensTarget]) -> Vec<&str> {
    targets
        .iter()
        .filter_map(|t| match &t.symbol {
            SymbolRefData::Config(k) => Some(k.as_str()),
            _ => None,
        })
        .collect()
}

fn translation_keys(targets: &[CodeLensTarget]) -> Vec<&str> {
    targets
        .iter()
        .filter_map(|t| match &t.symbol {
            SymbolRefData::Translation(k) => Some(k.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn config_lens_targets_prefix_file_stem_and_nest() {
    let src = r#"<?php
return [
    'default' => 'mysql',
    'connections' => [
        'mysql' => ['host' => '127.0.0.1'],
    ],
];
"#;
    let targets = config_lens_targets("database", src);
    assert_eq!(
        config_keys(&targets),
        vec![
            "database.default",
            "database.connections",
            "database.connections.mysql",
            "database.connections.mysql.host",
        ]
    );
}

#[test]
fn translation_lens_targets_prefix_file_stem() {
    let src = r#"<?php
return [
    'failed' => 'These credentials do not match our records.',
    'throttle' => 'Too many login attempts.',
];
"#;
    let targets = translation_lens_targets("auth", src);
    assert_eq!(
        translation_keys(&targets),
        vec!["auth.failed", "auth.throttle"]
    );
    assert_eq!((targets[0].line, targets[0].column > 0), (2, true));
}

#[test]
fn dotted_key_lens_targets_empty_for_non_array() {
    assert!(config_lens_targets("app", "<?php // nothing").is_empty());
    assert!(translation_lens_targets("auth", "<?php // nothing").is_empty());
}

// ── File-level view / component lenses ────────────────────────────────────

use crate::salsa_impl::LaravelConfigData;
use std::collections::HashMap;
use std::path::PathBuf;

fn lens_config(root: &Path) -> LaravelConfigData {
    LaravelConfigData {
        root: root.to_path_buf(),
        view_paths: vec![root.join("resources/views")],
        component_paths: vec![(String::new(), root.join("resources/views/components"))],
        livewire_path: None,
        has_livewire: false,
        view_namespaces: HashMap::new(),
        component_namespaces: HashMap::new(),
        anonymous_component_paths: HashMap::new(),
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
    }
}

#[test]
fn file_level_symbols_component_resolves_both_component_and_view() {
    let root = PathBuf::from("/proj");
    let config = lens_config(&root);
    // A component blade lives under a view root, so it has BOTH identities —
    // both are returned and the caller compounds their reference counts.
    let path = root.join("resources/views/components/forms/input.blade.php");
    let syms = file_level_symbols(&path, &config);
    assert!(syms
        .iter()
        .any(|s| matches!(s, SymbolRefData::Component(n) if n == "forms.input")));
    assert!(syms
        .iter()
        .any(|s| matches!(s, SymbolRefData::View(n) if n == "components.forms.input")));
}

#[test]
fn file_level_symbols_plain_view_is_view_only() {
    let root = PathBuf::from("/proj");
    let config = lens_config(&root);
    let path = root.join("resources/views/users/index.blade.php");
    let syms = file_level_symbols(&path, &config);
    assert_eq!(syms.len(), 1);
    assert!(matches!(&syms[0], SymbolRefData::View(n) if n == "users.index"));
}

#[test]
fn file_level_symbols_empty_for_non_blade() {
    let root = PathBuf::from("/proj");
    let config = lens_config(&root);
    assert!(file_level_symbols(&root.join("app/Models/User.php"), &config).is_empty());
}
