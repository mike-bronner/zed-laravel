//! This module provides tree-sitter parsers for PHP and Blade files.
//!
//! Tree-sitter parsers convert source code into Abstract Syntax Trees (ASTs)
//! that we can query to find patterns like view() calls and <x-component> tags.

use tree_sitter::{Language, Parser};

// ============================================================================
// PART 1: Language Definitions (FFI Bindings)
// ============================================================================

/// Gets the tree-sitter language definition for PHP
///
/// This comes from the tree-sitter-php crate, which provides pre-built
/// Rust bindings. We can just call this function directly.
pub fn language_php() -> Language {
    // The tree-sitter-php crate provides LANGUAGE_PHP (not LANGUAGE)
    // .into() converts the LanguageFn into a Language struct
    tree_sitter_php::LANGUAGE_PHP.into()
}

/// Gets the tree-sitter language definition for Blade
///
/// This is more complex because we compiled the grammar ourselves.
/// We need to use FFI (Foreign Function Interface) to call the C function.
pub fn language_blade() -> Language {
    // LEARNING MOMENT: extern "C" and unsafe
    //
    // When you compile a tree-sitter grammar, it exports a C function with this signature:
    //   const TSLanguage *tree_sitter_blade(void);
    //
    // To call it from Rust, we need to:
    // 1. Declare it in an `extern "C"` block (tells Rust about the C function)
    // 2. Call it inside `unsafe` (because Rust can't verify C code's safety)

    extern "C" {
        // This declares that a C function exists somewhere
        // The linker will find it in the libtree-sitter-blade.a we compiled
        //
        // The function name MUST match what the grammar exports
        // Tree-sitter grammars export: tree_sitter_<grammar_name>()
        fn tree_sitter_blade() -> *const tree_sitter::ffi::TSLanguage;
    }

    // LEARNING MOMENT: unsafe blocks
    //
    // Rust requires `unsafe` when:
    // - Calling foreign (C/C++) functions
    // - Dereferencing raw pointers
    // - Accessing mutable static variables
    //
    // This doesn't mean the code IS unsafe, just that Rust can't verify it.
    // It's your responsibility to ensure it's used correctly.
    unsafe {
        // Call the C function to get the language definition
        let ptr = tree_sitter_blade();

        // Convert the C pointer to a Rust Language struct
        // Language::from_raw() doesn't return a Result - it returns Language directly
        // The function signature is: unsafe fn from_raw(ptr: *const TSLanguage) -> Language
        //
        // This is safe because:
        // 1. We just compiled the grammar in build.rs
        // 2. The pointer comes directly from the tree-sitter-generated C code
        // 3. The grammar exports the correct function signature
        Language::from_raw(ptr)
    }
}

// ============================================================================
// PART 2: Parser Creation
// ============================================================================

/// Creates a new tree-sitter parser configured for PHP
///
/// A Parser is stateful and not thread-safe, so you typically create
/// one per file you're parsing or use a pool of parsers.
pub fn create_php_parser() -> anyhow::Result<Parser> {
    let mut parser = Parser::new();

    // Set the language for this parser
    // After this, calling parser.parse() will use PHP grammar rules
    parser
        .set_language(&language_php())
        .map_err(|e| anyhow::anyhow!("Failed to set PHP language: {:?}", e))?;

    Ok(parser)
}

/// Creates a new tree-sitter parser configured for Blade
pub fn create_blade_parser() -> anyhow::Result<Parser> {
    let mut parser = Parser::new();

    parser
        .set_language(&language_blade())
        .map_err(|e| anyhow::anyhow!("Failed to set Blade language: {:?}", e))?;

    Ok(parser)
}

// ============================================================================
// PART 3: Helper Functions for Parsing
// ============================================================================

/// Parse PHP source code into a syntax tree
///
/// Returns None if parsing fails (syntax errors, etc.)
pub fn parse_php(source: &str) -> anyhow::Result<tree_sitter::Tree> {
    let mut parser = create_php_parser()?;

    // parse() returns Option<Tree>
    // - Some(tree) if parsing succeeded (even with syntax errors in source)
    // - None if parser couldn't allocate memory or other critical failure
    parser
        .parse(source, None)
        .ok_or_else(|| anyhow::anyhow!("Failed to parse PHP source"))
}

/// Parse Blade source code into a syntax tree
pub fn parse_blade(source: &str) -> anyhow::Result<tree_sitter::Tree> {
    let mut parser = create_blade_parser()?;

    parser
        .parse(source, None)
        .ok_or_else(|| anyhow::anyhow!("Failed to parse Blade source"))
}

// ============================================================================
// PART 4: Tests
// ============================================================================

#[cfg(test)]
mod tests;
