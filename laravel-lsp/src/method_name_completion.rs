//! Method-name completion at `Model::|` (and, in a later phase, `$builder->|`).
//!
//! Sibling to `query_chain::try_query_chain_completion` (which completes
//! string arguments inside method calls). This module emits the method
//! NAMES themselves — `where`, `whereIn`, `find`, etc. — for the specific
//! position where `__callStatic` forwarding hides them from the PHP LSP.
//!
//! ## Why this exists at all
//!
//! `Illuminate\Database\Eloquent\Model` has **no** `@method` PHPDoc tags
//! and no `@mixin` for its `__callStatic` forwarder. Static analyzers
//! (Intelephense, PhpActor, PhpStan, …) can see methods defined directly
//! on the model class and methods added by traits, but they cannot see the
//! Builder methods reached via `Model::__callStatic`. So at `User::|` you
//! get model-internal methods (`save`, `toArray`, `fill`, …) and trait
//! methods, but never `where` / `find` / `query` / etc.
//!
//! `Illuminate\Database\Eloquent\Builder` does carry `@mixin
//! \Illuminate\Database\Query\Builder`, so once the user reaches an
//! Eloquent Builder receiver — `User::query()->|`, `$builder->|` — the PHP
//! LSP can resolve the method chain normally. That's why we only fire at
//! the **static** position: anywhere else, the PHP LSP already has the
//! information it needs.
//!
//! ## Where the method list comes from
//!
//! Parsed from the user's actual `vendor/laravel/framework/.../Builder.php`
//! and `vendor/laravel/framework/.../Query/Builder.php` at first use,
//! cached on the `Backend`. See [`vendor_parser`] for the parse logic and
//! [`vendor_parser::BuilderMethodIndex`] for the cached shape.
//!
//! Parsing (not hardcoding) means the list automatically tracks whatever
//! Laravel version the project is on — new methods appear, deprecated
//! ones disappear, summaries reflect the actual PHPDoc the framework
//! ships.
//!
//! ## Phase 1 (this file's current scope)
//!
//! - Detect cursor at `Model::|` via line-text scan
//! - Caller is expected to gate emission on "receiver is an Eloquent
//!   model" (the cursor detection itself returns the raw receiver text
//!   without validation; the `Backend` does the FQCN resolution + Eloquent
//!   check)
//! - Emit `CompletionItem`s with `kind: Method` and `detail: "Laravel
//!   Eloquent Builder"`, **no `sortText` override** — items sort
//!   alphabetically alongside the PHP LSP's, no push-down

// vendor_parser was consolidated into `laravel_introspector::builder_index`.
// Re-export the canonical type names so existing call sites that depended
// on `method_name_completion::{BuilderMethodIndex, ParsedMethod}` keep
// compiling while the migration completes.
pub use crate::laravel_introspector::{BuilderMethodIndex, ParsedMethod};

use tower_lsp::lsp_types::{CompletionItem, CompletionItemKind};

use crate::completion_format::{split_phpdoc, CodeBlock, CompletionDoc};

/// Where the cursor sits relative to a `::` or `->` operator.
///
/// Drives which method tables we emit. Only `Static` is acted on in the
/// current phase — `Instance` is detected so the dispatcher can short-
/// circuit (avoid running our static path), but no items are returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MethodNameContext {
    /// Cursor at `Receiver::|` or `Receiver::partial|`. `receiver` is the
    /// raw text from source (e.g. `"User"`, `"App\\Models\\User"`).
    /// Validation that the receiver actually IS an Eloquent model happens
    /// in the caller — this module just reports the position.
    Static { receiver: String },
    /// Cursor at `$var->|` or `something()->|`. Detected for completeness;
    /// the current phase yields to the PHP LSP here because Eloquent
    /// Builder's `@mixin` annotation makes the method chain visible to
    /// static analysis.
    Instance,
}

