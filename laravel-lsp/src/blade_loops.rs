//! Blade loop-block parsing.
//!
//! Extracts `@foreach` / `@forelse` / `@for` / `@while` block boundaries and the
//! variables they introduce, so the LSP can do scope-aware variable resolution
//! inside Blade templates.
//!
//! Types are defined here (rather than in main.rs) so the Salsa actor in
//! `salsa_impl` can return them from a tracked query.

use lazy_static::lazy_static;
use regex::Regex;

/// Represents a loop block in a Blade file for scope-aware variable resolution.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BladeLoopBlock {
    /// The type of loop directive (foreach, forelse, for, while)
    pub loop_type: BladeLoopType,
    /// Variables introduced by this loop (e.g., `$item`, `$key` from `@foreach`).
    /// Tuple is (name_without_dollar, php_type_hint).
    pub variables: Vec<(String, String)>,
    /// Iterable expression (left of `as` for foreach/forelse), e.g. `$this->audits`.
    pub iterable: Option<String>,
    /// Start line (0-indexed).
    pub start_line: usize,
    /// End line (0-indexed). `None` if the loop is unclosed (cursor still inside).
    pub end_line: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BladeLoopType {
    Foreach,
    Forelse,
    For,
    While,
}

/// Parse variables from `@foreach` / `@forelse` directive arguments.
/// Handles: `@foreach($items as $item)`, `@foreach($items as $key => $value)`,
/// `@foreach($category->items as $item)`.
pub fn parse_foreach_variables(arguments: &str) -> Vec<(String, String)> {
    lazy_static! {
        static ref FOREACH_RE: Regex =
            Regex::new(r#"\([^)]+\s+as\s+(?:\$(\w+)\s*=>\s*)?\$(\w+)\s*\)"#).unwrap();
    }

    let mut vars = Vec::new();
    if let Some(caps) = FOREACH_RE.captures(arguments) {
        if let Some(key_match) = caps.get(1) {
            vars.push((key_match.as_str().to_string(), "mixed".to_string()));
        }
        if let Some(value_match) = caps.get(2) {
            vars.push((value_match.as_str().to_string(), "mixed".to_string()));
        }
    }
    vars
}

/// Parse the iterable expression from `@foreach` / `@forelse` arguments.
/// e.g. `($this->audits as $audit)` -> `Some("$this->audits")`.
pub fn parse_foreach_iterable(arguments: &str) -> Option<String> {
    lazy_static! {
        static ref ITER_RE: Regex = Regex::new(r#"\(\s*(.+?)\s+as\s+\$"#).unwrap();
    }

    ITER_RE
        .captures(arguments)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().to_string())
}

/// Parse variables from `@for` directive arguments.
/// Handles: `@for($i = 0; $i < 10; $i++)`.
pub fn parse_for_variables(arguments: &str) -> Vec<(String, String)> {
    lazy_static! {
        static ref FOR_RE: Regex = Regex::new(r#"\(\s*\$(\w+)\s*="#).unwrap();
    }

    let mut vars = Vec::new();
    if let Some(caps) = FOR_RE.captures(arguments) {
        if let Some(var_match) = caps.get(1) {
            vars.push((var_match.as_str().to_string(), "int".to_string()));
        }
    }
    vars
}

/// Find all loop blocks in Blade content and their boundaries.
/// Returns a list of loop blocks with start/end lines and extracted variables.
pub fn find_loop_blocks(content: &str) -> Vec<BladeLoopBlock> {
    lazy_static! {
        static ref LOOP_START_RE: Regex =
            Regex::new(r#"@(foreach|forelse|for|while)\s*(\([^)]*\))"#).unwrap();
        static ref LOOP_END_RE: Regex =
            Regex::new(r#"@(endforeach|endforelse|endfor|endwhile)"#).unwrap();
    }

    let mut blocks = Vec::new();
    let mut open_loops: Vec<(BladeLoopType, Vec<(String, String)>, Option<String>, usize)> =
        Vec::new();

    for (line_idx, line) in content.lines().enumerate() {
        for caps in LOOP_START_RE.captures_iter(line) {
            let directive = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let arguments = caps.get(2).map(|m| m.as_str()).unwrap_or("");

            let (loop_type, variables, iterable) = match directive {
                "foreach" => (
                    BladeLoopType::Foreach,
                    parse_foreach_variables(arguments),
                    parse_foreach_iterable(arguments),
                ),
                "forelse" => (
                    BladeLoopType::Forelse,
                    parse_foreach_variables(arguments),
                    parse_foreach_iterable(arguments),
                ),
                "for" => (BladeLoopType::For, parse_for_variables(arguments), None),
                "while" => (BladeLoopType::While, Vec::new(), None),
                _ => continue,
            };

            open_loops.push((loop_type, variables, iterable, line_idx));
        }

        for caps in LOOP_END_RE.captures_iter(line) {
            let end_directive = caps.get(1).map(|m| m.as_str()).unwrap_or("");

            let expected_type = match end_directive {
                "endforeach" => Some(BladeLoopType::Foreach),
                "endforelse" => Some(BladeLoopType::Forelse),
                "endfor" => Some(BladeLoopType::For),
                "endwhile" => Some(BladeLoopType::While),
                _ => None,
            };

            if let Some(expected) = expected_type {
                if let Some(pos) = open_loops.iter().rposition(|(t, _, _, _)| *t == expected) {
                    let (loop_type, variables, iterable, start_line) = open_loops.remove(pos);
                    blocks.push(BladeLoopBlock {
                        loop_type,
                        variables,
                        iterable,
                        start_line,
                        end_line: Some(line_idx),
                    });
                }
            }
        }
    }

    // Add any unclosed loops (cursor might be inside them)
    for (loop_type, variables, iterable, start_line) in open_loops {
        blocks.push(BladeLoopBlock {
            loop_type,
            variables,
            iterable,
            start_line,
            end_line: None,
        });
    }

    blocks
}

/// Get all loop blocks that enclose the given cursor position.
/// Returns loops ordered innermost-first.
pub fn get_enclosing_loops(content: &str, cursor_line: usize) -> Vec<BladeLoopBlock> {
    let blocks = find_loop_blocks(content);

    let mut enclosing: Vec<BladeLoopBlock> = blocks
        .into_iter()
        .filter(|block| {
            let after_start = cursor_line > block.start_line;
            let before_end = match block.end_line {
                Some(end) => cursor_line < end,
                None => true,
            };
            after_start && before_end
        })
        .collect();

    enclosing.sort_by_key(|b| std::cmp::Reverse(b.start_line));
    enclosing
}
