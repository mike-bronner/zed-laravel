# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

This is a Zed editor extension that provides Laravel development support, similar to the Laravel VSCode extension. The extension is written in Rust and aims to provide features such as:

- Clickable "go-to-definition" for Blade templates
- Clickable "go-to-definition" for Livewire components
- Clickable "go-to-definition" for Flux components
- Other Laravel-specific IDE features

**Important**: This is a learning project. The developer is learning Rust while building this extension, so explanations of Rust concepts, providing options, and teaching best practices are essential.

## Development Commands

Zed extensions are typically developed using:

```bash
# Build the extension (assuming standard Rust project)
cargo build

# Run tests
cargo test

# Check code without building
cargo check

# Format code
cargo fmt

# Run linter
cargo clippy

# Build for release
cargo build --release
```

**IMPORTANT - Binary for Local Development:**
The `.zed/settings.json` configures Zed to use the local build directly:
```json
{
  "lsp": {
    "laravel-lsp": {
      "binary": {
        "path": "laravel-lsp/target/release/laravel-lsp"
      }
    }
  }
}
```

Development workflow:
```bash
cargo build --release
# Then in Zed: Cmd+Shift+P → "zed: reload extensions"
```

No copying or symlinks needed - Zed reads the binary path from settings.

## Running Diagnostics (Important for Zed)

When using Claude Code in Zed, it doesn't have direct access to LSP diagnostics. Always run these commands to check for errors:

### Check for Compilation Errors
```bash
cargo check
```
This is the fastest way to check if your code compiles without actually building the binary. Run this frequently while developing.

### See Detailed Compiler Messages
```bash
cargo build
```
This compiles the project and shows all errors and warnings with detailed explanations. The Rust compiler gives very helpful error messages - always read them carefully!

### Run Clippy for Best Practice Lints
```bash
cargo clippy
```
Clippy is Rust's linter that catches common mistakes and suggests more idiomatic code. Very useful when learning Rust!

### Run Tests
```bash
cargo test
```
Runs all tests in the project. Add `-- --nocapture` to see println! output during tests.

### Format Code
```bash
cargo fmt
```
Automatically formats your code according to Rust style guidelines. Run this before committing.

### Install the Extension Locally in Zed
```bash
# Install for local development/testing
zed: install dev extension
```
Use this command within Zed to load your extension for testing.

**Important**: After making changes, always run `cargo check` or `cargo build` to see if your code compiles before proceeding with more changes.

## Zed Extension Architecture

Zed extensions follow the Extension API provided by Zed. Key concepts:

- Extensions are written in Rust (or can use WebAssembly)
- Extensions interact with the Zed editor through the Extension API
- Language features like "go-to-definition" are typically implemented using the Language Server Protocol (LSP)
- Extensions can provide custom language servers or enhance existing ones

## Laravel-Specific Features to Implement

### Go-to-Definition Targets

1. **Blade Components**: `<x-component-name>` → `resources/views/components/component-name.blade.php`
2. **Livewire Components**: `<livewire:component-name>` → `app/Livewire/ComponentName.php`
3. **Flux Components**: `<flux:component>` → Flux component definition
4. **View References**: `view('view.name')` → `resources/views/view/name.blade.php`
5. **Route Names**: `route('route.name')` → route definition in `routes/` files
6. **Config References**: `config('app.name')` → `config/app.php`

## Architecture Notes

- Zed extensions MUST be written in Rust (compiled to WebAssembly)
- JavaScript/TypeScript cannot be used - VSCode extensions cannot be wrapped or ported
- Zed uses tree-sitter for syntax parsing
- May need custom tree-sitter queries for Laravel-specific patterns
- Extensions use the `zed_extension_api` crate and implement the `Extension` trait
- Language features use LSP (Language Server Protocol) integration

## LSP Architecture (laravel-lsp/)

### Core Components

