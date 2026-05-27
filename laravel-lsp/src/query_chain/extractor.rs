//! Tree-sitter walker that extracts [`BuilderChain`] values from a parsed PHP
//! file.
//!
//! The walker visits every `scoped_call_expression` (e.g. `User::query()`,
//! `DB::table(...)`) and `member_call_expression` (e.g. `$x->where(...)`),
//! identifies the ones that sit at the top of a chain, walks down through the
//! `object` field to discover the chain root (the receiver), then collects
//! every link in source order with its classification and parsed arguments.
//!
//! Why imperative and not a tree-sitter `.scm` query? Captures are great for
//! "match this exact shape" — but chains are recursive and the same node
//! (`member_call_expression`) appears at every depth. We'd need to either
//! capture every node and filter post-hoc (defeating the point of declarative
//! matching) or write a query that pattern-matches the whole chain (which
//! tree-sitter query syntax doesn't compose well for arbitrary depth).
//!
//! The walker is iterative — it uses a stack of tree-cursor positions rather
//! than recursion — so deeply-nested chains can't blow the stack on real-world
//! files. Closures inside chains are extracted as `ChainArg::Closure` with
//! their formal parameters; the bodies inside those closures are walked
//! independently (the cursor descends into every child).

use super::chain::*;
use super::methods::{arg_kind, chain_effect};
use super::use_aliases::{extract_use_aliases, resolve_class_name, UseAliases};
use tree_sitter::{Node, Tree};

/// Shift every byte range in a chain by `(byte_offset - wrapper_prefix_len)`.
///
/// Used when chains are extracted from a Blade-embedded PHP region that was
/// wrapped with `<?php ` before parsing: snippet-local byte `N` corresponds
/// to outer-file byte `byte_offset + (N - wrapper_prefix_len)`. Applies to
/// every span on the chain — the chain itself, every link, every string-arg
/// and closure-body, and the DbTable receiver's `name_byte_range`.
///
/// `saturating_sub` guards against the (impossible-in-practice) case of a
/// snippet position before the wrapper prefix.
pub fn shift_chain_byte_ranges(
    chain: &mut BuilderChain,
    byte_offset: usize,
    wrapper_prefix_len: usize,
) {
    let shift = |b: usize| byte_offset + b.saturating_sub(wrapper_prefix_len);

    chain.span_byte_range = (
        shift(chain.span_byte_range.0),
        shift(chain.span_byte_range.1),
    );

    if let ChainReceiver::DbTable {
        name_byte_range, ..
    } = &mut chain.receiver
    {
        *name_byte_range = (shift(name_byte_range.0), shift(name_byte_range.1));
    }

    for link in &mut chain.links {
        link.span_byte_range = (shift(link.span_byte_range.0), shift(link.span_byte_range.1));
        for arg in &mut link.args {
            match arg {
                ChainArg::StringLit {
                    span_byte_range, ..
                } => {
                    *span_byte_range = (shift(span_byte_range.0), shift(span_byte_range.1));
                }
                ChainArg::Closure {
                    body_byte_range, ..
                } => {
                    *body_byte_range = (shift(body_byte_range.0), shift(body_byte_range.1));
                }
                ChainArg::Other => {}
            }
        }
    }
}

/// Extract every builder chain in the file. Order of return is depth-first
/// pre-order across the AST — callers that need positional lookup should
/// build their own index.
///
/// `use` statement aliases are resolved as part of this pass so chains like
/// `Database::table(...)` (when imported as `use ... DB as Database;`) get
/// classified as `DbTable` receivers, not Eloquent models.
pub fn extract_chains(tree: &Tree, source: &str) -> Vec<BuilderChain> {
    let bytes = source.as_bytes();
    let aliases = extract_use_aliases(tree, source);
    let mut chains = Vec::new();
    let mut stack: Vec<Node> = vec![tree.root_node()];

    while let Some(node) = stack.pop() {
        if is_chain_root(node) {
            if let Some(chain) = build_chain_from_root(node, bytes, &aliases) {
                chains.push(chain);
            }
        }

        // Push children in reverse so the visit order is left-to-right.
        // (Iteration order doesn't affect correctness — chain extraction is
        // independent per root — but it makes test fixtures predictable.)
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }

    chains
}

