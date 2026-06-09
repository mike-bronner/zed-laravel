//! Hover content — a single rendering template plus the dispatch enum.
//!
//! The LSP `hover()` handler delegates to this module after its Salsa lookups:
//! caller-side code resolves whatever data each pattern needs (file paths,
//! env values, route definitions, property declarations, class FQNs) and
//! hands them to [`render`] via a [`HoverContent`] struct. Sections of the
//! template that aren't supplied are simply omitted — the same template
//! covers every pattern (view, route, env, Blade variable on property, …)
//! purely by which fields the caller populates.
//!
//! Earlier revisions had per-pattern `format_*` functions; that was overkill
//! since the visual style is uniform across patterns. Pattern variation
//! lives entirely in *what data we pass*, not *how it renders*.
//!
//! The [`HoverTarget`] enum lets `Backend::hover()` route both Salsa-indexed
//! Laravel patterns and ad-hoc Blade variables through one dispatch.
//!
//! # Template
//!
//! Sections, rendered in order with `\n\n` separators (paragraph breaks in
//! markdown). Each section is omitted entirely when absent:
//!
//! 1. **Bold header** — typically a fully-qualified class name. Wrap-free
//!    text; [`render`] adds the `**…**` markdown.
//! 2. **Detail line** — short inline markdown beneath the header
//!    (e.g. `` `GET /uri` → `Controller@show` ``).
//! 3. **Description** — a paragraph of prose (PHPDoc summary, etc.).
//! 4. **Code block** — fenced code with language hint. PHP-tagged blocks get
//!    the `<?php` opener prepended so Zed's `tree-sitter-php` grammar can
//!    parse them (the standard grammar variant requires the opening tag).
//! 5. **Tag lines** — one italic line per PHPDoc tag (`@param`, `@return`).
//! 6. **Source link** — markdown link to the source location, rendered
//!    verbatim (no prefix, no extra backticks; caller builds the link).
//! 7. **Trailer** — italic note like `*(file not found)*`.

use crate::livewire_resolver::extract_blade_variable_at_cursor;
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
// The unified template
// ============================================================================

/// All data a hover can carry. Every field is optional — [`render`] omits
/// any section whose field is `None` / empty. Build one of these per
/// pattern at the dispatch site and call [`render`].
#[derive(Debug, Default, Clone)]
pub struct HoverContent<'a> {
    /// Bold header — typically a fully-qualified class name
    /// (e.g. `App\Livewire\Counter`). `**…**` wrapping is added by render.
    pub header: Option<&'a str>,
    /// Detail line under the header. Free-form inline markdown.
    pub detail: Option<&'a str>,
    /// Free-form description paragraph (e.g. PHPDoc summary).
    pub description: Option<&'a str>,
    /// Fenced code block with language hint.
    pub code: Option<CodeBlock<'a>>,
    /// Italic tag lines (`@param`, `@return`, `@throws`).
    pub tags: &'a [String],
    /// Pre-built markdown link string for the source location (e.g.
    /// `[app/Models/User.php:42](file:///abs/path)`). Rendered verbatim
    /// — no `at` prefix, no surrounding backticks.
    pub source_link: Option<&'a str>,
    /// Italic trailer (`*(file not found)*`, `*(commented out)*`).
    pub trailer: Option<&'a str>,
}

/// A fenced code block. [`CodeLanguage::Php`] auto-prepends `<?php\n` so
/// Zed's `tree-sitter-php` grammar (which requires the opening tag) parses
/// the snippet and applies highlighting.
#[derive(Debug, Clone, Copy)]
pub struct CodeBlock<'a> {
    pub language: CodeLanguage,
    pub content: &'a str,
}

#[derive(Debug, Clone, Copy)]
pub enum CodeLanguage {
    /// `php` fence with a `<?php\n` opener prepended.
    Php,
    /// Plain fence (no language tag) — for raw values, translated strings,
    /// `.env` content where PHP highlighting would be misleading.
    Plain,
}

/// Render a [`HoverContent`] into the final hover markdown. Sections are
/// emitted in the documented order, joined with `\n\n`. Returns an empty
/// string when every field is absent — caller should treat that as
/// "no hover".
pub fn render(content: &HoverContent<'_>) -> String {
    let mut sections: Vec<String> = Vec::new();

    if let Some(h) = content.header {
        sections.push(format!("**{}**", h));
    }
    if let Some(d) = content.detail {
        sections.push(d.to_string());
    }
    if let Some(d) = content.description {
        sections.push(d.to_string());
    }
    if let Some(code) = &content.code {
        let block = match code.language {
            CodeLanguage::Php => format!("```php\n<?php\n{}\n```", code.content),
            CodeLanguage::Plain => format!("```\n{}\n```", code.content),
        };
        sections.push(block);
    }
    if !content.tags.is_empty() {
        let tag_lines = content
            .tags
            .iter()
            .map(|t| format!("*{}*", t))
            .collect::<Vec<_>>()
            .join("\n\n");
        sections.push(tag_lines);
    }
    if let Some(link) = content.source_link {
        sections.push(link.to_string());
    }
    if let Some(t) = content.trailer {
        sections.push(t.to_string());
    }

    sections.join("\n\n")
}

