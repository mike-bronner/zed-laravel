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
use super::methods::{
    arg_kind, chain_effect, CLOSURE_CARRIERS, RELATION_METHODS, SAME_MODEL_CLOSURE_CARRIERS,
};
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
            shift_arg(arg, &shift);
        }
    }
}

/// Recursively shift byte ranges inside a single `ChainArg`. Handles
/// `Array` by walking its elements (which themselves are `ChainArg`s) so
/// nested string literals inside `with(['posts'])` survive the Blade
/// span re-base.
fn shift_arg(arg: &mut ChainArg, shift: &impl Fn(usize) -> usize) {
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
        ChainArg::Array {
            elements,
            span_byte_range,
        } => {
            *span_byte_range = (shift(span_byte_range.0), shift(span_byte_range.1));
            for elem in elements {
                shift_arg(elem, shift);
            }
        }
        ChainArg::Other => {}
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
                    closure_scope: None,
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
            let receiver = member_chain_receiver(node, bytes, aliases);
            // If this chain's receiver is `$var`, see whether it's bound
            // by an enclosing relation closure (`whereHas('rel', fn ($var)
            // => …)` or `with(['rel' => fn ($var) => …])`). We record the
            // binding here so detect_in_chain can resolve `$var`'s
            // effective model from the parent chain at completion time.
            let closure_scope = match &receiver {
                ChainReceiver::Eloquent(EloquentReceiver::InstanceVar { var, .. }) => {
                    detect_closure_scope(root, bytes, var)
                }
                _ => None,
            };
            links_reversed.reverse();
            return Some(BuilderChain {
                receiver,
                span_byte_range,
                links: links_reversed,
                closure_scope,
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
            Some(("array_creation_expression", n)) => {
                // `with(['posts', 'comments'])`, `select(['name', 'email'])`,
                // etc. — recursively classify the elements so the cursor
                // resolver can find string literals inside the array.
                let elements = extract_array_elements(n, bytes);
                out.push(ChainArg::Array {
                    elements,
                    span_byte_range: (n.start_byte(), n.end_byte()),
                });
            }
            _ => out.push(ChainArg::Other),
        }
    }
    out
}

