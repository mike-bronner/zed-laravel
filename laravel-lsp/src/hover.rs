//! Hover content — pure formatters and a small dispatch enum.
//!
//! The LSP `hover()` handler delegates to this module after its Salsa lookups:
//! caller-side code resolves whatever extra data is needed (file paths, env
//! values, route definitions, property declarations) and hands it to one of
//! the `format_*` functions here. Formatters are pure — given the same
//! arguments they always produce the same markdown.
//!
//! The [`HoverTarget`] enum lets `Backend::hover()` route both Salsa-indexed
//! Laravel patterns and ad-hoc Blade variables through a single dispatch
//! point. Blade variables aren't part of [`crate::salsa_impl::PatternAtPosition`]
//! because they're identified by line scanning rather than by Salsa
//! extraction, but functionally they're the same kind of "thing the cursor is
//! sitting on" — modelling them as peer variants of the same enum keeps the
//! handler honest.
//!
//! # Output format
//!
//! Every hover follows the intelephense-style layout: a **bold header line**
//! naming the construct, an optional description paragraph, an optional code
//! block showing the actual declaration / value, optional rendered PHPDoc
//! tags, and an ``at `path:line` `` bottom-line source reference. This
//! matches what `intelephense` itself produces in Zed for PHP language
//! constructs, so the two sets of hovers feel consistent next to each other.

use crate::livewire_resolver::extract_blade_variable_at_cursor;
use crate::php_class::PropertyDeclaration;
use crate::route_discovery::RouteDefinition;
use crate::salsa_impl::{ParsedPatternsData, PatternAtPosition};

/// Anything the cursor might be hovering. Pattern variants come straight from
/// the Salsa position index; the Blade-variable variant is extracted by line
/// scanning, and only matters in `.blade.php` files.
pub enum HoverTarget {
    Pattern(PatternAtPosition),
    BladeVariable {
        var_name: String,
        property: Option<String>,
    },
}

/// Decide what (if anything) the cursor is on. Patterns take precedence;
/// Blade-variable extraction is only attempted on Blade files when no pattern
/// matched. Returns `None` when neither lookup finds something hoverable.
pub fn find_hover_target(
    patterns: &ParsedPatternsData,
    line_text: &str,
    line: u32,
    column: u32,
    is_blade: bool,
) -> Option<HoverTarget> {
    if let Some(p) = patterns.find_at_position(line, column) {
        return Some(HoverTarget::Pattern(p));
    }
    if is_blade {
        if let Some((var_name, property)) = extract_blade_variable_at_cursor(line_text, column) {
            return Some(HoverTarget::BladeVariable { var_name, property });
        }
    }
    None
}

// ============================================================================
// Laravel-pattern formatters
//
// All Laravel constructs use the same shape:
//
//   **<call-site form>**
//
//   <optional value / signature line — inline markdown or php code fence>
//
//   at `<source path>[:line]`
//
// `<call-site form>` is what you'd literally type in PHP/Blade to reference
// the thing — `view('users.profile')`, `route('users.show')`, `<x-button>`,
// `__('validation.required')`. Matches intelephense's "show the user what
// they're hovering, formatted the way they'd write it."
// ============================================================================

/// Hover for a view reference. `snippet` is an optional excerpt from the
/// resolved view file (typically the `@props([...])` declaration) — when
/// provided, rendered as a ```php fenced block above the source line so the
/// reader can see at-a-glance what variables the view expects.
pub fn format_view(
    name: &str,
    resolved_display_path: Option<&str>,
    snippet: Option<&str>,
) -> String {
    let header = format!("**view('{}')**", name);
    let code = snippet.map(php_block);
    finish(
        &header,
        None,
        code,
        resolved_display_path,
        missing_file_note(resolved_display_path),
    )
}

/// Hover for a Blade component reference. `tag_name` already carries the
/// `x-` prefix as captured by the tree-sitter query — do not re-add it.
/// `snippet` mirrors [`format_view`]: typically the `@props([...])` line.
pub fn format_component(
    tag_name: &str,
    resolved_display_path: Option<&str>,
    snippet: Option<&str>,
) -> String {
    let header = format!("**`<{}>`**", tag_name);
    let code = snippet.map(php_block);
    finish(
        &header,
        None,
        code,
        resolved_display_path,
        missing_file_note(resolved_display_path),
    )
}

