use std::path::{Path, PathBuf};
use std::{env, fs};

/// This build script runs before compilation and downloads the tree-sitter-blade grammar
/// from GitHub, then compiles it using the C compiler.
///
/// Build scripts are special Rust programs that run at build time (not runtime).
/// They can access special environment variables set by Cargo.
fn main() {
    // Tell Cargo to re-run this build script if it changes
    println!("cargo:rerun-if-changed=build.rs");

    // Get the output directory where Cargo puts build artifacts
    // OUT_DIR is set by Cargo and points to target/debug/build/<package>/out
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    // Path where we'll download and extract the grammar
    let grammar_dir = out_dir.join("tree-sitter-blade");

    // Only download if we haven't already
    if !grammar_dir.exists() {
        println!("cargo:warning=Downloading tree-sitter-blade grammar from GitHub...");
        download_and_extract_blade_grammar(&grammar_dir);
    } else {
        println!("cargo:warning=Using cached tree-sitter-blade grammar");
    }

    // Compile the Blade grammar's C code
    compile_blade_grammar(&grammar_dir);
}

/// Downloads the tree-sitter-blade grammar from GitHub and extracts it
fn download_and_extract_blade_grammar(dest: &PathBuf) {
    // GitHub URL for the latest release tarball
    // Using the 'main' branch - in production, you'd pin a specific version/tag
    let url = "https://github.com/EmranMR/tree-sitter-blade/archive/refs/heads/main.tar.gz";

    println!("cargo:warning=Downloading from: {}", url);

    // Download the tarball using ureq (a simple HTTP client)
    // This is synchronous - it blocks until download completes
    let response = ureq::get(url)
        .call()
        .expect("Failed to download tree-sitter-blade grammar");

    let mut reader = response.into_body().into_reader();
    let mut bytes = Vec::new();
    std::io::copy(&mut reader, &mut bytes)
        .expect("Failed to read download response");

    // The downloaded file is a .tar.gz (gzipped tarball)
    // We need to:
    // 1. Decompress with gzip (flate2)
    // 2. Extract tar archive (tar crate)

    // Step 1: Decompress gzip
    use flate2::read::GzDecoder;
    let decompressed = GzDecoder::new(&bytes[..]);

    // Step 2: Extract tar archive
    let mut archive = tar::Archive::new(decompressed);

    // Create the destination directory
    fs::create_dir_all(dest.parent().unwrap())
        .expect("Failed to create grammar directory");

    // Extract to a temporary location (archive root is "tree-sitter-blade-main")
    let temp_dir = dest.parent().unwrap().join("tree-sitter-blade-temp");
    archive.unpack(&temp_dir)
        .expect("Failed to extract tar archive");

    // Move the extracted folder to our desired location
    // The archive extracts to "tree-sitter-blade-main/"
    let extracted = temp_dir.join("tree-sitter-blade-main");
    fs::rename(extracted, dest)
        .expect("Failed to move extracted grammar");

    // Clean up temp directory
    fs::remove_dir_all(temp_dir).ok();

    println!("cargo:warning=Successfully extracted tree-sitter-blade grammar");
}

/// Compiles the Blade grammar's C code using the cc crate
fn compile_blade_grammar(grammar_dir: &Path) {
    // Tree-sitter grammars are written in C and consist of:
    // - parser.c: The main parser logic (generated from grammar.js)
    // - scanner.c: Custom lexer for language-specific tokens (optional, hand-written)

    let src_dir = grammar_dir.join("src");

    println!("cargo:warning=Compiling Blade grammar C code from {:?}", src_dir);

    // The cc crate is a build-time dependency that wraps the C compiler
    // It automatically detects your system's C compiler (gcc, clang, msvc)
    cc::Build::new()
        // Include the tree-sitter header files
        .include(&src_dir)
        // Compile the main parser
        .file(src_dir.join("parser.c"))
        // Compile the custom scanner (if it exists)
        .file(src_dir.join("scanner.c"))
        // Output as a static library named "tree-sitter-blade"
        // This creates libtree-sitter-blade.a (Unix) or tree-sitter-blade.lib (Windows)
        .compile("tree-sitter-blade");

    println!("cargo:warning=Successfully compiled Blade grammar");
}
