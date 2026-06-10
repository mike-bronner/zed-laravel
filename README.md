<p align="center">
  <img src="docs/logo.svg" width="128" height="128" alt="Laravel for Zed">
</p>

<h1 align="center">Laravel for Zed</h1>

<p align="center">
<strong>Cmd+Click your way through Laravel projects</strong>
</p>

<p align="center">
<a href="https://github.com/mike-bronner/zed-laravel/actions/workflows/release.yml"><img src="https://github.com/mike-bronner/zed-laravel/actions/workflows/release.yml/badge.svg?event=release" alt="Release"></a>
<a href="https://github.com/mike-bronner/zed-laravel/releases"><img src="https://img.shields.io/github/v/release/mike-bronner/zed-laravel?label=version" alt="Latest Release"></a>
<img src="https://img.shields.io/github/downloads/mike-bronner/zed-laravel/total" alt="Downloads">
<img src="https://img.shields.io/github/stars/mike-bronner/zed-laravel?style=flat" alt="GitHub Stars">
</p>

<p align="center">
<img src="https://img.shields.io/badge/Laravel-FF2D20?logo=laravel&logoColor=white" alt="Laravel">
<img src="https://img.shields.io/badge/Zed-Extension-8B5CF6" alt="Zed Extension">
<img src="https://img.shields.io/badge/Rust-000000?logo=rust&logoColor=white" alt="Rust">
<a href="https://github.com/mike-bronner/zed-laravel/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT License"></a>
</p>

<p align="center">
<sub>A community extension — not affiliated with Laravel LLC</sub>
</p>

## ❤️ Why we built this

We love Laravel, and we love Zed. When we moved our Laravel work into Zed, the deep, framework-aware tooling we'd relied on elsewhere wasn't there yet — so we built it. This extension exists to give Laravel first-class support in Zed, because a framework this good deserves great tooling everywhere its developers work.

The intelligence lives in a standalone language server (LSP) — the same protocol your editor already speaks for other languages. Today it targets Zed; because it's LSP-based, the same engine could reach other LSP-capable editors (Neovim, Helix, Sublime Text, and more) down the road. That's a direction we'd love to grow toward, not something we ship yet.

### How it works — static analysis

Everything is parsed statically with tree-sitter: the extension reads your files, it never runs them. It only touches your database when *you* opt into schema-backed completion, and it keeps working even when your app won't boot — a half-applied migration, a missing `.env`, or a dirty branch won't stop it. Declared Eloquent magic is resolved statically through a project-wide semantic index — scopes, accessors, relationships, columns, and dynamic finders, in both property and call form, including builder chains. The honest trade-off: truly runtime-only behaviour (dynamic member *names* like `$model->$attribute`, runtime-registered routes) stays out of reach, and ambiguous sites are dropped rather than guessed.

**⚡ Indexing performance.** The extension indexes every PHP and Blade file in your project (including `vendor/`) at startup so find-references and goto-definition return instantly. A persistent on-disk cache makes subsequent project opens near-instant — only files whose `mtime` has changed since they were last indexed get re-parsed. External changes (a `git pull`, a `composer install`, a formatter running outside Zed) are picked up live via `workspace/didChangeWatchedFiles`. The status bar shows progress during the initial warmup.


### Laravel across editors

Laravel developers are spoiled for choice — every major editor has a strong way to work with the framework. Here's roughly where things stand and what each needs, so you can pick whatever fits how you work:

