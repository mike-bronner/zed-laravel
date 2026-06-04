# Contributing to Laravel for Zed

[← Back to README](README.md)

Contributions are welcome! This guide covers the project layout, local development setup, running tests, and code style.

## Project Structure

```
zed-laravel/
├── src/lib.rs           # Zed extension (binary download/management)
├── extension.toml       # Extension manifest
├── laravel-lsp/         # Laravel Language Server (the actual LSP)
│   ├── src/main.rs      # LSP server implementation
│   ├── src/queries.rs   # Tree-sitter pattern extraction
│   └── tests/           # Integration tests
└── test-project/        # Laravel fixture for testing (dev-only — never shipped)
```

> **Note:** `test-project/` and `laravel-lsp/tests/` are development-only and are
> **never bundled** with the published extension. Installing the extension delivers
> exactly two artifacts: the Zed package — `extension.toml` + the compiled
> `extension.wasm` (~70 KB total) — and the `laravel-lsp` binary, downloaded from
> [GitHub Releases](https://github.com/mike-bronner/zed-laravel/releases) at runtime.
> Zed's packager only includes the manifest, the compiled WASM, and declared assets
> (languages/grammars/themes/snippets), so arbitrary directories like the fixture are
> excluded by default. The fixture exists solely as a parsing target for the
> integration tests; its `composer.lock` / `package-lock.json` are intentionally
> untracked (see `test-project/.gitignore`) to keep its transitive dependencies out
> of Dependabot.

## Local Development

1. **Clone and build the LSP:**

   ```bash
   git clone https://github.com/mike-bronner/zed-laravel.git
   cd zed-laravel/laravel-lsp
   cargo build --release
   ```

2. **Configure Zed to use your local build:**

   Add to your Zed `settings.json`:

   ```json
   {
     "lsp": {
       "laravel-lsp": {
         "binary": {
           "path": "/path/to/zed-laravel/laravel-lsp/target/release/laravel-lsp"
         }
       }
     }
   }
   ```

3. **Install the extension for language support:**

   In Zed: `Cmd+Shift+P` → "zed: install dev extension" → select the `zed-laravel` directory.

4. **After making changes:**

   ```bash
   cd laravel-lsp && cargo build --release
   ```

   Then in Zed: `Cmd+Shift+P` → "zed: reload extensions"

## Running Tests

```bash
cd laravel-lsp

# Run all tests
cargo test

# Run with output
cargo test -- --nocapture

# Run specific test
cargo test test_view_resolution
```

## Code Style

```bash
# Format code
cargo fmt

# Run linter
cargo clippy
```
