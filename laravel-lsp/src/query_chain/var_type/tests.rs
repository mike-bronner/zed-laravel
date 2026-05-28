use super::*;
use crate::parser::parse_php;
use crate::query_chain::use_aliases::extract_use_aliases;

/// Parse a PHP snippet (no leading `<?php`) and find the first
/// `variable_name` node whose text is `$<var>`. Returns the node and the
/// wrapped source so callers can pass them to `resolve`.
fn find_var_node<'tree>(
    tree: &'tree tree_sitter::Tree,
    bytes: &[u8],
    var: &str,
) -> Option<tree_sitter::Node<'tree>> {
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "variable_name" {
            let start = node.start_byte();
            let end = node.end_byte();
            if let Ok(text) = std::str::from_utf8(&bytes[start..end]) {
                if text.trim_start_matches('$') == var {
                    return Some(node);
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    None
}

/// End-to-end helper: parse the snippet, find the named variable, and run
/// `resolve` against it. Returns the resolved FQCN.
fn resolve_in(src: &str, var: &str) -> Option<String> {
    let wrapped = format!("<?php\n{src}");
    let tree = parse_php(&wrapped).expect("parse");
    let bytes = wrapped.as_bytes();
    let aliases = extract_use_aliases(&tree, &wrapped);
    let node = find_var_node(&tree, bytes, var)?;
    resolve(node, bytes, var, &aliases)
}

#[test]
fn typed_function_param_resolves_to_local_class() {
    // `function show(User $user)` — the receiver variable's type is `User`.
    // No use alias, no namespace ⇒ bare class name is returned.
    let src = r#"
function show(User $user) {
    $user->newQuery();
}
"#;
    assert_eq!(resolve_in(src, "user").as_deref(), Some("User"));
}

#[test]
fn typed_param_resolves_through_use_alias() {
    // `use App\Models\Post as Article;` — a parameter declared `Article $a`
    // should resolve to `App\Models\Post`. This is what makes downstream
    // class-locator routing work: it never sees `Article`, only the FQCN.
    let src = r#"
use App\Models\Post as Article;

function items(Article $a) {
    $a->where('published', true);
}
"#;
    assert_eq!(resolve_in(src, "a").as_deref(), Some("App\\Models\\Post"));
}

#[test]
fn typed_param_handles_nullable_shorthand() {
    // `?User` → `User`. PHP's nullable shorthand is just sugar for
    // `User|null` — same resolution.
    let src = r#"
function maybeShow(?User $user) {
    $user->newQuery();
}
"#;
    assert_eq!(resolve_in(src, "user").as_deref(), Some("User"));
}

#[test]
fn typed_param_handles_union_with_null() {
    // `User|null` → `User`. Same idea as nullable but spelled out.
    let src = r#"
function maybeShow(User|null $user) {
    $user->newQuery();
}
"#;
    assert_eq!(resolve_in(src, "user").as_deref(), Some("User"));
}

#[test]
fn typed_param_skips_primitives() {
    // `int $count` → None. We don't try to autoload `int.php`.
    let src = r#"
function tally(int $count) {
    $count;
}
"#;
    assert_eq!(resolve_in(src, "count"), None);
}

#[test]
fn typed_method_param_resolves_inside_class_method() {
    // Same logic for method declarations, not just bare functions.
    let src = r#"
namespace App\Http\Controllers;

use App\Models\User;

class UserController
{
    public function show(User $user)
    {
        $user->newQuery();
    }
}
"#;
    assert_eq!(
        resolve_in(src, "user").as_deref(),
        Some("App\\Models\\User")
    );
}

#[test]
fn var_docblock_resolves_when_no_typed_param() {
    // No typed param — the user wrote a docblock instead. We pick it up.
    let src = r#"
function pull($id) {
    /** @var User $user */
    $user = SomeRepository::findOne($id);
    $user->newQuery();
}
"#;
    assert_eq!(resolve_in(src, "user").as_deref(), Some("User"));
}

#[test]
fn var_docblock_resolves_through_use_alias() {
    // Same alias-resolution behavior as typed params.
    let src = r#"
use App\Models\Post as Article;

function pull() {
    /** @var Article $a */
    $a = repo();
    $a->where('published', true);
}
"#;
    assert_eq!(resolve_in(src, "a").as_deref(), Some("App\\Models\\Post"));
}

#[test]
fn var_docblock_multi_line_format() {
    // Real-world docblocks span multiple lines. The comment node carries
    // the full text so the regex still finds the @var line.
    let src = r#"
function pull() {
    /**
     * Loads the active user.
     *
     * @var User $user
     */
    $user = something();
    $user->newQuery();
}
"#;
    assert_eq!(resolve_in(src, "user").as_deref(), Some("User"));
}

#[test]
fn missing_type_info_returns_none() {
    // No docblock, no typed param — we have nothing to work with. Better
    // None than a false positive that confuses completion.
    let src = r#"
function pull() {
    $u = something();
    $u->newQuery();
}
"#;
    assert_eq!(resolve_in(src, "u"), None);
}

#[test]
fn docblock_for_different_var_is_ignored() {
    // A `@var X $other` shouldn't satisfy a query for `$user` — the
    // variable name must match exactly.
    let src = r#"
function pull() {
    /** @var Post $other */
    $user = something();
    $user->newQuery();
}
"#;
    assert_eq!(resolve_in(src, "user"), None);
}