/// Detect whether the cursor is at a method-name position on `line`.
///
/// Strategy: take the slice of the line before the cursor, strip any
/// trailing identifier chars (so `User::wher|` strips to `User::`), and
/// check what's at the end. `::` → static position with receiver
/// extraction; `->` → instance; anything else → `None`.
///
/// Receiver extraction walks back through word chars, `_`, and `\` to
/// support both bare names (`User`), namespaced names (`App\Models\User`),
/// and leading-backslash FQCNs (`\App\Models\User`).
pub fn detect_method_name_position(line: &str, cursor_col: usize) -> Option<MethodNameContext> {
    if cursor_col > line.len() {
        return None;
    }
    let before = &line[..cursor_col];
    let stripped = before.trim_end_matches(|c: char| c.is_alphanumeric() || c == '_');

    if stripped.ends_with("::") {
        let before_op = &stripped[..stripped.len() - 2];
        // Walk back through identifier / namespace chars to find the
        // receiver's start. Stop at the first char that can't be part of
        // a class name.
        let receiver_start = before_op
            .rfind(|c: char| !(c.is_alphanumeric() || c == '_' || c == '\\'))
            .map(|i| i + before_op[i..].chars().next().unwrap().len_utf8())
            .unwrap_or(0);
        let receiver = before_op[receiver_start..].to_string();
        if receiver.is_empty() {
            // `::` with no receiver — not a real method-name position.
            return None;
        }
        Some(MethodNameContext::Static { receiver })
    } else if stripped.ends_with("->") {
        Some(MethodNameContext::Instance)
    } else {
        None
    }
}

/// Build the completion items for a static-position cursor on an Eloquent
/// model. `index` is the cached parse of `vendor/laravel/framework`'s
/// Builder + Query/Builder; this function merges the surface and renders
/// each method as a `CompletionItem`.
///
/// Items carry:
///
/// - `kind: METHOD`
/// - `detail: "Laravel Eloquent Builder"` so users can attribute our
///   items at a glance when both LSPs return overlapping entries
/// - `documentation`: an Intelephense-style markdown panel built through
///   the shared [`crate::completion_format`] template — header
///   (`FQCN::method`), summary, a fenced PHP signature block, then the
///   `@param` / `@return` tags
/// - **No `sortText`** — items sort alphabetically alongside the PHP
///   LSP's, no push-down. At the static position there's effectively no
///   collision anyway (the PHP LSP doesn't see Builder methods at
///   `Model::|`), so push-down would only deprioritise us against the
///   model-internal methods that we don't want to lead with.
pub fn build_items_from_index(index: &BuilderMethodIndex) -> Vec<CompletionItem> {
    index
        .merged_surface()
        .iter()
        .map(|m| method_to_item(m))
        .collect()
}

fn method_to_item(m: &ParsedMethod) -> CompletionItem {
    // Field mapping is constrained by how Zed actually renders rows
    // (verified via source-dive of `crates/editor/src/code_context_menus.rs`
    // and `crates/language/src/language.rs`):
    //
    // - **Main row label** = `label` + " " + `detail`. Zed concatenates
    //   them into one bold string. We put the method name in `label` and
    //   the return type in `detail`, so the row reads `where $this`.
    //   `label_details.*` fields are IGNORED on the fallback render path
    //   that PHP / generic LSPs use — no point setting them.
    // - **Aside docs panel** (the rich panel on hover) = `documentation`
    //   when it's `Documentation::MarkupContent` markdown.
    //
    // We deliberately do NOT use the end-slot summary (which would require
    // a single-line `Documentation::String`). Zed's current design makes
    // end-slot summary and rich markdown aside mutually exclusive — you
    // get one or the other from a given completion item, never both, and
    // trying to swap them via `completionItem/resolve` produces visibly
    // broken UX (the end-slot disappears the moment the user focuses an
    // item). Filed as a feature request upstream; for now we match
    // Intelephense's pattern: short row, rich aside.
    CompletionItem {
        label: m.name.clone(),
        kind: Some(CompletionItemKind::METHOD),
        // Concatenated into the main label by Zed: row shows `where $this`.
        detail: m.return_type.clone(),
        documentation: Some(build_method_documentation(m).into_documentation()),
        // No insert_text: Zed inserts the label as-is and the user types
        // `(` themselves. Snippet `where($1)` would conflict with the PHP
        // LSP's signature help and with chained-call typing patterns —
        // keep it boring and let the editor compose.
        ..Default::default()
    }
}