| File | Purpose |
|------|---------|
| `main.rs` | LSP server, request handlers, Backend trait impl |
| `salsa_impl.rs` | Salsa incremental computation actor |
| `queries.rs` | Tree-sitter queries for pattern extraction |
| `parser.rs` | PHP and Blade tree-sitter parsing |
| `config.rs` | Laravel project configuration discovery |
| `env_parser.rs` | .env file parsing |
| `service_provider_analyzer.rs` | Middleware/binding extraction |
| `middleware_parser.rs` | Kernel.php and bootstrap/app.php parsing |

### Salsa Actor Pattern

The LSP uses a dedicated thread for Salsa incremental computation to avoid lifetime issues with async code:

```
┌─────────────────┐     oneshot channel     ┌─────────────────┐
│  LSP Handlers   │ ──────────────────────► │   SalsaActor    │
│  (async/await)  │ ◄────────────────────── │ (dedicated      │
│                 │        response         │  thread)        │
└─────────────────┘                         └─────────────────┘
```

**Key pattern for adding new Salsa features:**
1. Add `#[salsa::input]` type in `salsa_impl.rs`
2. Add data transfer type (no lifetimes) for async boundary
3. Add `SalsaRequest` variant with oneshot sender
4. Add `SalsaHandle` method (async interface)
5. Add handler method in `SalsaActor`
6. Add helper method in `main.rs` to register data

### Salsa Components

| Component | Input Type | Data Transfer Type | Purpose |
|-----------|------------|-------------------|---------|
| File Patterns | `SourceFile` | `ParsedPatternsData` | Cached parsed patterns per file |
| Config | `ConfigFile` | `LaravelConfigData` | Project configuration |
| Project Files | `ProjectFiles` | `ViewReferenceLocationData` | Reference finding across project |
| Service Providers | `ServiceProviderFile` | `MiddlewareRegistrationData`, `BindingRegistrationData` | Middleware/binding lookups |
| Env Variables | `EnvFile` | `EnvVariableData` | Environment variable lookups |

### Important Conventions

- **Data transfer types**: Use `*Data` suffix (e.g., `EnvVariableData`) for types crossing async boundaries
- **Salsa inputs**: Use `#[salsa::input]` for source data, store in `HashMap` for O(1) lookup
- **Registration pattern**: Call `register_*_with_salsa()` after successful parsing
- **Fallback pattern**: Use Salsa cache first, fall back to direct computation if unavailable
- **Priority merging**: Framework=0, Package=1, App=2 (higher wins)

### Position Indexing Convention

All positions are **0-based** throughout the stack:
- Tree-sitter `Point`: row/column are 0-based
- LSP `Position`: line/character are 0-based
- All match structs: row/column/end_column are 0-based

**Key fields in match structs:**

| Field | Points to |
|-------|-----------|
| `column` | Start of entire pattern (e.g., `@` in `@include`) |
| `end_column` | End of entire pattern (e.g., after `)` in `@include('x')`) |
| `string_column` | Start of **content** inside quotes (first char after quote) |
| `string_end_column` | End of content (position one past last char, before closing quote) |

**Rule**: Never manually calculate string positions in `main.rs`. Use `string_column`/`string_end_column` from Salsa.

### Cache Invalidation Architecture (CRITICAL)

**All file-derived features MUST use Salsa incremental computation:**

```
did_change(file) → Debounce 250ms → Update Salsa input → Queries recompute → UI updates
```

**Rules:**
1. **Never bypass Salsa** - All file parsing goes through Salsa inputs
2. **Update on edit, not just save** - Wire `did_change` to Salsa (debounced)
3. **Salsa handles invalidation** - Don't manually track what needs recomputing
4. **Pure query functions** - Queries derive from inputs, no side effects

**Pattern Types (all extracted via Salsa queries):**

