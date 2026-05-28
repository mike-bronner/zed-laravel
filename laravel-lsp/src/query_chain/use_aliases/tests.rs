use super::*;
use crate::parser::parse_php;

fn aliases_for(src: &str) -> UseAliases {
    let wrapped = format!("<?php\n{src}");
    let tree = parse_php(&wrapped).expect("parse");
    extract_use_aliases(&tree, &wrapped)
}

#[allow(dead_code)]
fn dump_tree(src: &str) {
    let wrapped = format!("<?php\n{src}");
    let tree = parse_php(&wrapped).expect("parse");
    fn walk(n: tree_sitter::Node, src: &str, depth: usize) {
        let indent = "  ".repeat(depth);
        let text = if n.child_count() == 0 {
            format!(" :: {:?}", &src[n.start_byte()..n.end_byte()])
        } else {
            String::new()
        };
        eprintln!("{}{}{}", indent, n.kind(), text);
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            walk(ch, src, depth + 1);
        }
    }
    walk(tree.root_node(), &wrapped, 0);
}

#[test]
fn dump_simple_use() {
    // Print the tree for diagnostic purposes — useful when AST shape
    // changes break the alias parser.
    dump_tree("use Illuminate\\Support\\Facades\\DB as Database;");
}

#[test]
fn dump_grouped_use() {
    dump_tree("use App\\Models\\{User, Post as P};");
}

#[test]
fn dump_function_use() {
    dump_tree("use function foo\\bar\\baz;");
}

#[test]
fn no_use_statements_returns_empty() {
    let aliases = aliases_for("DB::table('users');");
    assert!(aliases.is_empty());
}

#[test]
fn flat_use_no_alias_uses_last_segment() {
    let aliases = aliases_for("use Illuminate\\Support\\Facades\\DB;");
    assert_eq!(
        aliases.get("DB").map(String::as_str),
        Some("Illuminate\\Support\\Facades\\DB"),
        "aliases: {:?}",
        aliases
    );
}

#[test]
fn flat_use_with_as_alias() {
    let aliases = aliases_for("use Illuminate\\Support\\Facades\\DB as Database;");
    assert_eq!(
        aliases.get("Database").map(String::as_str),
        Some("Illuminate\\Support\\Facades\\DB")
    );
    assert!(
        !aliases.contains_key("DB"),
        "aliased import shouldn't also bind the bare name"
    );
}

#[test]
fn multiple_independent_imports() {
    let src = r#"use Illuminate\Support\Facades\DB;
use App\Models\User as MyUser;
use App\Models\Post;"#;
    let aliases = aliases_for(src);
    assert_eq!(
        aliases.get("DB").map(String::as_str),
        Some("Illuminate\\Support\\Facades\\DB")
    );
    assert_eq!(
        aliases.get("MyUser").map(String::as_str),
        Some("App\\Models\\User")
    );
    assert_eq!(
        aliases.get("Post").map(String::as_str),
        Some("App\\Models\\Post")
    );
}

#[test]
fn grouped_use_distributes_prefix() {
    let src = r#"use App\Models\{User, Post as P, Comment};"#;
    let aliases = aliases_for(src);
    assert_eq!(
        aliases.get("User").map(String::as_str),
        Some("App\\Models\\User")
    );
    assert_eq!(
        aliases.get("P").map(String::as_str),
        Some("App\\Models\\Post")
    );
    assert_eq!(
        aliases.get("Comment").map(String::as_str),
        Some("App\\Models\\Comment")
    );
}

#[test]
fn function_use_is_ignored() {
    // `use function foo;` doesn't bind a class — chains never receive
    // functions, so we don't track them.
    let src = "use function foo\\bar\\baz;";
    let aliases = aliases_for(src);
    assert!(aliases.is_empty(), "got {:?}", aliases);
}

#[test]
fn const_use_is_ignored() {
    let src = "use const FOO\\BAR;";
    let aliases = aliases_for(src);
    assert!(aliases.is_empty(), "got {:?}", aliases);
}

// ---- resolve_class_name -------------------------------------------------

#[test]
fn resolve_with_no_aliases_returns_input_unchanged() {
    let aliases = UseAliases::new();
    assert_eq!(resolve_class_name("DB", &aliases), "DB");
    assert_eq!(resolve_class_name("\\DB", &aliases), "DB");
    assert_eq!(resolve_class_name("App\\Foo", &aliases), "App\\Foo");
}

#[test]
fn resolve_replaces_leading_segment_with_aliased_fqcn() {
    let mut aliases = UseAliases::new();
    aliases.insert(
        "Database".to_string(),
        "Illuminate\\Support\\Facades\\DB".to_string(),
    );
    assert_eq!(
        resolve_class_name("Database", &aliases),
        "Illuminate\\Support\\Facades\\DB"
    );
}

#[test]
fn resolve_handles_namespaced_alias_use() {
    // `use App\Models as M; M\User::query()` — the `M` segment resolves to
    // `App\Models` and the rest of the path tacks on. (Rare in practice but
    // legal PHP.)
    let mut aliases = UseAliases::new();
    aliases.insert("M".to_string(), "App\\Models".to_string());
    assert_eq!(resolve_class_name("M\\User", &aliases), "App\\Models\\User");
}

#[test]
fn resolve_is_case_insensitive_on_alias_head() {
    // PHP class names are case-insensitive. `db::table()` should resolve
    // via the `DB` alias.
    let mut aliases = UseAliases::new();
    aliases.insert(
        "DB".to_string(),
        "Illuminate\\Support\\Facades\\DB".to_string(),
    );
    assert_eq!(
        resolve_class_name("db", &aliases),
        "Illuminate\\Support\\Facades\\DB"
    );
}

#[test]
fn resolve_strips_leading_backslash() {
    let aliases = UseAliases::new();
    assert_eq!(
        resolve_class_name("\\Illuminate\\Support\\Facades\\DB", &aliases),
        "Illuminate\\Support\\Facades\\DB"
    );
}