/// A node is a chain root if it's a call expression AND its parent is NOT a
/// call expression that holds it as the `object` field. In other words:
/// `User::query()->where(...)` has one root (the outer `where` call); the
/// inner `query()` call is part of that chain, not a separate root.
fn is_chain_root(node: Node) -> bool {
    let kind = node.kind();
    if kind != "member_call_expression" && kind != "scoped_call_expression" {
        return false;
    }
    let Some(parent) = node.parent() else {
        return true;
    };
    if parent.kind() != "member_call_expression" {
        return true;
    }
    // Parent is a member-call. We're the chain root unless the parent uses
    // this node as its `object` (i.e., we're earlier in the chain than the
    // parent).
    let Some(object_field) = parent.child_by_field_name("object") else {
        return true;
    };
    object_field.id() != node.id()
}

/// Walk down the `object` chain from `root`, collecting links in reverse, then
/// reverse to source order. Returns `None` if the chain receiver can't be
/// identified at all (the chain is then uninteresting for completion).
fn build_chain_from_root(root: Node, bytes: &[u8], aliases: &UseAliases) -> Option<BuilderChain> {
    let span_byte_range = (root.start_byte(), root.end_byte());

    // Collect links bottom-up: start at `root`, walk its `object` until we
    // hit a non-call node. The deepest call is the bottom; its `scope` (for
    // scoped-call) or `object` (for member-call against a variable) defines
    // the receiver.
    let mut links_reversed: Vec<ChainLink> = Vec::new();
    let mut node = root;

    loop {
        let kind = node.kind();
        if kind == "member_call_expression" || kind == "scoped_call_expression" {
            if let Some(link) = extract_link(node, bytes) {
                links_reversed.push(link);
            }
            // Descend into the `object` (member-call) or stop (scoped-call —
            // the scope is the receiver, not another call).
            if kind == "scoped_call_expression" {
                // Bottom of the chain. The scope is the receiver.
                let receiver = scoped_call_receiver(node, bytes, &links_reversed, aliases);

                // For `DB::table('|')`, the bottom link's first string arg
                // names a table. Annotate it so the cursor resolver can
                // distinguish "table name" from "column name" at the
                // ArgKind level. The bottom link is the one we just pushed
                // (last in links_reversed before reversal).
                if matches!(receiver, ChainReceiver::DbTable { .. }) {
                    if let Some(bottom) = links_reversed.last_mut() {
                        bottom.arg = ArgKind::Table;
                    }
                }

                links_reversed.reverse();
                return Some(BuilderChain {
                    receiver,
                    span_byte_range,
                    links: links_reversed,
                });
            }
            // member-call: keep descending through .object
            match node.child_by_field_name("object") {
                Some(next) => node = next,
                None => break,
            }
        } else {
            // We've descended past the calls. The current node is the
            // receiver expression (variable, parenthesised expr, etc.).
            let receiver = member_chain_receiver(node, bytes);
            links_reversed.reverse();
            return Some(BuilderChain {
                receiver,
                span_byte_range,
                links: links_reversed,
            });
        }
    }

    None
}

/// Extract one link from a call expression node. Returns `None` if the node
/// doesn't have a method name we can read (malformed AST).
fn extract_link(call_node: Node, bytes: &[u8]) -> Option<ChainLink> {
    let name_node = call_node.child_by_field_name("name")?;
    let method = node_text(name_node, bytes)?.to_string();

    let args_node = call_node.child_by_field_name("arguments");
    let args = args_node
        .map(|n| extract_args(n, bytes))
        .unwrap_or_default();

    Some(ChainLink {
        arg: arg_kind(&method),
        effect: chain_effect(&method),
        method,
        span_byte_range: (call_node.start_byte(), call_node.end_byte()),
        args,
    })
}

