//! Shared markdown template for LSP completion documentation panels.
//!
//! Every completion source in this LSP — Builder methods, columns,
//! relations, routes, config keys, translations, views, components, etc. —
//! renders its documentation panel through this one module so the popups
//! look consistent and match the structure Intelephense uses for PHP
//! symbols.
//!
//! ## The structure
//!
//! Modelled on Intelephense's panel. Sections are separated by a blank
//! line and emitted only when present:
//!
//! ```text
//! <header>                      ← qualified identifier, plain text
//!                                 (e.g. Model::with, users.email, app.name)
//!
//! <summary>                     ← one-line prose description
//!
//! ```<lang>                     ← fenced code block: signature, SQL DDL,
//! <code>                          config value, route definition, …
//! ```
//!
//! <section>                     ← zero or more trailing blocks: PHPDoc
//! <section>                       @tags, key/value metadata, file paths
//! ```
//!
//! Not every consumer fills every slot. A column completion has no PHPDoc
//! `@param` tags; a config key has no signature. The builder skips absent
//! parts and never emits stray blank lines.
//!
//! ## Why a builder instead of format strings
//!
//! Each call site assembles its own data, but they all funnel through
//! [`CompletionDoc::render`] / [`CompletionDoc::into_documentation`], so
//! the spacing and code-fence rules live in exactly one place. Change the
//! house style here and every popup updates.

use tower_lsp::lsp_types::{Documentation, MarkupContent, MarkupKind};

/// A fenced code block in a documentation panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeBlock {
    /// Fence info-string language tag (`php`, `sql`, `json`, …). Drives
    /// the editor's syntax highlighting inside the panel.
    pub language: String,
    /// Raw code to place between the fences. Rendered verbatim — the
    /// caller is responsible for any normalisation (e.g. collapsing a
    /// multi-line signature to one line).
    pub code: String,
}

impl CodeBlock {
    pub fn new(language: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            language: language.into(),
            code: code.into(),
        }
    }
}

/// Structured input for a completion item's documentation panel. Build
/// with the chained setters, then call [`render`](Self::render) for the
/// markdown string or [`into_documentation`](Self::into_documentation)
/// for the LSP `Documentation` value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompletionDoc {
    header: Option<String>,
    summary: Option<String>,
    code: Option<CodeBlock>,
    sections: Vec<String>,
    /// When set, `@param` / `@return` / `@throws` / `@var` / `@property*`
    /// tags rendered in the panel get their type slots rewritten so
    /// `$this`/`self`/`static` resolve to `<basename>::<static>` form.
    /// Matches how the row's `detail` field is displayed.
    self_resolution_class: Option<String>,
}

impl CompletionDoc {
    pub fn new() -> Self {
        Self::default()
    }

    /// The qualified identifier shown on the first line — `Model::with`,
    /// `users.email`, `app.name`, `route('home')`, etc. Plain text;
    /// matches how Intelephense leads its panels.
    pub fn header(mut self, header: impl Into<String>) -> Self {
        let h = header.into();
        if !h.is_empty() {
            self.header = Some(h);
        }
        self
    }

    /// One-line prose summary. For methods this is the PHPDoc summary; for
    /// other items it's a short description ("Route definition", a config
    /// value, the translated string, …).
    pub fn summary(mut self, summary: impl Into<String>) -> Self {
        let s = summary.into();
        if !s.is_empty() {
            self.summary = Some(s);
        }
        self
    }

    /// Same as [`summary`](Self::summary) but takes an `Option`, so call
    /// sites with optional docblocks don't need an `if let`.
    pub fn summary_opt(mut self, summary: Option<String>) -> Self {
        if let Some(s) = summary {
            if !s.is_empty() {
                self.summary = Some(s);
            }
        }
        self
    }

    /// The fenced code block — a method signature, a column's SQL type, a
    /// config value's PHP literal, etc.
    pub fn code(mut self, code: CodeBlock) -> Self {
        if !code.code.is_empty() {
            self.code = Some(code);
        }
        self
    }

    /// Optional-taking variant of [`code`](Self::code).
    pub fn code_opt(mut self, code: Option<CodeBlock>) -> Self {
        if let Some(c) = code {
            if !c.code.is_empty() {
                self.code = Some(c);
            }
        }
        self
    }

    /// Append a trailing block — a PHPDoc `@tag` line, a metadata row, a
    /// file path. Each section renders as its own paragraph (blank line
    /// before it). Empty strings are ignored.
    pub fn section(mut self, section: impl Into<String>) -> Self {
        let s = section.into();
        if !s.is_empty() {
            self.sections.push(s);
        }
        self
    }