/// Render a [`crate::laravel_introspector::ScopeInfo`] as a popup item — same
/// visual treatment as a Builder method (`label`, `detail =
/// Builder<static>`, Intelephense-style markdown panel), with the docs
/// panel header pointing at the actual defining class
/// (`App\Models\Portfolio::scopeActive` etc.) so users can tell scopes
/// apart from framework Builder methods.
///
/// Scopes always return a Builder for chaining, so `detail` is always
/// `Builder<static>` regardless of the underlying method's declared
/// return type (which may be `void` for old-style scopes that mutate
/// via `$query->where(…)` and return implicitly).
pub fn scope_to_item(scope: &crate::laravel_introspector::ScopeInfo) -> CompletionItem {
    CompletionItem {
        label: scope.name.clone(),
        kind: Some(CompletionItemKind::METHOD),
        detail: Some("Builder<static>".to_string()),
        documentation: Some(build_scope_documentation(scope).into_documentation()),
        ..Default::default()
    }
}

fn build_scope_documentation(scope: &crate::laravel_introspector::ScopeInfo) -> CompletionDoc {
    let (summary, tags) = match &scope.doc_body {
        Some(body) => split_phpdoc(body),
        None => (scope.summary.clone(), Vec::new()),
    };

    // Header points at the defining class + the underlying method name
    // so users hovering see exactly which class declares the scope and
    // by which original method.
    CompletionDoc::new()
        .header(format!("{}::{}", scope.source_class, scope.name))
        .summary_opt(summary.or_else(|| scope.summary.clone()))
        .code(CodeBlock::new("php", scope.signature.clone()))
        // Resolve $this/self/static in tags to match the row detail —
        // scopes return Builder<static> the same way Builder methods do.
        .resolve_self_for("Illuminate\\Database\\Eloquent\\Builder")
        .sections(tags)
}

/// Assemble the [`CompletionDoc`] for a Builder method, matching
/// Intelephense's panel layout — header (`FQCN::method`), summary,
/// fenced PHP signature block, `@param` / `@return` tags as paragraphs.
/// See the [`crate::completion_format`] module for the rendered shape.
///
/// The summary / tag split comes from [`split_phpdoc`] when a docblock is
/// present; the parser's pre-extracted `summary` is used as a fallback so
/// methods with a one-line doc still get a description.
fn build_method_documentation(m: &ParsedMethod) -> CompletionDoc {
    let (summary, tags) = match &m.doc_body {
        Some(body) => split_phpdoc(body),
        None => (m.summary.clone(), Vec::new()),
    };

    CompletionDoc::new()
        .header(format!("{}::{}", m.source_class, m.name))
        .summary_opt(summary.or_else(|| m.summary.clone()))
        .code(CodeBlock::new("php", m.signature.clone()))
        // Resolve `$this`/`self`/`static` in @return / @param tags so the
        // panel matches the row's `detail` field (which already shows
        // `Builder<static>` instead of `$this`). Without this, the panel
        // says ``@return `$this` `` while the row says `Builder<static>`
        // — same fact, different display, confusing.
        .resolve_self_for(&m.source_class)
        .sections(tags)
}

// ---- Phase 3: Eloquent dynamic where{Column} item synthesis ------------