/// Decode an `arguments` node into a `Vec<ChainArg>` in source order. We only
/// recognise the shapes that matter for completion — string literals and
/// closures — everything else collapses to `ChainArg::Other`.
fn extract_args(args_node: Node, bytes: &[u8]) -> Vec<ChainArg> {
    let mut out = Vec::new();
    let mut cursor = args_node.walk();
    for child in args_node.children(&mut cursor) {
        if child.kind() != "argument" {
            continue;
        }
        // An `argument` wraps a single expression. Look at its first named
        // child.
        let inner = child.named_child(0);
        match inner.map(|n| (n.kind(), n)) {
            Some(("string", n)) => {
                if let Some((value, quote)) = single_quoted_string(n, bytes) {
                    out.push(ChainArg::StringLit {
                        value,
                        quote,
                        span_byte_range: (n.start_byte(), n.end_byte()),
                    });
                } else {
                    out.push(ChainArg::Other);
                }
            }
            Some(("encapsed_string", n)) => {
                if let Some(value) = encapsed_string_content(n, bytes) {
                    out.push(ChainArg::StringLit {
                        value,
                        quote: '"',
                        span_byte_range: (n.start_byte(), n.end_byte()),
                    });
                } else {
                    out.push(ChainArg::Other);
                }
            }
            Some(("anonymous_function_creation_expression", n))
            | Some(("anonymous_function", n))
            | Some(("arrow_function", n)) => {
                let params = extract_closure_params(n, bytes);
                let body = closure_body_range(n).unwrap_or((n.start_byte(), n.end_byte()));
                out.push(ChainArg::Closure {
                    params,
                    body_byte_range: body,
                });
            }
            _ => out.push(ChainArg::Other),
        }
    }
    out
}

/// Pull the parameter list out of a closure node, recording the parameter
/// names and (when present) their type hints. The PHP-side default is
/// untyped (`function ($q) { ... }`); typed params show up as
/// `simple_parameter` with a `type` field.
fn extract_closure_params(closure: Node, bytes: &[u8]) -> Vec<ClosureParam> {
    let Some(params_node) = closure.child_by_field_name("parameters") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cursor = params_node.walk();
    for param in params_node.children(&mut cursor) {
        if param.kind() != "simple_parameter" && param.kind() != "variadic_parameter" {
            continue;
        }
        // The `name` field holds a `variable_name` like `$q`. Strip the `$`.
        let Some(name_node) = param.child_by_field_name("name") else {
            continue;
        };
        let Some(raw) = node_text(name_node, bytes) else {
            continue;
        };
        let name = raw.trim_start_matches('$').to_string();
        let php_type = param
            .child_by_field_name("type")
            .and_then(|t| node_text(t, bytes))
            .map(|s| s.to_string());
        out.push(ClosureParam { name, php_type });
    }
    out
}

/// Byte range of the closure body. For traditional closures this is the
/// `{ ... }` compound statement; for arrow functions it's the expression
/// after `=>`. Falls back to the full closure span if neither field is
/// present.
fn closure_body_range(closure: Node) -> Option<(usize, usize)> {
    closure
        .child_by_field_name("body")
        .map(|b| (b.start_byte(), b.end_byte()))
}

/// Receiver for a bottomed-out chain whose deepest call is a
/// `scoped_call_expression` (`Class::method(...)`).
///
/// `links_reversed` holds the chain's links in root-first order — we pushed
/// the outermost call first and descended through `.object`, so the deepest
/// call (the one whose scope is the receiver) sits at `links_reversed.last()`.
/// That's the link we inspect to spot `DB::table('users')`: scope is `DB`,
/// method is `table`, first arg is a string literal naming the table.
///
/// `aliases` lets us resolve `use ... as Foo; Foo::table(...)` — without it
/// aliased imports of the DB facade would slip through as Eloquent models.
fn scoped_call_receiver(
    call_node: Node,
    bytes: &[u8],
    links_reversed: &[ChainLink],
    aliases: &UseAliases,
) -> ChainReceiver {
    let Some(scope_node) = call_node.child_by_field_name("scope") else {
        return ChainReceiver::Unknown;
    };
    let Some(class_name_raw) = node_text(scope_node, bytes) else {
        return ChainReceiver::Unknown;
    };

    // Resolve the class name through `use` aliases. Examples:
    //   `Database::table` with `use ... DB as Database;` → `Illuminate\...\DB`
    //   `DB::table` with `use Illuminate\...\DB;` → `Illuminate\...\DB`
    //   `DB::table` with no `use` → `DB` (unchanged; Laravel's global alias)
    let resolved = resolve_class_name(class_name_raw, aliases);

    // PHP class names are case-insensitive — `db::table()` and `DB::table()`
    // resolve to the same class at runtime. Match accordingly so `db::`
    // chains aren't silently misclassified as Eloquent models.
    if is_db_facade(&resolved) {
        if let Some(bottom_link) = links_reversed.last() {
            if bottom_link.method.eq_ignore_ascii_case("table") {
                if let Some(ChainArg::StringLit {
                    value,
                    span_byte_range,
                    ..
                }) = bottom_link.args.first().cloned()
                {
                    return ChainReceiver::DbTable {
                        table: value,
                        name_byte_range: span_byte_range,
                    };
                }
            }
        }
    }

    // Not the DB facade — emit an Eloquent model receiver using the
    // *resolved* class name. This means `use App\Models\User as MyUser;
    // MyUser::query()` will land as `StaticModel("App\Models\User")`, which
    // resolves cleanly in later phases. The leading backslash is already
    // stripped by `resolve_class_name`.
    ChainReceiver::Eloquent(EloquentReceiver::StaticModel(resolved))
}