    /// Set the class name used to resolve `$this`/`self`/`static`
    /// references inside `@param` / `@return` / etc. tags. Without this,
    /// those keywords render verbatim (`@return $this`); with it, they
    /// render as `<class basename><static>` (e.g.
    /// ``@return `Builder<static>` ``), matching the row's `detail`
    /// column.
    pub fn resolve_self_for(mut self, class: impl Into<String>) -> Self {
        let c = class.into();
        if !c.is_empty() {
            self.self_resolution_class = Some(c);
        }
        self
    }

    /// Append many sections at once. Empty strings are skipped.
    pub fn sections<I, S>(mut self, sections: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for s in sections {
            let s = s.into();
            if !s.is_empty() {
                self.sections.push(s);
            }
        }
        self
    }

    /// `true` when nothing has been set — the panel would be empty. Call
    /// sites can use this to decide whether to attach documentation at all.
    pub fn is_empty(&self) -> bool {
        self.header.is_none()
            && self.summary.is_none()
            && self.code.is_none()
            && self.sections.is_empty()
    }

    /// Render to a markdown string. Sections are joined with a single
    /// blank line; absent parts produce no output and no stray blank
    /// lines.
    ///
    /// Formatting rules applied here so every call site gets them for free:
    ///
    /// - **Header is bolded** (`**…**`) — matches how Intelephense renders
    ///   its qualified-identifier first line.
    /// - **PHP code blocks are seeded with `<?php`** — Zed (and most other
    ///   markdown renderers) only kick in PHP syntax highlighting once the
    ///   PHP open tag is present in the fenced block.
    /// - **Sections starting with `@` are formatted as PHPDoc tags** — type
    ///   references get wrapped in inline-code backticks so `array|string`
    ///   renders as `` `array|string` ``, matching Intelephense's style.
    pub fn render(&self) -> String {
        let mut blocks: Vec<String> = Vec::new();

        if let Some(header) = &self.header {
            blocks.push(format!("**{}**", header));
        }
        if let Some(summary) = &self.summary {
            blocks.push(summary.clone());
        }
        if let Some(code) = &self.code {
            let body = if code.language == "php" && !code.code.trim_start().starts_with("<?php") {
                // Seed `<?php` directly above the signature — no blank
                // line between them. The open tag is purely to trigger
                // syntax highlighting; visually it should read as one
                // continuous block.
                format!("<?php\n{}", code.code)
            } else {
                code.code.clone()
            };
            blocks.push(format!("```{}\n{}\n```", code.language, body));
        }
        for section in &self.sections {
            if section.trim_start().starts_with('@') {
                blocks.push(format_phpdoc_tag_with(
                    section,
                    self.self_resolution_class.as_deref(),
                ));
            } else {
                blocks.push(section.clone());
            }
        }

        blocks.join("\n\n")
    }

    /// Render and wrap as an LSP `Documentation::MarkupContent` with
    /// markdown kind. This is what completion items carry in their
    /// `documentation` field.
    pub fn into_documentation(self) -> Documentation {
        Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: self.render(),
        })
    }
}

/// Rewrite `$this`, `self`, and `static` (as whole-type tokens) into
/// `<basename of entry_class><static>` — the conventional display form
/// Intelephense uses for "an instance of the actual runtime class."
///
/// Operates over union/intersection types so `$this|null` becomes
/// `Builder<static>|null`. Types that don't reference self-keywords
/// pass through unchanged.
pub fn resolve_self_type(raw: &str, entry_class: &str) -> String {
    let basename = entry_class.rsplit('\\').next().unwrap_or(entry_class);
    let replacement = format!("{basename}<static>");

    // Split on union/intersection separators while preserving them.
    let mut out = String::with_capacity(raw.len());
    let mut current = String::new();
    for c in raw.chars() {
        match c {
            '|' | '&' => {
                out.push_str(&rewrite_self_token(&current, &replacement));
                out.push(c);
                current.clear();
            }
            _ => current.push(c),
        }
    }
    out.push_str(&rewrite_self_token(&current, &replacement));
    out
}

fn rewrite_self_token(token: &str, replacement: &str) -> String {
    match token.trim() {
        "$this" | "self" | "static" => replacement.to_string(),
        _ => token.to_string(),
    }
}

