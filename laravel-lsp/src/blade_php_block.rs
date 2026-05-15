//! Parsing for `@php ... @endphp` blocks in Blade files.
//!
//! Extracts simple `$name = expression;` assignments so the LSP can recognize
//! variables that are introduced inline in Blade templates (not just from
//! Livewire / controllers / props).
//!
//! Type inference is intentionally lightweight:
//!   - `$x = $loop;` -> ("x", "Loop")
//!   - everything else -> ("x", "mixed")
//!
//! Defined in the library crate so the Salsa actor can memoize the result.

use lazy_static::lazy_static;
use regex::Regex;

/// Extract simple `$name = expression;` assignments from `@php ... @endphp` blocks.
pub fn extract_php_block_assignments(content: &str) -> Vec<(String, String)> {
    lazy_static! {
        // Match @php ... @endphp regions (multiline)
        static ref PHP_BLOCK_RE: Regex = Regex::new(r"(?s)@php\s*(.*?)@endphp").unwrap();
        // Match `$name = ...;` at start of statement (allowing leading whitespace).
        // The RHS is captured up to the first semicolon.
        static ref ASSIGN_RE: Regex = Regex::new(r"(?m)^\s*\$([a-zA-Z_]\w*)\s*=\s*([\s\S]*?);").unwrap();
    }

    let mut results = Vec::new();
    for block_caps in PHP_BLOCK_RE.captures_iter(content) {
        let body = match block_caps.get(1) {
            Some(m) => m.as_str(),
            None => continue,
        };
        for caps in ASSIGN_RE.captures_iter(body) {
            let name = match caps.get(1) {
                Some(m) => m.as_str().to_string(),
                None => continue,
            };
            let rhs = caps.get(2).map(|m| m.as_str().trim()).unwrap_or("");

            let php_type = if rhs == "$loop" {
                "Loop".to_string()
            } else {
                "mixed".to_string()
            };
            results.push((name, php_type));
        }
    }
    results
}