| Pattern | Example | Extracted From | Target |
|---------|---------|----------------|--------|
| Views | `view('welcome')` | SourceFile | `resources/views/*.blade.php` |
| Blade Components | `<x-button>` | SourceFile | `resources/views/components/*.blade.php` |
| Blade Directives | `@include('partial')` | SourceFile | `resources/views/*.blade.php` |
| Livewire | `<livewire:counter>` | SourceFile | `app/Livewire/*.php` |
| Translations | `__('messages.key')` | SourceFile | `lang/*/*.php` |
| Assets | `asset('css/app.css')` | SourceFile | `public/*` |
| Vite | `@vite('resources/js/app.js')` | SourceFile | `resources/*` |
| Routes | `route('home')` | SourceFile | Route name in `routes/*.php` |
| Config | `config('app.name')` | SourceFile | `config/*.php` |
| Env | `env('APP_NAME')` | SourceFile | `.env` |
| Middleware | `->middleware('auth')` | SourceFile | Alias in registry |
| Bindings | `app('cache')` | SourceFile | Binding in registry |

**File Type → Salsa Input Mapping:**

| File Pattern | Salsa Input | What It Provides |
|--------------|-------------|------------------|
| `*.php`, `*.blade.php` | `SourceFile` | Pattern extraction (views, components, etc.) |
| `bootstrap/app.php`, `Providers/*.php` | `ServiceProviderFile` | Middleware aliases, container bindings |
| `.env`, `.env.*` | `EnvFile` | Environment variable values |
| `config/*.php`, `composer.json` | `ConfigFile` | View paths, namespaces, PSR-4 mappings |

**Target Files (existence only):**
- View files, component files, Livewire classes, translation files, assets
- Tracked via file existence cache with 5-minute TTL
- No Salsa input needed - just check if file exists

**Adding New Features:**
1. Define `#[salsa::input]` for source data
2. Define `#[salsa::tracked]` query function (pure, no side effects)
3. Ensure `did_change` updates the input (automatic via file type detection)
4. Query results are automatically cached and incrementally updated

### Request Flow Example

```
User hovers over view('users.index')
    │
    ▼
Backend::hover() in main.rs
    │
    ▼
salsa.get_parsed_patterns(file_path, content)
    │
    ▼
SalsaActor checks cache, returns ParsedPatternsData
    │
    ▼
Find matching pattern at cursor position
    │
    ▼
Resolve view name to file path using config
    │
    ▼
Return HoverContents with file location
```

## Implementation Plan

This project follows a phased approach designed for learning Rust while building:

### Phase 1: Rust & Zed Extension Basics
**Goal**: Create a minimal working Zed extension

**Learning Focus**:
- Rust project structure (`Cargo.toml`, `src/lib.rs`)
- Basic Rust syntax (structs, traits, macros)
- The `zed_extension_api` crate
- What `impl` means and how traits work
- The `register_extension!` macro
- Rust's ownership model basics

**Deliverable**: Extension that loads in Zed and prints "Hello from Laravel Extension"

### Phase 2: File System Navigation
**Goal**: Given a view name, find the corresponding `.blade.php` file

**Learning Focus**:
- Rust's `String` vs `&str` types
- Working with file paths (`std::path::Path`)
- Result and Option types (error handling)
- Basic pattern matching with `match`
- The `?` operator for error propagation
- Why Rust doesn't have `null`

**Deliverable**: Function that converts `view('users.profile')` → `resources/views/users/profile.blade.php`

### Phase 3: Pattern Matching
**Goal**: Detect Laravel patterns in code using regex

**Learning Focus**:
- Regular expressions in Rust (`regex` crate)
- Iterators and closures
- Borrowing and references (`&` and `&mut`)
- Collections (`Vec`, `HashMap`)
- Iterator methods (`.map()`, `.filter()`, `.collect()`)

**Deliverable**: Function that finds all `view('...')` calls in a file

### Phase 4: Tree-sitter Integration
**Goal**: Parse Blade and PHP files properly using tree-sitter

**Learning Focus**:
- Working with tree-sitter's Rust API
- Tree traversal algorithms
- Lifetimes (what they are and why they matter)
- Memory management and performance
- Rust's zero-cost abstractions

**Deliverable**: Parse `<x-button>` tags from Blade files

### Phase 5: Go-to-Definition
**Goal**: Implement clickable "go-to-definition" for Blade components