/// Wrap the type portion of a PHPDoc `@tag` line in inline-code backticks.
///
/// Recognises the common Laravel-flavour tag shapes:
///
/// - `@param TYPE $name [description]` → ``@param `TYPE` $name [description]``
/// - `@return TYPE [description]`      → ``@return `TYPE` [description]``
/// - `@throws TYPE [description]`      → ``@throws `TYPE` [description]``
/// - `@var TYPE [$name] [description]` → ``@var `TYPE` [$name] [description]``
///
/// Tags without a recognisable type slot (`@deprecated`, `@internal`,
/// `@inheritdoc`, …) pass through unchanged. Already-backticked types
/// aren't double-wrapped.
///
/// Type detection is whitespace-based: the type is whatever comes between
/// the tag keyword and the next space (or the `$variable` for `@param` /
/// `@var`). Laravel's PHPDoc types don't contain unescaped spaces — even
/// complex generics like `array<int, string>` use commas, not spaces — so
/// this works without a real type-grammar parser.
pub fn format_phpdoc_tag(tag: &str) -> String {
    format_phpdoc_tag_with(tag, None)
}

/// Same as [`format_phpdoc_tag`] but also resolves `$this`/`self`/`static`
/// in the type slot when `entry_class` is provided. Used by panel
/// rendering for method completions so `@return $this` becomes
/// ``@return `Builder<static>` `` — matching the row's `detail` field
/// and Intelephense's display convention.
pub fn format_phpdoc_tag_with(tag: &str, entry_class: Option<&str>) -> String {
    let tag = tag.trim();
    let Some((keyword, rest)) = tag.split_once(char::is_whitespace) else {
        return tag.to_string();
    };
    let rest = rest.trim_start();
    if rest.is_empty() {
        return tag.to_string();
    }

    let typed_tag = matches!(
        keyword,
        "@param" | "@return" | "@throws" | "@var" | "@property" | "@property-read" | "@property-write"
    );
    if !typed_tag {
        return tag.to_string();
    }

    // For @param / @var / @property* the type is whatever comes before the
    // `$name`. For @return / @throws there's no `$name` — the type is the
    // first whitespace-delimited token and any tail is description.
    let needs_var = matches!(
        keyword,
        "@param" | "@var" | "@property" | "@property-read" | "@property-write"
    );

    let (type_str, tail) = if needs_var {
        match rest.find('$') {
            Some(idx) => (rest[..idx].trim_end(), &rest[idx..]),
            // No `$var` found — fall back to first-token split so we still
            // wrap *something*; better than leaving the whole thing bare.
            None => split_first_token(rest),
        }
    } else {
        split_first_token(rest)
    };

    if type_str.is_empty() {
        return tag.to_string();
    }
    if type_str.starts_with('`') && type_str.ends_with('`') {
        // Already wrapped — leave as-is.
        return tag.to_string();
    }

    // Resolve self-references if a context class was supplied.
    let resolved_type: String = match entry_class {
        Some(class) => resolve_self_type(type_str, class),
        None => type_str.to_string(),
    };

    if tail.is_empty() {
        format!("{} `{}`", keyword, resolved_type)
    } else {
        format!("{} `{}` {}", keyword, resolved_type, tail)
    }
}

/// Split `s` into (first whitespace-delimited token, rest). The rest has
/// its leading whitespace trimmed; both pieces are `""` when `s` is empty.
fn split_first_token(s: &str) -> (&str, &str) {
    match s.find(char::is_whitespace) {
        Some(idx) => (&s[..idx], s[idx..].trim_start()),
        None => (s, ""),
    }
}

/// Split a stripped PHPDoc body into its summary (leading prose) and its
/// `@tag` lines. The summary is every line up to the first `@tag` or blank
/// line, joined with spaces; tags are each `@…` line as its own string.
///
/// Mirrors how Intelephense separates the description from the
/// `@param` / `@return` / `@throws` block. Returns `(summary, tags)` where
/// either may be empty.
pub fn split_phpdoc(stripped_doc: &str) -> (Option<String>, Vec<String>) {
    let mut summary_lines: Vec<&str> = Vec::new();
    let mut tags: Vec<String> = Vec::new();
    let mut in_tags = false;

    for raw in stripped_doc.lines() {
        let line = raw.trim();
        if line.starts_with('@') {
            in_tags = true;
            tags.push(line.to_string());
            continue;
        }
        if in_tags {
            // A continuation line of a multi-line tag — append to the last
            // tag so wrapped @param descriptions stay attached.
            if !line.is_empty() {
                if let Some(last) = tags.last_mut() {
                    last.push(' ');
                    last.push_str(line);
                }
            }
            continue;
        }
        // Still in the summary section.
        if line.is_empty() {
            // Blank line ends the summary only if we've already collected
            // some — leading blanks are skipped.
            if !summary_lines.is_empty() {
                in_tags = true;
            }
            continue;
        }
        summary_lines.push(line);
    }

    let summary = if summary_lines.is_empty() {
        None
    } else {
        Some(summary_lines.join(" "))
    };
    (summary, tags)
}

#[cfg(test)]
mod tests;