/// Hover for a Livewire component reference. `snippet` is typically the
/// `class Foo extends Component` signature line so the reader sees the
/// component class without leaving the hover.
pub fn format_livewire(
    name: &str,
    resolved_display_path: Option<&str>,
    snippet: Option<&str>,
) -> String {
    let header = format!("**`<livewire:{}>`**", name);
    let code = snippet.map(php_block);
    finish(
        &header,
        None,
        code,
        resolved_display_path,
        missing_file_note(resolved_display_path),
    )
}

/// Hover for a route reference. `source_display` is the markdown link for
/// the file:line of the `->name(...)` callsite. `snippet` is typically the
/// actual `Route::verb(...)->name(...)` source line — rendered as a ```php
/// block above the source line.
pub fn format_route(
    name: &str,
    def: Option<&RouteDefinition>,
    source_display: Option<&str>,
    snippet: Option<&str>,
) -> String {
    let header = format!("**route('{}')**", name);
    let Some(d) = def else {
        return finish(
            &header,
            None,
            None,
            None,
            Some("*(route not found in index)*"),
        );
    };
    let method = d
        .method
        .as_deref()
        .map(|m| m.to_uppercase())
        .unwrap_or_else(|| "?".to_string());
    let uri = d.uri.as_deref().unwrap_or("?");
    let action = d.action.as_deref().unwrap_or("?");
    let detail = format!("`{} {}` → `{}`", method, uri, action);
    let code = snippet.map(php_block);
    finish(&header, Some(detail), code, source_display, None)
}

/// Hover for a `config('app.name')` reference. Resolved value is shown as a
/// PHP code block (most config values are PHP expressions like
/// `env('APP_NAME', 'Laravel')`).
pub fn format_config(key: &str, resolved_value: Option<&str>, source_file: Option<&str>) -> String {
    let header = format!("**config('{}')**", key);
    let code = resolved_value.map(|v| php_block(truncate_for_display(v, 200).as_str()));
    let trailer = if resolved_value.is_none() {
        Some("*(value not found)*")
    } else {
        None
    };
    finish(&header, None, code, source_file, trailer)
}

/// Hover for an `env('APP_NAME')` reference. The value is shown as a plain
/// (no-language) code block since `.env` values are literal strings.
pub fn format_env(name: &str, env_var: Option<EnvHoverInput<'_>>) -> String {
    let header = format!("**env('{}')**", name);
    match env_var {
        Some(EnvHoverInput {
            is_commented: true,
            source_file,
            ..
        }) => finish(
            &header,
            Some("*(commented out)*".to_string()),
            None,
            source_file,
            None,
        ),
        Some(EnvHoverInput {
            value, source_file, ..
        }) => {
            let code = plain_block(value);
            finish(&header, None, Some(code), source_file, None)
        }
        None => finish(&header, None, None, None, Some("*(not defined in .env)*")),
    }
}

/// Lightweight view over an env-var record — just the fields hover cares about.
/// Avoids coupling [`format_env`] to any particular struct in the salsa layer.
#[derive(Debug, Clone, Copy)]
pub struct EnvHoverInput<'a> {
    pub value: &'a str,
    pub is_commented: bool,
    /// The .env file the value was read from (`.env`, `.env.local`, etc.) —
    /// rendered as a display-friendly path. `None` to omit the source line.
    pub source_file: Option<&'a str>,
}