**Learning Focus**:
- Zed's LSP integration APIs
- Async Rust (`async`/`await`, `Future` trait)
- More advanced trait usage
- Position/range calculations
- How async works in Rust vs JavaScript

**Deliverable**: Click `<x-button>` and jump to `components/button.blade.php`

### Phase 6: Advanced Features
**Goal**: Extend to Livewire, Flux, routes, config

**Learning Focus**:
- Code organization (modules, workspace structure)
- Advanced error handling
- Testing in Rust (`#[cfg(test)]`)
- Documentation (`///` comments)
- Publishing extensions

**Deliverable**: Full-featured Laravel extension with multiple go-to features

## Teaching Approach

When working on this project:
1. **Explain concepts first** - Explain Rust concepts before implementing them
2. **Provide options** - Present multiple implementation approaches with trade-offs
3. **Write code together** - Explain each line as it's written
4. **Encourage questions** - Answer "why" questions about design decisions
5. **Iterative development** - Build working code first, then refactor to be "more Rusty"
6. **Help with compiler errors** - Rust's compiler is helpful; explain what errors mean

## Resources

- Zed Extension API documentation: https://zed.dev/docs/extensions
- Existing Zed extensions for reference: https://github.com/zed-industries/extensions
- Laravel VSCode extension (for feature reference): https://github.com/amiralizadeh9480/laravel-extra-intellisense

---

## Session State (2026-02-08)

### Last Session Summary

**Focus**: Investigating whether to remove the bundled Blade language definition and rely on the separate Blade Zed extension, per Zed reviewer feedback. Explored Option C (LSP semantic tokens) in depth and found it's not viable yet.

### Context: Zed Reviewer Feedback

The Zed extension reviewer asked why this extension bundles its own Blade language definition when the existing Blade extension (`bajrangCoder/zed-laravel-blade`) already provides one. The reviewer suggested users could disable the Blade extension's language servers and use `laravel-lsp` instead.

### Branch: `experiment/remove-blade-language`

**What was done:**
- Deleted `languages/blade/` (config.toml, highlights.scm, brackets.scm, indents.scm, injections.scm)
- Deleted `languages/php_only/` (config.toml, highlights.scm)
- Removed `[grammars.blade]` and `[grammars.php_only]` from `extension.toml`
- Removed empty `languages/` directory
- Updated README: replaced "Blade Language Support" section, updated project structure, added note about separate Blade extension
- Changed LSP semantic token type from `KEYWORD` to `FUNCTION` in `main.rs` (so directive highlighting matches our `@function` tree-sitter capture)
- Build passes, all 190 tests pass