| Editor | Laravel-aware tooling | Cost |
|---|---|---|
| **PHPStorm** | Laravel support built in, powered by the [Laravel Idea](https://laravel-idea.com/) plugin | Paid IDE (free for non-commercial use) |
| **VS Code** | [Official Laravel extension](https://github.com/laravel/vs-code-extension), maintained by the Laravel team | Free |
| **Zed** | This extension, in addition to companion extensions:  [Laravel Blade](https://github.com/bajrangCoder/zed-laravel-blade), [PHP](https://github.com/zed-extensions/php) (Intelephense), [phpcs](https://github.com/mike-bronner/zed-phpcs-lsp), and [phpmd](https://github.com/mike-bronner/zed-phpmd-lsp) | Free |

<sub>A high-level snapshot as of 2026-05-30 — not a feature-by-feature scorecard. Every option here is capable and actively developed. (As of 2025, the Laravel Idea plugin is bundled free with PhpStorm.) Corrections welcome via PR.</sub>

## ✨ Features

Each feature has a focused reference under [`docs/`](docs/) — click through to dive in.

| Feature | What it does |
|---|---|
| [🔗 Go-to-Definition](docs/go-to-definition.md) | Jump to views, components, routes, config, translations, env, assets, middleware, bindings, Artisan commands — plus query-chain columns / relations / tables and Eloquent magic members |
| [ℹ️ Hover](docs/hover.md) | Intelephense-style summary cards for every recognised pattern, including semantic cards for Eloquent magic (scopes, accessors, relationships, cast-aware column types) |
| [🔍 Find References](docs/find-references.md) | Every call site across the project, vendor packages included — including the magic-member usages Intelephense can't see |
| [✏️ Rename](docs/rename.md) | Atomic rename of routes, configs, translations, env vars, views, components, Livewire, middleware, bindings, model classes, magic members — and database columns, migration included |
| [🔢 Code Lens](docs/code-lens.md) | Opt-in reference counts above magic members, routes, config / translation / env keys, and Blade templates — plus an unused-symbol warning |
| [💡 Autocomplete](docs/autocomplete.md) | Cast types, model properties, query chains, builder methods, Blade / loop / slot variables, Pennant flags |
| [❌ Diagnostics](docs/diagnostics.md) | Missing views / components / features, invalid rules, query-chain typos against your real schema |
| [⚡ Quick Actions](docs/quick-actions.md) | One-click create missing views, components, middleware, features, and migrations |
| [🎨 Blade Editing](docs/blade-editing.md) | Directive autocomplete, smart bracket expansion, closing-tag navigation |
| [🗺️ Outline Panel](docs/outline.md) | Laravel-aware route + Blade structure in Zed's outline and breadcrumbs |

## 📦 Install

Search **"Laravel"** in Zed Extensions and click Install.

### 🤝 Recommended companions

Also install the following extensions for a more complete experience:
- [**Laravel Blade**](https://github.com/bajrangCoder/zed-laravel-blade) extension (`bajrangCoder/zed-laravel-blade`) for Blade-related fetures, syntax highlighting, etc.
- [**PHP**](https://github.com/zed-extensions/php) (Intelephense) for php intellisense functionality
- [**phpcs**](https://github.com/mike-bronner/zed-phpcs-lsp) PHP CodeSniffer linting
- [**phpmd**](https://github.com/mike-bronner/zed-phpmd-lsp) PHP Mess Detector linting

### From source

Clone the repo, run `cargo build --release` in `laravel-lsp/`, then use "zed: install dev extension".

## ⚙️ Configuration

The extension works out of the box with zero configuration. It automatically discovers your Laravel project structure, including view paths, component namespaces, route files, and service providers.

Everything below is optional.

### 🎛️ Extension settings

Add any of these to your Zed `settings.json`:

```json
{
  "lsp": {
    "laravel-lsp": {
      "settings": {
        "autoCompleteDebounce": 200,
        "blade": {
          "directiveSpacing": false
        },
        "codeLens": {
          "enabled": false
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
| `codeLens.enabled` | `false` | Turn on [reference-count code lenses](docs/code-lens.md) and the unused-symbol diagnostic. Opt-in while the feature matures. |
| `diagnostics.severity` | `"warning"` | Severity for query-chain diagnostics (unknown column/relation/table in Eloquent & `DB::table()` chains). One of `"warning"`, `"error"`, `"info"`, or `"off"` to disable. Requires a working database connection — diagnostics stay silent when the schema can't be introspected. |

### 🗄️ Database connection

**Database autocomplete** (`exists:` / `unique:` rules, Eloquent properties) and query-chain diagnostics only work with a live database connection. Configure it in your `.env`:

```env
DB_CONNECTION=mysql
DB_HOST=127.0.0.1
DB_DATABASE=myapp
DB_USERNAME=root
DB_PASSWORD=secret
```

Supports MySQL, PostgreSQL, SQLite, and SQL Server.

### 🌱 `.env` "appears unused" warnings

Open a `.env` and Zed underlines every line — `APP_NAME appears unused. Verify use (or export if used externally)`. That's **shellcheck's SC2034**, not this extension: Zed lints `.env` files as *Shell Script*, where the `KEY=value` lines Laravel reads at runtime look like unused variables. Silence just that rule while keeping shell highlighting, via your `settings.json`:

```json
{
  "lsp": {
    "bash-language-server": {
      "settings": {
        "bashIde": {
          "shellcheckArguments": ["--exclude=SC2034"]
        }
      }
    }
  }
}
```

The `bashIde` wrapper is required — the bash server won't see the setting without it. Prefer a project-scoped `.shellcheckrc`, a per-file directive, or reclassifying `.env` away from Shell Script entirely (so real shell scripts keep SC2034)? See the **[environment files guide](docs/environment.md)** for all the options and trade-offs.

### 🎨 Blade directive highlighting

The [Laravel Blade](https://github.com/bajrangCoder/zed-laravel-blade) extension already highlights standard directives and paired `@custom … @endcustom` blocks through tree-sitter. This optional setting adds the one case tree-sitter can't see: your app's **custom inline directives** registered via `Blade::directive()` (e.g. a `@money($amount)` macro). The LSP highlights them precisely — it colors only directives it has actually discovered (the same scan that drives directive completion), so PHPDoc `@param` tags, CSS at-rules like `@media`, and the `@` in email addresses are left alone, and commented-out directives stay dark. Enable it in your Zed `settings.json`:

```json
{
  "languages": {
    "Blade": {
      "semantic_tokens": "combined"
    }
  }
}
```

### 🗺️ Outline panel

The extension populates Zed's outline panel and breadcrumbs with Laravel-specific structure that no PHP language server understands:

- **Route files** — every `Route::get/post/...` call labelled `METHOD URI [name=...]`, with nested `Route::group(...)` calls becoming hierarchical containers labelled `group [prefix=..., name=...]`. Prefix and name chains propagate to children. Covers all Route methods including `resource`, `apiResource`, `singleton`, `livewire`, `view`, `redirect`, `fallback`, etc.
- **Blade templates** — `@extends`, `@section`, `@push`, `@yield`, `@stack`, `@include*`, `@props`, plus the modern tag syntax: `<x-component>`, `<livewire:counter>`, `<flux:icon>`, `<x-slot:name>`. Paired tags nest their children; self-closing tags appear as leaves.

PHP class outlines (controllers, models, Livewire components, jobs, services) come from whatever PHP language server you have installed — those servers have real semantic understanding of PHP that a tree-sitter walker can't match. The official [**PHP**](https://github.com/zed-extensions/php) Zed extension registers Intelephense, Phpactor, and PhpTools; install it and pick whichever LSP you prefer.

**Requirements**

| Outline | Requires |
|---|---|
| Route files | This extension, plus `document_symbols: on` for `PHP` (route files use the `PHP` language). |
| Blade templates | This extension, the [Laravel Blade](https://github.com/bajrangCoder/zed-laravel-blade) extension (for the `Blade` language definition), plus `document_symbols: on` for `Blade`. |
| PHP class files | A PHP language server (the [PHP](https://github.com/zed-extensions/php) extension provides Intelephense / Phpactor / PhpTools), plus `document_symbols: on` for `PHP`. |

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

## 🚧 Planned Features

**Rename — remaining work** (the class-backed kinds, the Eloquent model-class engine, magic members, and database columns shipped; variables didn't):

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
<a href="https://github.com/mike-bronner/zed-laravel/blob/main/LICENSE">MIT</a> · <a href="https://github.com/mike-bronner">mike-bronner</a>
</p>