/// Hover for a `__('validation.required')` / `__('Welcome')` reference. The
/// translated string is rendered as a plain code block — it's content, not
/// PHP code.
pub fn format_translation(
    key: &str,
    resolved_value: Option<&str>,
    source_file: Option<&str>,
) -> String {
    let header = format!("**__('{}')**", key);
    let code = resolved_value.map(|v| {
        // Strip outer quotes from the PHP-literal representation
        // (`'foo'` → `foo`) for nicer in-block display.
        let v = v.trim();
        let unquoted = v
            .strip_prefix('\'')
            .and_then(|s| s.strip_suffix('\''))
            .or_else(|| v.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
            .unwrap_or(v);
        plain_block(&truncate_for_display(unquoted, 200))
    });
    let trailer = if resolved_value.is_none() {
        Some("*(translation not found for default locale)*")
    } else {
        None
    };
    finish(&header, None, code, source_file, trailer)
}

/// Hover for a `->middleware('auth')` reference. `class_fqn` is the fully
/// qualified class name behind the alias.
pub fn format_middleware(
    alias: &str,
    class_fqn: Option<&str>,
    source_file: Option<&str>,
) -> String {
    let header = format!("**middleware('{}')**", alias);
    let code = class_fqn.map(php_block);
    let trailer = if class_fqn.is_none() {
        Some("*(alias not registered)*")
    } else {
        None
    };
    finish(&header, None, code, source_file, trailer)
}

/// Hover for an `app('cache')` / `App::make('cache')` reference. Concrete
/// class shown as a PHP code block.
pub fn format_binding(alias: &str, class_fqn: Option<&str>, source_file: Option<&str>) -> String {
    let header = format!("**app('{}')**", alias);
    let code = class_fqn.map(php_block);
    let trailer = if class_fqn.is_none() {
        Some("*(binding not registered)*")
    } else {
        None
    };
    finish(&header, None, code, source_file, trailer)
}

/// Hover for an asset reference — `asset('css/app.css')`,
/// `Vite::asset('resources/js/app.js')`, `mix('app.css')`, `public_path('x')`,
/// etc. `helper_label` is the user-facing function/method call form so the
/// bold header reads like what the developer wrote.
pub fn format_asset(path: &str, helper_label: &str, resolved_display_path: Option<&str>) -> String {
    let header = format!("**{}('{}')**", helper_label, path);
    finish(
        &header,
        None,
        None,
        resolved_display_path,
        missing_file_note(resolved_display_path),
    )
}

/// Hover for a `url('/path')` reference. Resolves relative to `public/`.
pub fn format_url(path: &str, resolved_display_path: Option<&str>) -> String {
    let header = format!("**url('{}')**", path);
    finish(
        &header,
        None,
        None,
        resolved_display_path,
        missing_file_note(resolved_display_path),
    )
}

// ============================================================================
// Blade variable formatter — the intelephense-style flagship
// ============================================================================

/// Hover for a Blade variable reference (`$user`, `$user->email`). Renders in
/// intelephense's hover style: bold `Class::$prop` header, PHPDoc description
/// paragraph, ```php block with the actual declaration line, then PHPDoc tag
/// lines (when present), then source location.
///
/// Falls back to simpler shapes when class-side data isn't available.
pub fn format_blade_variable(input: &BladeVariableHover<'_>) -> String {
    let header = blade_variable_header(input);
    let description = input.declaration.and_then(|d| d.description.clone());
    let code = input.declaration.map(|d| php_block(&d.declaration_text));

    let mut tags_section: Option<String> = None;
    if let Some(decl) = input.declaration {
        if !decl.phpdoc_tags.is_empty() {
            let lines: Vec<String> = decl
                .phpdoc_tags
                .iter()
                .map(|t| format!("*{}*", t))
                .collect();
            tags_section = Some(lines.join("\n\n"));
        }
    }

    let mut out = header;
    if let Some(d) = description {
        out.push_str("\n\n");
        out.push_str(&d);
    }
    if let Some(c) = code {
        out.push_str("\n\n");
        out.push_str(&c);
    }
    if let Some(t) = tags_section {
        out.push_str("\n\n");
        out.push_str(&t);
    }
    if let Some(src) = input.defined_in {
        out.push_str(&format!("\n\nat {}", src));
    }
    out
}

/// Caller-supplied inputs for [`format_blade_variable`]. Building a struct
/// rather than threading six positional `Option<&str>` arguments through.
#[derive(Debug, Default)]
pub struct BladeVariableHover<'a> {
    pub var_name: &'a str,
    pub property: Option<&'a str>,
    /// Resolved variable type — class FQN (e.g. `App\Models\User`) or a
    /// primitive type name (`mixed`, `string`). [`is_class_like_type`]
    /// disambiguates.
    pub var_type: Option<&'a str>,
    /// Property declaration + PHPDoc, when the variable's type resolved to a
    /// class and we could find the property on it.
    pub declaration: Option<&'a PropertyDeclaration>,
    /// Display path of the defining file (relative to project root,
    /// optionally with `:line` suffix).
    pub defined_in: Option<&'a str>,
}