**What was NOT changed:**
- `extension.toml` still lists `languages = ["PHP", "Blade", "XML", "Shell Script"]` for the LSP (correct — this just activates the LSP for those file types, doesn't own the language)
- All LSP code (build.rs, parser.rs, queries.rs, etc.) still uses tree-sitter-blade internally for parsing

### Key Findings

1. **Zed does NOT support augmenting a language from another extension** — you either own the full language definition or you don't. No partial overrides. Community has requested this but it's not implemented.

2. **Providing `languages/blade/` with .scm files but no `config.toml` errors** — Zed requires `config.toml` when it finds the directory.

3. **Providing `config.toml` without grammar declarations works** — Zed resolves the `blade` grammar from the other Blade extension. BUT this takes full ownership of the language definition, replacing (not augmenting) the other extension's .scm files.

4. **A file can only belong to ONE language in Zed** — creating a separate language name (e.g., "laravel-blade") doesn't work because the Blade extension's language servers are registered for "Blade", not the custom name.

5. **The Blade extension (`bajrangCoder/zed-laravel-blade`) provides:**
   - Language definition: config.toml, highlights.scm, brackets.scm, indents.scm, injections.scm, outline.scm, overrides.scm
   - Grammars: tree-sitter-blade, tree-sitter-php (php_only)
   - Language servers: emmet, intelephense, phptools, phpactor (all PHP LSPs wired to Blade files)
   - No custom Blade LSP of its own
   - Cloned to `/Users/mike/Developer/zed-laravel-blade` and installed as dev extension for testing

6. **Semantic tokens (Option C) NOT viable yet:**
   - Zed PR #46356 "editor: Implement semantic highlighting" merged to `main` on Feb 4, 2026
   - But it has NOT been released in any build — not in stable (0.222.4) nor preview (0.223.2)
   - The `semantic_tokens` setting, `lsp: restart language servers` command, and `dev: open highlights tree view` command do not exist in current Zed builds
   - The documentation at https://zed.dev/docs/semantic-tokens is published ahead of the actual release
   - Once released, users would need `"semantic_tokens": "combined"` in their Blade language settings
   - The LSP already sends `FUNCTION` tokens for directives (changed from `KEYWORD` this session)

### Detailed Feature Comparison (Our Extension vs Blade Extension)

**Losses from removing our language files:**

| Category | Impact | Details |
|---|---|---|
| **Directive highlighting** | **Significant** | Our `@function` (blue) vs their `@tag` (cyan) — directives become visually indistinguishable from HTML tags |
| **Blade bracket pairs** | **Moderate** | `{{ }}`, `{!! !!}`, `{{-- --}}` bracket matching/pairing lost — they don't define these in config.toml |
| **`{` auto-close** | **Minor** | We set `close = false` (LSP handles snippets), they set `close = true` |
| **Hyphenated word selection** | **Minor** | We define `word_characters = ["-"]` on root + `element` + `string` overrides; they only define it on `string` |

**No loss:**
- Indentation (`indents.scm`) — identical files
- Injections (`injections.scm`) — identical files
- Bracket queries (`brackets.scm`) — identical files
- `php_only` language — identical highlights and config (whitespace-only diff)

**Features gained from Blade extension:**
- `outline.scm` — comments in symbol outline panel
- `overrides.scm` — TailwindCSS completions in attribute values
- `wrap_characters` — HTML tag wrapping support

### Network Issue (Resolved)

- **Proxyman Guard** network extension was still active on macOS despite the app being uninstalled, causing connection timeouts to `api.zed.dev` and `zed-extensions.nyc3.digitaloceanspaces.com`
- Disabled via System Settings > General > Login Items & Extensions > Network Extensions
- `api.zed.dev` works after disabling Proxyman; DigitalOcean Spaces still has ISP-level routing issues (Lumen NYC edge router drops packets to DO's NYC3 region)
- Workaround: `/etc/hosts` entry for `api.zed.dev` pointing to working Cloudflare IP `172.66.165.132` (can be removed when ISP routing resolves)
- Blade extension installed as dev extension from `/Users/mike/Developer/zed-laravel-blade` to bypass download

### Remaining Options

1. **Option A: Contribute upstream** — PR our highlighting improvements (`@function` for directives, bracket pair definitions, `word_characters` config) to `bajrangCoder/zed-laravel-blade`. Cleanest long-term solution.
2. **Option B: Keep owning the language definition without grammars** — Provide `languages/blade/` with our `.scm` files + `config.toml` but no `[grammars.*]` in `extension.toml`. Grammar resolves from Blade extension. Addresses reviewer's duplicate grammar concern but still duplicates language definition.
3. **Option C: Wait for semantic tokens** — Once Zed ships the feature (PR #46356), the LSP's `FUNCTION` tokens will overlay directive highlighting. Doesn't solve bracket pairs or config differences. Can be combined with A or B later.

### Current Status

- Branch: `experiment/remove-blade-language`
- Build: **Passing**
- Tests: **190 tests passing**
- Uncommitted changes: language files deleted, README updated, extension.toml grammar refs removed, LSP semantic token type changed to FUNCTION
- Blade extension cloned to `/Users/mike/Developer/zed-laravel-blade` and installed as dev extension
- Zed settings reverted (removed `semantic_tokens: "combined"` that was added for testing)
- Decision pending on which option (A, B, or C) to pursue
