<p align="center">
  <img src="docs/logo.svg" width="128" height="128" alt="Laravel for Zed">
</p>

<h1 align="center">Laravel for Zed</h1>

<p align="center">
<strong>Cmd+Click your way through Laravel projects</strong>
</p>

<p align="center">
<a href="https://github.com/GeneaLabs/zed-laravel/actions/workflows/release.yml"><img src="https://github.com/GeneaLabs/zed-laravel/actions/workflows/release.yml/badge.svg" alt="Build Status"></a>
<a href="https://github.com/GeneaLabs/zed-laravel/releases"><img src="https://img.shields.io/github/v/release/GeneaLabs/zed-laravel?label=version" alt="Latest Release"></a>
<img src="https://img.shields.io/github/downloads/GeneaLabs/zed-laravel/total" alt="Downloads">
<img src="https://img.shields.io/github/stars/GeneaLabs/zed-laravel?style=flat" alt="GitHub Stars">
</p>

<p align="center">
<img src="https://img.shields.io/badge/Laravel-FF2D20?logo=laravel&logoColor=white" alt="Laravel">
<img src="https://img.shields.io/badge/Zed-Extension-8B5CF6" alt="Zed Extension">
<img src="https://img.shields.io/badge/Rust-000000?logo=rust&logoColor=white" alt="Rust">
<a href="https://github.com/GeneaLabs/zed-laravel/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT License"></a>
</p>

<p align="center">
<sub>A community extension — not affiliated with Laravel LLC</sub>
</p>

---

## 💛 Why we built this

We love Laravel, and we love Zed. When we moved our Laravel work into Zed, the deep, framework-aware tooling we'd relied on elsewhere wasn't there yet — so we built it. This extension exists to give Laravel first-class support in Zed, because a framework this good deserves great tooling everywhere its developers work.

The intelligence lives in a standalone language server (LSP) — the same protocol your editor already speaks for other languages. Today it targets Zed; because it's LSP-based, the same engine could reach other LSP-capable editors (Neovim, Helix, Sublime Text, and more) down the road. That's a direction we'd love to grow toward, not something we ship yet.

### How it works — static analysis

Everything is parsed statically with tree-sitter: the extension reads your files, it never runs them. It only touches your database when *you* opt into schema-backed completion, and it keeps working even when your app won't boot — a half-applied migration, a missing `.env`, or a dirty branch won't stop it. The honest trade-off: some deeply dynamic runtime behaviour (fully dynamic Eloquent magic, runtime-registered routes) is harder to reach through static analysis alone.

### Laravel across editors

Laravel developers are spoiled for choice — every major editor has a strong way to work with the framework. Here's roughly where things stand and what each needs, so you can pick whatever fits how you work:

| Editor | Laravel-aware tooling | Cost |
|---|---|---|
| **PHPStorm** | Laravel support built in, powered by the [Laravel Idea](https://laravel-idea.com/) plugin | Paid IDE (free for non-commercial use) |
| **VS Code** | [Official Laravel extension](https://github.com/laravel/vs-code-extension), maintained by the Laravel team | Free |
| **Zed** | This extension, plus [Laravel Blade](https://github.com/bajrangCoder/zed-laravel-blade) for syntax highlighting | Free |

<sub>A high-level snapshot as of 2026-05-30 — not a feature-by-feature scorecard. Every option here is capable and actively developed. (As of 2025, the Laravel Idea plugin is bundled free with PhpStorm.) Corrections welcome via PR.</sub>

## 📦 Install

Search **"Laravel"** in Zed Extensions and click Install.

**🤝 Recommended companion:** Also install the [**Laravel Blade**](https://github.com/bajrangCoder/zed-laravel-blade) extension (`bajrangCoder/zed-laravel-blade`) for Blade syntax highlighting, bracket matching, and PHP language server integration. The two extensions complement each other — Blade handles the language definition and grammar, while this extension handles Laravel-specific intelligence (go-to-definition, autocomplete, diagnostics).

**From source:** Clone the repo, run `cargo build --release` in `laravel-lsp/`, then use "zed: install dev extension".

## ⚙️ Configuration

The extension works out of the box with zero configuration. It automatically discovers your Laravel project structure, including view paths, component namespaces, route files, and service providers.

**Optional settings** can be added to your Zed `settings.json`:

```json
{
  "lsp": {
    "laravel-lsp": {
      "settings": {
        "autoCompleteDebounce": 200,
        "blade": {
          "directiveSpacing": false
        },
        "diagnostics": {
          "severity": "warning"
        }
      }
    }
  }
}
```

| Setting | Default | Description |
|---------|---------|-------------|
| `autoCompleteDebounce` | `200` | Delay (ms) before autocomplete updates after typing. Lower values (50-100ms) give faster feedback. Higher values (300-500ms) reduce CPU usage. |
| `blade.directiveSpacing` | `false` | Add space between directive name and parentheses. `false`: `@if($condition)` / `true`: `@if ($condition)` |
| `diagnostics.severity` | `"warning"` | Severity for query-chain diagnostics (unknown column/relation/table in Eloquent & `DB::table()` chains). One of `"warning"`, `"error"`, `"info"`, or `"off"` to disable. Requires a working database connection — diagnostics stay silent when the schema can't be introspected. |

**🗄️ Database autocomplete** (`exists:`, `unique:` rules, Eloquent properties) requires a working database connection. Configure in your `.env`:

```env
DB_CONNECTION=mysql
DB_HOST=127.0.0.1
DB_DATABASE=myapp
DB_USERNAME=root
DB_PASSWORD=secret
```

Supports MySQL, PostgreSQL, SQLite, and SQL Server.

**⚡ Indexing performance.** The extension indexes every PHP and Blade file in your project (including `vendor/`) at startup so find-references and goto-definition return instantly. A persistent on-disk cache makes subsequent project opens near-instant — only files whose `mtime` has changed since they were last indexed get re-parsed. External changes (a `git pull`, a `composer install`, a formatter running outside Zed) are picked up live via `workspace/didChangeWatchedFiles`. The status bar shows progress during the initial warmup.

**🎨 Enhanced Blade directive highlighting** uses LSP semantic tokens to give directives like `@if`, `@foreach`, and `@section` distinct function-style coloring. This is also the only way to get correct highlighting for **custom directives** (e.g., `@myCustomDirective`, Livewire's `@teleport`, Pennant's `@feature`) that tree-sitter doesn't know about. Enable it in your Zed `settings.json`:

```json
{
  "languages": {
    "Blade": {
      "semantic_tokens": "combined"
    }
  }
}
```

**🗺️ Laravel-aware outline panel** populates Zed's outline panel and breadcrumbs with Laravel-specific structure that no PHP language server understands:

- **Route files** — every `Route::get/post/...` call labelled `METHOD URI [name=...]`, with nested `Route::group(...)` calls becoming hierarchical containers labelled `group [prefix=..., name=...]`. Prefix and name chains propagate to children. Covers all Route methods including `resource`, `apiResource`, `singleton`, `livewire`, `view`, `redirect`, `fallback`, etc.
- **Blade templates** — `@extends`, `@section`, `@push`, `@yield`, `@stack`, `@include*`, `@props`, plus the modern tag syntax: `<x-component>`, `<livewire:counter>`, `<flux:icon>`, `<x-slot:name>`. Paired tags nest their children; self-closing tags appear as leaves.

PHP class outlines (controllers, models, Livewire components, jobs, services) come from whatever PHP language server you have installed — those servers have real semantic understanding of PHP that a tree-sitter walker can't match. The official [**PHP**](https://github.com/zed-extensions/php) Zed extension registers Intelephense, Phpactor, and PhpTools; install it and pick whichever LSP you prefer.

**Requirements**

| Outline | Requires |
|---|---|
| Route files | This extension, plus `document_symbols: on` for `PHP` (route files use the `PHP` language). |
| Blade templates | This extension, the [Laravel Blade](https://github.com/bajrangCoder/zed-laravel-blade) extension (for the `Blade` language definition), plus `document_symbols: on` for `Blade`. |
| PHP class files | A PHP language server (the [PHP](https://github.com/zed-extensions/php) extension provides Intelephense / Phpactor / PhpTools), plus `document_symbols: on` for `PHP`. |

**Configuration**

Zed defaults to tree-sitter outlines, which don't call any LSP — opt into LSP outlines per-language ([`zed#48780`](https://github.com/zed-industries/zed/pull/48780)):

```json
{
  "languages": {
    "PHP": {
      "document_symbols": "on"
    },
    "Blade": {
      "document_symbols": "on"
    }
  }
}
```

`document_symbols: on` for `PHP` unlocks both our route outline (for files under `routes/`) and your PHP LSP's class outline (for everything else). `document_symbols: on` for `Blade` unlocks our Blade outline. (This opt-in is a Zed quirk — LSP clients that request `textDocument/documentSymbol` unconditionally, like Helix or Neovim, wouldn't need it.)

> **Quirks worth knowing** — Zed colors outline labels by word-matching them against the source buffer's tree-sitter highlights, which produces slightly inconsistent colors on multi-segment URLs (e.g., `/cra-details` may color `cra` and `details` differently if they match different tokens elsewhere in the file). Route names appear in the LSP `detail` field, which Zed's outline panel doesn't currently render (VSCode and Sublime/LSP do). Both are tracked upstream: [zed#57576](https://github.com/zed-industries/zed/issues/57576).

### 🔧 Tuning Intelephense

If you use Intelephense as your PHP language server, goto-definition on a class name can surface noisy stub-template results from `vendor/*/stubs/`. The quick fix is to exclude them — those scaffold templates are never loaded at runtime:

```json
{
  "lsp": {
    "intelephense": {
      "settings": {
        "files": {
          "exclude": ["**/stubs/**"]
        }
      }
    }
  }
}
```

After saving, restart Intelephense (`Cmd+Shift+P → lsp: restart`). For the licence-key shape, the `_ide_helper*.php` / `.phpstorm.meta.php` trade-offs, the cache caveat, and per-project `.intelephense.json`, see the **[full Intelephense tuning guide](docs/tuning-intelephense.md)**.

## ✨ Features

Each feature has a focused reference under [`docs/`](docs/) — click through to dive in.

| Feature | What it does |
|---|---|
| [🔗 Go-to-Definition](docs/go-to-definition.md) | Jump to views, components, routes, config, translations, env, assets, middleware, bindings — plus query-chain columns / relations / tables |
| [ℹ️ Hover](docs/hover.md) | Intelephense-style summary cards for every recognised pattern |
| [🔍 Find References](docs/find-references.md) | Every call site across the project, vendor packages included |
| [✏️ Rename](docs/rename.md) | Atomic rename of routes, configs, translations, env vars, views, components, Livewire, middleware, bindings, and model classes |
| [💡 Autocomplete](docs/autocomplete.md) | Cast types, model properties, query chains, builder methods, Blade / loop / slot variables, Pennant flags |
| [❌ Diagnostics](docs/diagnostics.md) | Missing views / components / features, invalid rules, query-chain typos against your real schema |
| [⚡ Quick Actions](docs/quick-actions.md) | One-click create missing views, components, middleware, features, and migrations |
| [🎨 Blade Editing](docs/blade-editing.md) | Directive autocomplete, smart bracket expansion, closing-tag navigation |
| [🗺️ Outline Panel](docs/outline.md) | Laravel-aware route + Blade structure in Zed's outline and breadcrumbs |

## 🚧 Planned Features

**Rename — remaining work** (the class-backed kinds and the Eloquent model-class engine shipped; variables didn't):

- ✏️ **More PHP class kinds** — the FQCN rename engine (use-statement updates, type-hint / static-call / `new` / `::class` rewrites, docblocks, file move) now powers Eloquent model rename; extending it to controllers, jobs, services, and form requests as first-class symbols is the follow-up.
- 📝 **Blade variable rename** — scope-aware within a template (`@foreach`, `@php`, etc.), plus cross-file via the `view('x', ['key' => …])` / `compact('key')` linkage from controller into view.
- 🔧 **PHP variable rename** — scope-aware function-local. Class properties (`$this->foo`) are out of scope for this round and folded into a future class-property rename.

**Framework integrations:**

- 🎨 **Inertia.js support** — go-to-definition and autocomplete for `Inertia::render('Page')` calls
- 📁 **Folio page routing** — surface Folio's filesystem-routed pages in goto-definition / completion / find-references

## 🤝 Contributing

Contributions are welcome! See **[CONTRIBUTING.md](CONTRIBUTING.md)** for the project layout, local development setup, building the LSP, running tests, and code style.

---

<p align="center">
<a href="https://github.com/GeneaLabs/zed-laravel/blob/main/LICENSE">MIT</a> · <a href="https://github.com/GeneaLabs">GeneaLabs</a>
</p>