/// Build the bold header line for a Blade variable hover. Picks between
/// class-qualified (`**App\Models\User::$email**`) and `$var->prop`
/// (`**$user->email**`) forms based on whether we have a class-like parent
/// type to qualify with.
fn blade_variable_header(input: &BladeVariableHover<'_>) -> String {
    match (input.var_type, input.property) {
        // Have both a class-like parent and a property — intelephense shape.
        (Some(class), Some(prop)) if is_class_like_type(class) => {
            format!("**{}::${}**", class, prop)
        }
        // Property access but parent is a primitive / unknown — fall back to
        // `$var->prop` form. Avoids weird strings like `mixed::$prop`.
        (_, Some(prop)) => format!("**$`{}->{}`**", input.var_name, prop),
        // No property, class-like type known — render as a typed variable.
        (Some(class), None) if is_class_like_type(class) => {
            format!("**${}** : `{}`", input.var_name, class)
        }
        // No property, primitive type — show the type next to the variable.
        (Some(prim), None) => format!("**${}** : `{}`", input.var_name, prim),
        // Bare variable, no type info.
        (None, None) => format!("**${}**", input.var_name),
    }
}

/// Heuristic: a type string represents a class (rather than a PHP primitive)
/// if it contains a namespace separator OR its first character is an
/// uppercase ASCII letter. Catches `App\Models\User`, `Carbon`, `Collection`
/// while excluding `mixed`, `string`, `int`, `?int`, `null`, etc.
///
/// `pub` because the LSP server uses this predicate to decide whether to
/// run a `find_php_class_file` lookup on a resolved variable type — calling
/// it for primitive sentinels like `"mixed"` always misses.
pub fn is_class_like_type(t: &str) -> bool {
    let t = t.trim_start_matches('?').trim_start_matches('\\');
    if t.contains('\\') {
        return true;
    }
    t.chars()
        .next()
        .map(|c| c.is_ascii_uppercase())
        .unwrap_or(false)
}

// ============================================================================
// Internals — code blocks, finish-assembly, truncation
// ============================================================================

/// PHP-tagged fenced code block. Zed renders these with PHP syntax
/// highlighting in hover.
fn php_block(content: &str) -> String {
    format!("```php\n{}\n```", content)
}

/// Plain (no-language) fenced code block. Use for raw values where PHP
/// syntax highlighting would be misleading (env values, translated strings).
fn plain_block(content: &str) -> String {
    format!("```\n{}\n```", content)
}

/// Assemble the standard Laravel-pattern hover shape:
/// header / [detail-line] / [code-block] / [at-path] / [trailer].
fn finish(
    header: &str,
    detail: Option<String>,
    code: Option<String>,
    source_display: Option<&str>,
    trailer: Option<&str>,
) -> String {
    let mut out = header.to_string();
    if let Some(d) = detail {
        out.push_str("\n\n");
        out.push_str(&d);
    }
    if let Some(c) = code {
        out.push_str("\n\n");
        out.push_str(&c);
    }
    if let Some(s) = source_display {
        out.push_str(&format!("\n\nat {}", s));
    }
    if let Some(t) = trailer {
        out.push_str("\n\n");
        out.push_str(t);
    }
    out
}

/// Standard `*(file not found)*` trailer when no resolved path was available.
fn missing_file_note(resolved_display_path: Option<&str>) -> Option<&'static str> {
    if resolved_display_path.is_none() {
        Some("*(file not found)*")
    } else {
        None
    }
}

/// Build the markdown link string used for the `at <link>` source-line at
/// the bottom of every hover. The label is the display path (relative to the
/// project root, optionally with `:line`); the URL is a `file://` URI that
/// Zed and other LSP clients resolve to "open this file at this line".
///
/// Caller is expected to pre-resolve the absolute file URL via
/// [`tower_lsp::lsp_types::Url::from_file_path`] so percent-encoding for
/// spaces and other URL-unsafe path bytes is handled correctly.
pub fn source_link(display: &str, file_url: &str, line: Option<u32>) -> String {
    match line {
        Some(l) => format!("[`{}:{}`]({}#L{})", display, l, file_url, l),
        None => format!("[`{}`]({})", display, file_url),
    }
}

/// Truncate strings longer than `limit` chars with a `…` ellipsis. Operates
/// on chars (not bytes) so it never splits a multibyte character.
fn truncate_for_display(s: &str, limit: usize) -> String {
    if s.chars().count() <= limit {
        return s.to_string();
    }
    let head: String = s.chars().take(limit).collect();
    format!("{}…", head)
}

#[cfg(test)]
mod tests;