/// Synthesize completion items for Eloquent's dynamic `where{Column}` /
/// `orWhere{Column}` magic against `columns`. PHP magic methods only
/// fire when no real method matches — so for each column we skip the
/// synthetic when the resulting `where{Studly(column)}` collides with
/// a real Builder method (Eloquent or Query), an inherited model
/// method, or a local scope. This is modeling PHP semantics directly,
/// not deduplication: the synthetic never exists at runtime when a
/// real method does.
///
/// `view` supplies the real-method surface (`callstatic_surface` for
/// Builder methods, `scopes` for model-local scopes). `index` is the
/// pre-built Builder method index — same data, different shape; both
/// are consulted because not every Builder method appears in the
/// model's resolved view (the walker stops at Eloquent's base Model
/// and the Builder methods come from the index instead).
///
/// Returns items in source order (whatever order `columns` was passed
/// in). The caller orders columns from highest- to lowest-confidence
/// provenance (cast > fillable > convention > DB schema) — that order
/// carries into the popup, which is the right default before any
/// `sortText` override.
pub fn dynamic_where_to_items(
    view: &crate::laravel_introspector::ClassView,
    index: &BuilderMethodIndex,
    columns: &[crate::laravel_introspector::ColumnInfo],
) -> Vec<CompletionItem> {
    use std::collections::HashSet;

    // Real-method surface: every name that PHP would resolve BEFORE
    // routing to __call/__callStatic. Built once, reused per column.
    let mut real_methods: HashSet<&str> = HashSet::new();
    for m in &view.callstatic_surface {
        real_methods.insert(m.name.as_str());
    }
    for m in &index.eloquent_builder {
        real_methods.insert(m.name.as_str());
    }
    for m in &index.query_builder {
        real_methods.insert(m.name.as_str());
    }
    for s in &view.scopes {
        real_methods.insert(s.name.as_str());
    }

    let mut items: Vec<CompletionItem> = Vec::new();
    for col in columns {
        let studly = crate::laravel_introspector::snake_to_studly(&col.name);
        for prefix in ["where", "orWhere"] {
            let synthetic = format!("{prefix}{studly}");
            // PHP's `__call` only fires for unknown methods. If the
            // synthetic matches a real method, PHP routes there
            // instead — emitting our item would be misleading.
            if real_methods.contains(synthetic.as_str()) {
                continue;
            }
            items.push(dynamic_where_item(&synthetic, col, prefix == "orWhere"));
        }
    }
    items
}

/// Build one `where{Column}` / `orWhere{Column}` completion item with
/// the Intelephense-style doc panel. The panel header includes the
/// column source so the user can tell at a glance whether the column
/// came from `$fillable`, a cast, a convention, etc.
fn dynamic_where_item(
    synthetic_name: &str,
    col: &crate::laravel_introspector::ColumnInfo,
    is_or: bool,
) -> CompletionItem {
    let detail = col.php_type.clone();
    let summary = if is_or {
        format!(
            "Eloquent dynamic where (or {} = ?)",
            col.name
        )
    } else {
        format!("Eloquent dynamic where ({} = ?)", col.name)
    };
    let signature = format!(
        "public function {synthetic_name}({} $value): Builder<static>",
        col.php_type
    );
    let provenance = column_provenance_line(col);

    let doc = CompletionDoc::new()
        .header(synthetic_name.to_string())
        .summary(summary)
        .code(CodeBlock::new("php", signature))
        .section(provenance);

    CompletionItem {
        label: synthetic_name.to_string(),
        kind: Some(CompletionItemKind::METHOD),
        detail: Some(detail),
        documentation: Some(doc.into_documentation()),
        ..Default::default()
    }
}

/// One-line provenance string for the column's source. Renders below
/// the signature block as a `> Note:`-style aside so users can tell
/// `$fillable`-derived columns apart from convention guesses.
fn column_provenance_line(col: &crate::laravel_introspector::ColumnInfo) -> String {
    use crate::laravel_introspector::ColumnSource;
    let detail = match col.source {
        ColumnSource::Fillable => "declared in `$fillable`",
        ColumnSource::Cast => "declared in `$casts`",
        ColumnSource::Dates => "declared in `$dates`",
        ColumnSource::Attributes => "declared in `$attributes`",
        ColumnSource::Convention => "Laravel convention",
        ColumnSource::Trait => "implied by composed trait",
        ColumnSource::ParentClass => "implied by parent class",
        ColumnSource::DatabaseSchema => "from live DB schema",
    };
    format!("> Column `{}` — {}.", col.name, detail)
}

#[cfg(test)]
mod tests;