/// Build a semantic hover card for a resolved magic member (M6) — the
/// Eloquent-magic sites Intelephense can't see through (`->active()` is a
/// scope, `$user->posts` a relationship, `$model->full_name` an accessor,
/// `$user->email` a column). `source_link` is a pre-built markdown link to the
/// declaring class, or `None` if it couldn't be located.
///
/// Returns an empty string for [`MagicMemberKind::PlainMember`] — a generic
/// property is Intelephense's job, and duplicating it would just add noise (the
/// multi-LSP dedup policy: suppress at the source).
pub fn magic_member_card(
    kind: crate::salsa_impl::MagicMemberKind,
    member: &str,
    declaring_fqcn: &str,
    confidence: crate::salsa_impl::Confidence,
    definition: Option<&str>,
    type_hint: Option<&str>,
    source_link: Option<&str>,
) -> String {
    use crate::salsa_impl::{Confidence, MagicMemberKind};
    let kind_label = match kind {
        MagicMemberKind::Scope => "Eloquent scope",
        MagicMemberKind::Accessor => "Eloquent accessor",
        MagicMemberKind::Relationship => "Eloquent relationship",
        MagicMemberKind::Column => "Database column",
        MagicMemberKind::DynamicFinder => "Dynamic finder",
        // Generic property — Intelephense already covers it. Don't duplicate.
        MagicMemberKind::PlainMember => return String::new(),
    };
    let detail = format!("`{member}` on `{declaring_fqcn}`");
    // For a column, the resolved PHP type (cast-aware) from the DB schema.
    let type_desc = type_hint.map(|t| format!("Type `{t}`"));
    // A MEDIUM-confidence resolution leaned on an inferred receiver type — flag
    // it so the reader knows it's a best-effort, not a static guarantee.
    let trailer = match confidence {
        Confidence::Medium => Some("*receiver type inferred*"),
        _ => None,
    };
    render(&HoverContent {
        header: Some(kind_label),
        detail: Some(&detail),
        description: type_desc.as_deref(),
        // The declaring method's source — for a relationship this reveals the
        // target model (`$this->belongsTo(Account::class)`), for a scope its
        // query body, for an accessor what it computes.
        code: definition.map(|d| CodeBlock {
            language: CodeLanguage::Php,
            content: d,
        }),
        source_link,
        trailer,
        ..Default::default()
    })
}

/// The declaring method names a magic-member usage name could map to, by kind.
/// Relationships/finders are accessed under their method name verbatim
/// (`$user->account` ← `account()`); scopes and accessors transform
/// (`active` ← `scopeActive`, `full_name` ← `getFullNameAttribute` or the
/// new-style `fullName(): Attribute`). Used to locate the declaration for the
/// hover snippet.
pub fn candidate_method_names(
    kind: crate::salsa_impl::MagicMemberKind,
    member: &str,
) -> Vec<String> {
    use crate::salsa_impl::MagicMemberKind;
    let pascal = crate::naming::snake_to_pascal(member);
    match kind {
        MagicMemberKind::Scope => vec![format!("scope{pascal}")],
        MagicMemberKind::Accessor => {
            // Old-style `get{Pascal}Attribute` + new-style camelCase method.
            let camel = {
                let mut c = pascal.chars();
                match c.next() {
                    Some(first) => first.to_ascii_lowercase().to_string() + c.as_str(),
                    None => String::new(),
                }
            };
            vec![format!("get{pascal}Attribute"), camel]
        }
        // Relationship / DynamicFinder / PlainMember: accessed by method name.
        _ => vec![member.to_string()],
    }
}

/// Slice a declaration's source (0-based `start_line..=end_line`) into a snippet
/// for a hover code block: dedents by the first line's indentation and caps
/// runaway bodies. Returns `""` if the range is out of bounds.
pub fn extract_member_snippet(source: &str, start_line: u32, end_line: u32) -> String {
    const MAX_LINES: usize = 20;
    let lines: Vec<&str> = source.lines().collect();
    let start = start_line as usize;
    if start >= lines.len() {
        return String::new();
    }
    let end = (end_line as usize).min(lines.len() - 1);
    if end < start {
        return String::new();
    }
    let slice = &lines[start..=end];
    let indent = slice
        .iter()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .unwrap_or(0);
    let mut out: Vec<String> = slice
        .iter()
        .map(|l| {
            if l.len() >= indent {
                l[indent..].to_string()
            } else {
                l.trim_start().to_string()
            }
        })
        .collect();
    if out.len() > MAX_LINES {
        out.truncate(MAX_LINES);
        out.push("// …".to_string());
    }
    out.join("\n")
}

// ============================================================================
// Caller utilities
// ============================================================================

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

/// Build the markdown link string used for the bottom-line source location.
/// The label is the display path (relative to the project root, optionally
/// with `:line`); the URL is a `file://` URI that Zed resolves to "open
/// this file at this line".
///
/// Label is rendered as plain markdown link text — NOT wrapped in backticks
/// — so it doesn't pick up the inline-code background/styling and looks
/// like a normal hyperlink.
///
/// Caller is expected to pre-resolve the absolute file URL via
/// [`tower_lsp::lsp_types::Url::from_file_path`] so percent-encoding for
/// spaces and other URL-unsafe path bytes is handled correctly.
pub fn source_link(display: &str, file_url: &str, line: Option<u32>) -> String {
    match line {
        Some(l) => format!("[{}:{}]({}#L{})", display, l, file_url, l),
        None => format!("[{}]({})", display, file_url),
    }
}

/// Truncate strings longer than `limit` chars with a `…` ellipsis. Operates
/// on chars (not bytes) so it never splits a multibyte character.
///
/// Used by config/translation dispatch code to clip long resolved values
/// before stuffing them into a code block.
pub fn truncate_for_display(s: &str, limit: usize) -> String {
    if s.chars().count() <= limit {
        return s.to_string();
    }
    let head: String = s.chars().take(limit).collect();
    format!("{}…", head)
}

#[cfg(test)]
mod tests;