/// Decode an `array_creation_expression` into a flat `Vec<ChainArg>` of
/// its top-level elements. We don't recurse into nested arrays — Laravel
/// idioms (`with([…])`, `select([…])`) put strings directly inside, and
/// surfacing string literals from a 2D array would mis-fire completion.
///
/// Each array element in tree-sitter PHP is wrapped in an
/// `array_element_initializer` whose `value` field holds the actual
/// expression. For keyed pairs (`'key' => $value`), we look at the
/// `value` side — that's what completion typically targets (e.g.
/// `with(['posts' => fn ($q) => …])`, the cursor in `'posts'` is the
/// relation; the closure is the constraint).
fn extract_array_elements(array_node: Node, bytes: &[u8]) -> Vec<ChainArg> {
    let mut out = Vec::new();
    let mut cursor = array_node.walk();
    for child in array_node.named_children(&mut cursor) {
        if child.kind() != "array_element_initializer" {
            continue;
        }
        // Look at every named child of the element — we want to surface
        // BOTH the key string (in `'foo' => …`, the user might be on
        // 'foo') AND the value string (in `'foo'` non-keyed, that's the
        // value). Iterating both means a cursor on either side gets
        // matched.
        let mut inner_cursor = child.walk();
        for sub in child.named_children(&mut inner_cursor) {
            match sub.kind() {
                "string" => {
                    if let Some((value, quote)) = single_quoted_string(sub, bytes) {
                        out.push(ChainArg::StringLit {
                            value,
                            quote,
                            span_byte_range: (sub.start_byte(), sub.end_byte()),
                        });
                    } else {
                        out.push(ChainArg::Other);
                    }
                }
                "encapsed_string" => {
                    if let Some(value) = encapsed_string_content(sub, bytes) {
                        out.push(ChainArg::StringLit {
                            value,
                            quote: '"',
                            span_byte_range: (sub.start_byte(), sub.end_byte()),
                        });
                    } else {
                        out.push(ChainArg::Other);
                    }
                }
                _ => out.push(ChainArg::Other),
            }
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
fn member_chain_receiver(node: Node, bytes: &[u8], aliases: &UseAliases) -> ChainReceiver {
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
        // `(new self)->with(...)` / `(new User)->where(...)` etc. — the
        // common Laravel pattern of starting a chain from a freshly-
        // constructed model instance. Unwrap the parens and try the inner
        // expression. If it's an object_creation_expression, resolve the
        // class name (including self/static against the enclosing class
        // declaration) and route through the same EloquentBuilder path as
        // static calls.
        "parenthesized_expression" => parenthesized_receiver(node, bytes, aliases),
        // (Future) `$this->prop->...`, `$obj->method()->...`, etc. fall
        // through. Lands alongside the var-type resolver in Phase 9.
        _ => ChainReceiver::Unknown,
    }
}

/// Resolve `(new X)->...` style receivers. The parens wrap exactly one
/// inner expression — we look at its first named child. For an
/// `object_creation_expression`, we extract the class name and return an
/// Eloquent static receiver pointing at it. For anything else (a paren'd
/// variable, a method call, etc.) we recurse so `($var)->method()` still
/// resolves like `$var->method()`.
fn parenthesized_receiver(node: Node, bytes: &[u8], aliases: &UseAliases) -> ChainReceiver {
    let Some(inner) = node.named_child(0) else {
        return ChainReceiver::Unknown;
    };
    if inner.kind() == "object_creation_expression" {
        // Extract the class-name node. PHP tree-sitter gives this back via
        // a `name`, `qualified_name`, or — for `self`/`static`/`parent` —
        // as the relevant keyword node. Walk the named children looking
        // for whichever shape appears.
        let mut class_text: Option<String> = None;
        let mut cursor = inner.walk();
        for child in inner.named_children(&mut cursor) {
            match child.kind() {
                "name" | "qualified_name" | "relative_name" => {
                    class_text = node_text(child, bytes).map(|s| s.to_string());
                    break;
                }
                _ => {}
            }
        }
        // `new self`, `new static`, `new parent` — these are anonymous in
        // some tree-sitter PHP grammar versions (no `name` named-child;
        // the keyword sits as an unnamed child). Fall back to scanning the
        // raw text of the object_creation_expression node.
        if class_text.is_none() {
            if let Some(raw) = node_text(inner, bytes) {
                let after_new = raw.trim_start_matches("new").trim_start();
                let token = after_new
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '\\')
                    .collect::<String>();
                if !token.is_empty() {
                    class_text = Some(token);
                }
            }
        }
        let Some(raw_class) = class_text else {
            return ChainReceiver::Unknown;
        };

        // Resolve self / static / parent against the enclosing class
        // declaration. `self` and `static` map to the containing class.
        // `parent` would map to its parent class — that needs cross-file
        // resolution, so for now we punt and return Unknown.
        let resolved = match raw_class.as_str() {
            "self" | "static" => match enclosing_class_name(inner, bytes) {
                Some(cls) => cls,
                None => return ChainReceiver::Unknown,
            },
            "parent" => return ChainReceiver::Unknown,
            other => super::use_aliases::resolve_class_name(other, aliases),
        };

        return ChainReceiver::Eloquent(EloquentReceiver::StaticModel(resolved));
    }
    // `($var)->method()` — the parens are syntactic noise around a
    // variable receiver. Recurse so var-type resolution (Phase 9) can
    // still kick in.
    member_chain_receiver(inner, bytes, aliases)
}

/// Walk up the syntax tree from `node` looking for the nearest enclosing
/// `class_declaration`. Returns the class's short name (no namespace) on
/// success — that's what `self` / `static` refer to within the class body.
/// Callers can then push the short name through `resolve_class_name` to
/// get the FQCN if the file's namespace declaration is in scope.
fn enclosing_class_name(node: Node, bytes: &[u8]) -> Option<String> {
    let mut current = node.parent();
    while let Some(n) = current {
        if n.kind() == "class_declaration" {
            // The `name` field holds the class identifier.
            let name_node = n.child_by_field_name("name")?;
            return node_text(name_node, bytes).map(|s| s.to_string());
        }
        current = n.parent();
    }
    None
}

/// Phase 8: detect whether `chain_root` sits inside a relation-bearing
/// closure that binds `$receiver_var` to a Builder for some relation.
///
/// Two shapes we recognize:
///
/// 1. `whereHas('rel', fn ($q) => …)` — the closure is the 2nd argument
///    of a method call (`whereHas` / `doesntHave` / `whereDoesntHave` /
///    `withCount` etc.). The 1st argument is a string literal holding
///    the relation name.
///
/// 2. `with(['rel' => fn ($q) => …])` — the closure is the value of a
///    keyed array-element initializer inside the array argument of a
///    relation method (`with` / `load` / `loadMissing` etc.). The
///    array element's KEY is the relation name.
///
/// Returns `Some(binding)` only when:
///
/// - The chain root is genuinely inside such a closure
/// - The closure's first parameter name matches `receiver_var` (so we
///   don't accidentally bind a different variable that happens to be
///   used inside the same closure body)
/// - A relation-name string is in the right position
/// - The enclosing call's method name is one we recognise as
///   relation-carrying
fn detect_closure_scope(
    chain_root: Node,
    bytes: &[u8],
    receiver_var: &str,
) -> Option<ClosureScopeBinding> {
    // Find the nearest enclosing closure by walking parents.
    let mut current = chain_root.parent();
    let closure = loop {
        let n = current?;
        match n.kind() {
            "anonymous_function_creation_expression" | "anonymous_function" | "arrow_function" => {
                break n
            }
            _ => current = n.parent(),
        }
    };

    // The closure's first formal parameter must be `$receiver_var` —
    // otherwise the chain's receiver isn't the closure-bound builder,
    // it's some unrelated variable being used inside.
    let params = closure.child_by_field_name("parameters")?;
    let mut params_cursor = params.walk();
    let first_param = params
        .named_children(&mut params_cursor)
        .find(|n| matches!(n.kind(), "simple_parameter" | "variadic_parameter"))?;
    let name_node = first_param.child_by_field_name("name")?;
    let raw_name = node_text(name_node, bytes)?;
    let param_var = raw_name.trim_start_matches('$').to_string();
    if param_var != receiver_var {
        return None;
    }

    // Walk up from the closure to determine which shape we're in.
    let closure_parent = closure.parent()?;
    match closure_parent.kind() {
        // Shape: keyed-array value — `with(['rel' => closure])` (related-model)
        "array_element_initializer" => {
            detect_closure_scope_array_keyed(closure, closure_parent, bytes, param_var)
        }
        // Shape: positional argument — either `whereHas('rel', closure)`
        // (related-model hop) or `where(closure)` / `when($cond,
        // closure)` / `having(closure)` (same-model). The helper
        // distinguishes based on the enclosing call's method name.
        "argument" => detect_closure_scope_positional(closure, closure_parent, bytes, param_var),
        _ => None,
    }
}

/// Resolve closure-bearing positional-arg shapes. Two flavors handled:
///
/// - Relation-hop carriers (`whereHas('rel', closure)`): the closure
///   binds to the *related* model's builder. Relation name extracted
///   from the FIRST string-literal arg.
/// - Same-model carriers (`where(closure)`, `when($cond, closure)`,
///   `having(closure)`, `tap(closure)`): the closure binds to the same
///   model as the outer chain. No relation hop needed.
fn detect_closure_scope_positional(
    _closure: Node,
    arg_node: Node,
    bytes: &[u8],
    param_var: String,
) -> Option<ClosureScopeBinding> {
    let args_node = arg_node.parent()?;
    if args_node.kind() != "arguments" {
        return None;
    }
    let call_node = args_node.parent()?;
    if !matches!(
        call_node.kind(),
        "member_call_expression" | "scoped_call_expression"
    ) {
        return None;
    }
    let method_name_node = call_node.child_by_field_name("name")?;
    let method_name = node_text(method_name_node, bytes)?;

    // Relation-hop carrier? Extract the relation name from the first
    // string arg.
    if CLOSURE_CARRIERS.contains(&method_name) {
        let mut args_cursor = args_node.walk();
        let first_arg = args_node
            .named_children(&mut args_cursor)
            .find(|n| n.kind() == "argument")?;
        let first_inner = first_arg.named_child(0)?;
        let relation_name = match first_inner.kind() {
            "string" => single_quoted_string(first_inner, bytes).map(|(s, _)| s)?,
            "encapsed_string" => encapsed_string_content(first_inner, bytes)?,
            _ => return None,
        };
        return Some(ClosureScopeBinding {
            param_var,
            kind: ClosureScopeKind::RelationHop { relation_name },
        });
    }

    // Same-model carrier? Bind to the outer chain's effective model.
    if SAME_MODEL_CLOSURE_CARRIERS.contains(&method_name) {
        return Some(ClosureScopeBinding {
            param_var,
            kind: ClosureScopeKind::SameModel,
        });
    }

    None
}

/// Resolve `with(['rel' => fn ($q) => …])` shape. The closure is the
/// `value` side of an `array_element_initializer` whose `key` is the
/// relation-name string. The enclosing array_creation_expression must be
/// the first argument of a relation-carrying method call.
fn detect_closure_scope_array_keyed(
    closure: Node,
    element_node: Node,
    bytes: &[u8],
    param_var: String,
) -> Option<ClosureScopeBinding> {
    // Locate the key — the array_element_initializer's named children are
    // [key, value] for keyed pairs, or just [value] for plain entries.
    // The key is whichever named child ISN'T the closure.
    let mut elem_cursor = element_node.walk();
    let key_node = element_node
        .named_children(&mut elem_cursor)
        .find(|n| n.id() != closure.id())?;
    let relation_name = match key_node.kind() {
        "string" => single_quoted_string(key_node, bytes).map(|(s, _)| s)?,
        "encapsed_string" => encapsed_string_content(key_node, bytes)?,
        _ => return None,
    };

    // Walk up: element → array → argument → arguments → call.
    let array_node = element_node.parent()?;
    if array_node.kind() != "array_creation_expression" {
        return None;
    }
    let arg_node = array_node.parent()?;
    if arg_node.kind() != "argument" {
        return None;
    }
    let args_node = arg_node.parent()?;
    if args_node.kind() != "arguments" {
        return None;
    }
    let call_node = args_node.parent()?;
    if !matches!(
        call_node.kind(),
        "member_call_expression" | "scoped_call_expression"
    ) {
        return None;
    }
    let method_name_node = call_node.child_by_field_name("name")?;
    let method_name = node_text(method_name_node, bytes)?;
    // For the keyed-array shape, the method is in RELATION_METHODS
    // (`with`, `load`, `loadMissing`, `loadCount`, etc.). CLOSURE_CARRIERS
    // also use a keyed-array form sometimes, so accept either.
    if !RELATION_METHODS.contains(&method_name) && !CLOSURE_CARRIERS.contains(&method_name) {
        return None;
    }

    Some(ClosureScopeBinding {
        param_var,
        kind: ClosureScopeKind::RelationHop { relation_name },
    })
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