/// Whether `class_name` (as it appears in the source — possibly with `\`
/// segments) refers to the Laravel `DB` facade. PHP class names are
/// case-insensitive, so `DB`, `db`, `Db`, and `\Illuminate\Support\Facades\DB`
/// all match.
fn is_db_facade(class_name: &str) -> bool {
    let basename = class_name.rsplit('\\').next().unwrap_or(class_name);
    basename.eq_ignore_ascii_case("DB")
}

/// Receiver for a chain whose deepest call is a `member_call_expression`
/// against something that isn't itself a call — most commonly a `$var`. For
/// anything we don't recognise, return `Unknown`; completion will silently
/// no-op rather than guess.
fn member_chain_receiver(node: Node, bytes: &[u8]) -> ChainReceiver {
    match node.kind() {
        "variable_name" => {
            let Some(raw) = node_text(node, bytes) else {
                return ChainReceiver::Unknown;
            };
            let var = raw.trim_start_matches('$').to_string();
            ChainReceiver::Eloquent(EloquentReceiver::InstanceVar {
                var,
                php_type: None, // var_type::resolve fills this in later
            })
        }
        // (Future) parenthesised expressions, `$this->prop->...`, etc. fall
        // through. Phase 2 keeps the receiver detection conservative; richer
        // shapes land alongside the var-type resolver in Phase 9.
        _ => ChainReceiver::Unknown,
    }
}

/// Extract a single-quoted string's content. Returns `(value, '\'')` on
/// success. Returns `None` for malformed strings or empty content nodes.
fn single_quoted_string(node: Node, bytes: &[u8]) -> Option<(String, char)> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "string_content" {
            if let Some(s) = node_text(child, bytes) {
                return Some((s.to_string(), '\''));
            }
        }
    }
    // Empty string literal (`''`) — string node with no string_content child.
    if let Some(text) = node_text(node, bytes) {
        if text == "''" {
            return Some((String::new(), '\''));
        }
    }
    None
}

/// Extract a double-quoted string's content, joining any `string_content`
/// children. Interpolations are skipped — for a chain like
/// `where("col_$idx")` we don't try to resolve the variable; just return the
/// literal slice so callers can decide whether to use it.
fn encapsed_string_content(node: Node, bytes: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    let mut parts = Vec::new();
    for child in node.children(&mut cursor) {
        if child.kind() == "string_content" {
            if let Some(s) = node_text(child, bytes) {
                parts.push(s.to_string());
            }
        }
    }
    if parts.is_empty() {
        // Empty double-quoted string (`""`) — return empty content.
        if node_text(node, bytes).map(|t| t == "\"\"").unwrap_or(false) {
            return Some(String::new());
        }
        return None;
    }
    Some(parts.concat())
}

fn node_text<'a>(node: Node<'_>, bytes: &'a [u8]) -> Option<&'a str> {
    let start = node.start_byte();
    let end = node.end_byte();
    std::str::from_utf8(bytes.get(start..end)?).ok()
}

#[cfg(test)]
mod tests;
