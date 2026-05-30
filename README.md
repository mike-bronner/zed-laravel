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

If you use Intelephense as your PHP language server, goto-definition on a class name (e.g. `User`) can land you in a Zed multi-buffer with several unrelated files — the actual class, plus generated stubs from [barryvdh/laravel-ide-helper](https://github.com/barryvdh/laravel-ide-helper), `.phpstorm.meta.php`, and `class User` stub templates that packages like Jetstream ship under `vendor/*/stubs/`. Intelephense indexes all of them by default.

Zed merges goto responses across every running LSP and only dedupes identical locations — distinct paths from the same logical click stay separate. So even though `laravel-lsp` returns nothing for bare class references (we don't claim that pattern), Intelephense's noisy results show through unfiltered.

#### Safe baseline

Tell Intelephense to skip `vendor/*/stubs/` — those are scaffold templates (Jetstream, Filament, etc.), never loaded at runtime. Excluding them costs nothing.

```json
{
  "lsp": {
    "intelephense": {
      "initialization_options": {
        "licenceKey": "~/intelephense/licence.txt"
      },
      "settings": {
        "files": {
          "exclude": ["**/stubs/**"]
        }
      }
    }
  }
}
```

Two things about this shape that catch people:

- **`initialization_options` vs `settings` are siblings.** `initialization_options` is the right home for the four startup-only keys Intelephense accepts (`licenceKey`, `clearCache`, `storagePath`, `globalStoragePath`); drop the block entirely if you don't have a licence. Everything else (including `files.exclude`) belongs in `settings`. Nesting `settings` inside `initialization_options` makes Intelephense silently ignore it.
- **No `intelephense` namespace inside `settings`.** Most VSCode-style Intelephense docs show keys nested under an `intelephense` object (e.g. `intelephense.files.exclude`). Don't do that here — Zed's PHP extension wraps your `settings` block inside `{ "intelephense": ... }` before sending it to the server. Adding your own `intelephense` key creates `intelephense.intelephense.files.exclude`, which the server silently ignores. Put `files.exclude` directly under `settings`.

After saving, restart Intelephense (`Cmd+Shift+P → lsp: restart`). The stub-template results disappear from class-name goto.

> ⚠️ **Cache caveat.** Intelephense keeps a persistent symbol index on disk, so already-indexed symbols can still surface after you add excludes. To force a rebuild, add `"clearCache": true` to `initialization_options` for one startup (then remove it — leaving it `true` re-indexes from scratch every launch). As a fallback, wipe `~/Library/Application Support/intelephense/` (macOS) while Zed isn't running.

#### More aggressive (with trade-offs)

If `_ide_helper_models.php` and `.phpstorm.meta.php` still cluttering goto bothers you more than the Intelephense fidelity they enable, add them to the exclude array:

```json
"exclude": [
  "**/stubs/**",
  "**/_ide_helper*.php",
  "**/.phpstorm.meta.php"
]
```

What you trade:

| Excluded | Intelephense loses | `laravel-lsp` covers |
|---|---|---|
| `_ide_helper*.php` | Eloquent dynamic attributes/methods (`$user->name`, `User::find()`), plus extra methods added to facades by third-party packages (e.g. Scout, Telescope, Spatie permissions) | Eloquent completion from the actual DB schema (more accurate than ide-helper docblocks). **Core framework facade resolution (`Auth::user()`, `Cache::get()`, `Route::get()`, etc.) keeps working without ide-helper** — Intelephense reads the `@method` PHPDoc tags directly off the facade source files at `vendor/laravel/framework/src/Illuminate/Support/Facades/*.php`. |
| `.phpstorm.meta.php` | Container-binding type narrowing (`app('cache')` → `CacheManager`) | Container-binding goto via the `Binding` pattern. |

If you use packages that extend Laravel facades with their own methods (Scout adds `Searchable` methods, Telescope extends `Gate`, etc.), keep `_ide_helper*.php` in to retain completion for those package-added methods. Core Laravel facades resolve fine without it.

#### Per-project

Drop the same exclude patterns into an `.intelephense.json` at your project root. Unlike the Zed `settings` block, this file is read directly by Intelephense, so it uses the standard namespaced shape:

```json
{
  "files.exclude": [
    "**/stubs/**",
    "**/_ide_helper*.php",
    "**/.phpstorm.meta.php"
  ]
}
```

## ✨ Features

### 🔗 Go-to-Definition

Navigate your Laravel codebase by Cmd+Clicking (or `Cmd+D`) on any recognized pattern. The extension understands Laravel's conventions and jumps directly to the source file, whether it's a view, component, route, config key, or translation.

```php
class UserController extends Controller
{
    public function show(User $user)
    {
        return view('users.profile', compact('user'));
        //          ^^^^^^^^^^^^^^^ → resources/views/users/profile.blade.php
    }
}
```

```blade
@extends('layouts.app')
{{--      ^^^^^^^^^^^ → resources/views/layouts/app.blade.php --}}

<x-button type="submit">Save</x-button>
{{-- ^^^^ → resources/views/components/button.blade.php --}}

<livewire:user-settings :user="$user" />
{{--       ^^^^^^^^^^^^^ → app/Livewire/UserSettings.php --}}
```

```php
$url = route('users.show', $user);
//           ^^^^^^^^^^^^ → routes/web.php

$name = config('app.name');
//             ^^^^^^^^^^ → config/app.php

$message = __('auth.failed');
//            ^^^^^^^^^^^^ → lang/en/auth.php
```

Cmd+Click also works on **query-chain literals** — columns jump to the migration line that defines them, relations to the relation method on the model, and `DB::table()` names to the create-table migration:

```php
User::where('email', $value)->with('posts');
//          ^^^^^ → database/migrations/..._create_users_table.php  ($table->string('email'))
//                                ^^^^^ → app/Models/User.php  (public function posts())

DB::table('users')->get();
//        ^^^^^ → database/migrations/..._create_users_table.php  (Schema::create('users'))
```

**Supported patterns:**
`view()` `View::make()` `@extends` `@include` `@component` `<x-*>` `</x-*>` `<livewire:*>` `</livewire:*>` `@livewire()` `route()` `to_route()` `signed_route()` `URL::signedRoute()` `config()` `Config::get()` `Config::getMany()` `config()->string()` `env()` `Env::get()` `__()` `trans()` `@lang` `->middleware()` `app()` `resolve()` `App::bound()` `App::isShared()` `asset()` `@vite` `app_path()` `base_path()` `storage_path()` `resource_path()` `public_path()` `Feature::active()` `Feature::inactive()` `Feature::value()` `@feature` · query-chain columns / relations / tables

### ℹ️ Hover

Hover any recognised pattern to get an Intelephense-style summary card — a header, the relevant source snippet, and a clickable link to the file it resolves to. No need to jump away from your current line to remember what a view, route, or config key points at.

```php
return view('users.profile');
//          ^^^^^^^^^^^^^^^ hover →  resources/views/users/profile.blade.php
//                                   @props([...]) declaration + click-to-open link

$url = route('users.show', $user);
//           ^^^^^^^^^^^^ hover →  Route::get('/users/{user}', ...)->name('users.show')
//                                 verb · URI · controller@action · click-to-open link

$tz = config('app.timezone');
//           ^^^^^^^^^^^^^^ hover →  'UTC'   (the resolved value, from config/app.php)
```

```blade
{{ $user->email }}
{{-- ^^^^^ hover →  App\Models\User::$email, its PHPDoc summary, and the declaration --}}
```

**Hovered patterns:** views, Blade components (anonymous *and* class-backed), Livewire components, routes, config keys, env vars, translations (including `vendor::namespace.key`), middleware aliases, container bindings, assets (`asset()`, `Vite::asset()`, `mix()`, `public_path()`, …), `url()`, and Blade variables. The bottom-line source path renders as a `file://` link, so the whole card is click-to-open in any LSP client that supports markdown links.

Class-backed components and Livewire components show the `class Foo extends Component` signature and link to the PHP class; anonymous components fall back to the `@props([...])` declaration from the `.blade.php` template. Patterns without a meaningful target (directives, controller actions, Pennant features) stay silent rather than showing an empty card.

### 🔍 Find References

Right-click any recognised pattern and choose **"Find All References"** (or `Shift+F12`) to surface every call site across the project — including inside installed Composer packages.

```php
$url = route('users.index', $user);
//           ^^^^^^^^^^^^^ Find references here →
//
//   📂 routes/web.php:42         ->name('users.index')
//   📂 app/Http/Controllers/UserController.php:67   redirect()->route('users.index')
//   📂 resources/views/nav.blade.php:12   <a href="{{ route('users.index') }}">
//   📂 vendor/some/package/src/Helpers.php:8   route('users.index')
```

**Pattern kinds covered:**

| Kind | Example | Where references come from |
|---|---|---|
| Views | `view('users.profile')` | every `view()` / `View::make()` / `@extends` / `@include` call site |
| Routes | `route('home')` | every `route()` call + the `->name(...)` declaration |
| Configs | `config('app.name')` | every `config()` / `Config::get()` call + the array-key in the source config file |
| Translations | `__('messages.key')` | every `__()` / `trans()` / `@lang` call + the array-key in every locale's lang file |
| Env vars | `env('APP_NAME')` | every `env()` call site |
| Blade components | `<x-button>` | every `<x-button>` opening/closing tag |
| Livewire | `<livewire:counter>` | tags AND `@livewire('counter')` directives |
| Middleware | `'auth'` | every `->middleware()` registration |
| Bindings | `app('cache')` | every `app()` / `resolve()` resolution |

**Parser-classified guarantee:** a coincidental string `'home'` sitting in an unrelated PHP literal is never returned. Only positions the parser has classified as the matching pattern kind appear in results. The LSP `includeDeclaration` flag is honoured — declaration sites (route names, config-key array entries, translation-key array entries) are included or excluded as the client asks.

### ✏️ Rename

Press `F2` (or right-click → **"Rename Symbol"**) on a route name, config key, translation key, environment variable, view, Blade component, Livewire component, middleware alias, or container binding. The extension rewrites every call site AND the declaration site (or moves the backing file) in one atomic operation.

You can also right-click a `.blade.php` file in Zed's file explorer → **Rename** → call sites update atomically with the file move.

**Route names** rewrite call sites and the `->name(...)` declaration together:

```php
// Before:
Route::get('/dashboard', DashboardController::class)->name('home');
// in a controller somewhere:
return redirect()->route('home');
// in a blade view:
<a href="{{ route('home') }}">

// After renaming 'home' → 'dashboard':
Route::get('/dashboard', DashboardController::class)->name('dashboard');
return redirect()->route('dashboard');
<a href="{{ route('dashboard') }}">
```

Route group prefixes compose correctly — renaming a route from inside `Route::group(['as' => 'admin.'], …)` rewrites only the leaf segment in the declaration while every call site still gets the full new dotted name.

**Config keys** rewrite call sites and the array-key in the source config file:

```php
// Before — config/app.php:
'timezone' => 'UTC',
// usage somewhere:
$tz = config('app.timezone');

// After renaming 'app.timezone' → 'app.tz':
'tz' => 'UTC',                 // only the leaf segment rewrites in config/
$tz = config('app.tz');        // call sites rewrite the full dotted form
```

**Translation keys** rewrite call sites AND the array-key in **every** locale's lang file:

```
lang/en/messages.php:  'welcome' => 'Welcome'  →  'greeting' => 'Welcome'
lang/es/messages.php:  'welcome' => 'Bienvenido'  →  'greeting' => 'Bienvenido'
lang/fr/messages.php:  'welcome' => 'Bienvenue'  →  'greeting' => 'Bienvenue'
// every `__('messages.welcome')` becomes `__('messages.greeting')`
```

**Environment variables** rewrite call sites AND the key in **every** `.env*` file at the project root that declares it — `.env`, `.env.local`, `.env.testing`, `.env.production`, `.env.staging`, `.env.example`, and any custom variant (`.env.qa`, `.env.docker`, etc.):

```
.env:             DB_HOST=127.0.0.1   →   DATABASE_HOST=127.0.0.1
.env.local:       DB_HOST=localhost   →   DATABASE_HOST=localhost
.env.testing:     DB_HOST=memory      →   DATABASE_HOST=memory
.env.production:  DB_HOST=prod.db     →   DATABASE_HOST=prod.db
.env.example:     DB_HOST=127.0.0.1   →   DATABASE_HOST=127.0.0.1
// every `env('DB_HOST')` becomes `env('DATABASE_HOST')`
```

**Views** move the `.blade.php` file and rewrite every call site:

```php
// Before — resources/views/users/profile.blade.php exists:
return view('users.profile');
@include('users.profile')
<x-card>{{ view('users.profile') }}</x-card>

// After renaming 'users.profile' → 'users.account':
//   File moved: resources/views/users/profile.blade.php → resources/views/users/account.blade.php
return view('users.account');
@include('users.account')
<x-card>{{ view('users.account') }}</x-card>
```

**Blade components** handle both anonymous and class-backed flavours. For class-backed components, the `app/View/Components/Foo.php` file is also moved, the `class Foo extends Component` declaration is rewritten, and the `namespace App\View\Components\…;` declaration is updated when the move crosses directories. Tag-site rewrites preserve the `x-` prefix.

**Livewire components** dispatch over four shapes (V4 SFC, V4 MFC, V3 Class, Volt) auto-detected from your `livewire.php` config and `composer.lock`. Both `<livewire:name>` tag form and `@livewire('name')` directive form get rewritten. Volt single-file components rename atomically. The MFC directory's children get renamed in place; the empty old directory is left behind as a known LSP-protocol limitation (LSPs can't delete directories atomically alongside child renames).

**Middleware aliases** rewrite the registration string at its source (in `Kernel.php`, `bootstrap/app.php`, or any service-provider `register()`) AND every `->middleware('x')` call site. Works for both the per-entry `'auth' => …` form and Laravel 11's bulk `$middleware->alias([…])` form. Parameterized references like `'auth:sanctum'` are refused with a clear message — rename the bare alias instead.

**Container bindings** follow the same shape as middleware aliases: the quoted name at the registration site PLUS every `app('x')`, `resolve('x')`, `app()->make('x')` call site.

**Eloquent model classes** rename project-wide. Press `F2` on a model class name and every reference rewrites in one pass — `use` imports, `User::` static calls, `new User`, type hints, `::class` references, `extends`/`implements`, `instanceof`, and `@param`/`@return`/`@var` docblocks — and the backing `.php` file is renamed alongside. Aliased imports are respected (`use App\Models\User as U;` keeps `U`), and members that just happen to share the class's name are left untouched. Same-namespace renames only — moving a class to a different namespace returns a status message rather than a half-applied move.

**Same parser-classified guarantee as Find References** — only positions the parser has tagged as the matching kind are mutated. A random string `'home'` in an unrelated literal is never touched.

**Vendor-located files refuse to rename** — never moves a Composer-installed view, component, or Livewire class, and never rewrites a middleware alias or binding registered inside `vendor/`. You'll see a toast explaining why instead of a silent no-op.

**Not yet renameable** (out of scope for this round, planned follow-up): Blade variables (`@foreach`, `@php` locals + the `view('x', ['key' => …])` / `compact('key')` linkage), PHP function-local variables. `prepare_rename` returns nothing for these so F2 silently does nothing.

### 💡 Autocomplete

Get intelligent suggestions as you type. The extension provides context-aware completions for views, Blade components, validation rules, Eloquent casts, database schemas, config keys, routes, middleware, translations, environment variables, Eloquent models, and Blade variables.

```php
$request->validate([
    'email' => 'required|email|exists:',
    //                               ^ 🗄️ database tables appear here

    'email' => 'required|email|exists:users,',
    //                                     ^ 🗄️ column names appear here

    'name' => 'required|',
    //                  ^ 📋 90+ validation rules appear here
]);

$name = config('app.');
//                  ^ ⚙️ config keys with resolved values

return view('users.');
//                 ^ 📄 view names from resources/views

$url = route('users.');
//                  ^ 🔗 named routes from routes/*.php

Route::middleware('');
//                ^ 🛡️ middleware aliases from bootstrap/app.php

$message = __('auth.');
//                  ^ 🌐 translation keys with values
```

#### 🎭 Eloquent Cast Types

Get autocomplete for Eloquent cast types in `$casts` property or `casts()` method:

```php
protected $casts = [
    'is_admin' => '',
    //            ^ 🎭 cast types appear here
];

protected function casts(): array
{
    return [
        'email_verified_at' => 'datetime',
        'settings' => '',
        //            ^ string, integer, boolean, datetime, array,
        //              encrypted, hashed, collection, object...
    ];
}
```

Cast completions include:
- **Primitives:** `string`, `integer`, `float`, `boolean`, `array`, `object`, `collection`
- **Dates:** `datetime`, `date`, `timestamp`, `immutable_date`, `immutable_datetime`
- **Security:** `encrypted`, `encrypted:array`, `encrypted:collection`, `hashed`
- **Numbers:** `decimal:` (with precision parameter)
- **Custom casts** from `app/Casts/` and installed packages

#### 🏗️ Eloquent Model Properties

Type `$user->` to get completions for model properties, including database columns, casts, accessors, and relationships:

```php
$user->
//    ^ name (string)        ← database column
//    ^ email (string)       ← database column
//    ^ email_verified_at (Carbon)  ← cast to datetime
//    ^ is_admin (bool)      ← cast to boolean
//    ^ full_name (string)   ← accessor
//    ^ posts (Collection)   ← hasMany relationship
```

Works with type-hinted variables, PHPDoc annotations, and static chains like `User::find(1)->`.

#### 🔗 Eloquent Query Chains

Type a string literal inside a query-builder method and the extension completes it from your schema and model definitions — **columns** inside `where`, `orderBy`, `whereIn`, `pluck`, `select`, … and **relations** inside `with`, `whereHas`, `withCount`, `load`, `has`, …:

```php
User::where('')->orderBy('');
//          ^ 🗄️ columns of the users table        ^ 🗄️ same

User::with('');
//         ^ 🔗 relation methods on User (posts, profile, roles…)

DB::table('')->where('');
//        ^ 🗄️ table names    ^ 🗄️ columns of the chosen table

User::whereHas('posts', fn ($q) => $q->where(''));
//                                           ^ 🗄️ columns of the *Post* model (relation hop)

User::with('posts.author.');
//                       ^ 🔗 relations of the final model in the dotted path
```

Chains rooted at a variable resolve through intervening statements and conditional rebuilds:

```php
$query = User::query();
if ($active) { $query = $query->where('active', true); }
$query->orderBy('');
//              ^ 🗄️ still completes User's columns
```

**Joined tables** are fully resolved. After a `join`, `leftJoin`, or closure-form join, bare columns complete against *every* accessible table and `qualifier.` narrows to one; `from`/`fromSub`/`joinSub`/`lateral` are handled too (subquery `SELECT` lists become virtual columns):

```php
DB::table('orders')->join('users', 'users.id', '=', 'orders.user_id')->where('');
//                                                                            ^ 🗄️ orders + users columns
DB::table('orders')->join('users', ...)->where('users.');
//                                                     ^ 🗄️ narrowed to users
```

Columns are read live from your database (cast-aware), so completions reflect the *actual* schema. Relations come from the model's relation methods, walking parent classes and traits. Works in PHP **and** in Blade-embedded expressions (`@php`, `{{ }}`). Raw-SQL methods (`whereRaw`, `havingRaw`, `selectRaw`, `DB::raw`, …) are deliberately left to your PHP language server — their arguments are opaque SQL, not column names.

> 🗄️ Column and relation completion needs a working database connection (see [Configuration](#️-configuration)). Without one, the chain still parses — you just won't get column suggestions.

#### 🧰 Query Builder Methods

Type `Model::wher` and the extension surfaces the query-builder methods that PHP routes through `Model::__callStatic` — `where`, `whereIn`, `find`, `first`, `firstOrFail`, and dozens more. Laravel's `Model.php` carries no `@method` or `@mixin` tags for these, so most PHP language servers miss them at the static-call position; we fill the gap by parsing the `Builder` / `Query\Builder` classes (and their composed traits) from *your* installed `vendor/laravel/framework`:

```php
Portfolio::wher
//        ^ 🧰 where, whereIn, whereNot, whereBetween…   (Builder<static>)
//        🎯 active, popular…                            (your scopeActive / scopePopular methods)
//        🪄 whereName, whereEmail, orWhereCreatedAt…    (dynamic where{Column}, synthesized per column)
```

Model **scopes** (`scopeActive` → `active`) and **dynamic `where{Column}` finders** are included. The dynamic finders are synthesized from the model's columns — `$fillable`, `$casts`, conventions, and the live schema — and only when no real method of that name already exists, mirroring exactly what PHP's magic methods would route at runtime. Scoped to the `Model::` static position; instance chains (`->`) yield to your PHP LSP, which already sees them via `Builder`'s `@mixin`. Every item we add is attributable in its docs panel header, so you can tell ours apart from your PHP LSP's.

#### 📝 Blade Variables

Type `$` in Blade files to see all available variables passed to the view:

```blade
{{ $
{{-- ^ user (User)     ← from controller
     ^ posts (Collection) ← from controller
     ^ title (string)  ← from @props --}}
```

Variables are resolved from:
- `view('name', compact('user', 'posts'))`
- `view('name', ['user' => $user])`
- `view('name')->with('user', $user)`
- `view('name')->with(['user' => $user])`
- `@props(['title' => string])` in Blade components
- Livewire component public properties

#### 🔄 Loop Variables (Scope-Aware)

Variables from loop directives are available **only inside** the loop block:

```blade
@foreach($users as $user)
    {{ $user->name }}   {{-- ✅ $user available here --}}
    {{ $loop->index }}  {{-- ✅ $loop available in all loops --}}
@endforeach
{{ $user }}  {{-- ❌ $user NOT available outside loop --}}
```

Supported loop directives:
- `@foreach($items as $item)` / `@foreach($items as $key => $value)`
- `@forelse($items as $item)`
- `@for($i = 0; $i < 10; $i++)`
- `@while($condition)`

Nested loops work correctly—inner loop variables are scoped to their block.

#### 🎰 Slot Variables (Components)

In component files (`resources/views/components/*.blade.php`), slot variables are detected from usage:

```blade
{{-- components/card.blade.php --}}
<div class="card">
    <header>{{ $header }}</header>   {{-- $header autocomplete available --}}
    <div>{{ $slot }}</div>           {{-- $slot always available --}}
    <footer>{{ $footer }}</footer>   {{-- $footer autocomplete available --}}
</div>
```

Component files automatically get:
- `$slot` — default slot content
- `$attributes` — component attribute bag
- `$component` — component instance
- Named slots detected from `{{ $name }}` usage

#### 🚩 Laravel Pennant Feature Flags

Get autocomplete for Laravel Pennant feature flags in PHP and Blade:

```php
Feature::active('');
//               ^ 🚩 feature names from app/Features/

Feature::for($user)->active('');
//                          ^ 🚩 same completions for scoped checks

Feature::allAreActive(['']);
//                     ^ 🚩 works in array methods too
```

```blade
@feature('')
{{--     ^ 🚩 feature names appear here --}}
```

Features are discovered from `app/Features/*.php` class files. String keys (`'new-api'`) get autocomplete; class references (`NewApi::class`) are resolved for go-to-definition and diagnostics.

### ❌ Diagnostics

See problems in real-time as you type. The extension validates your Laravel code against your actual project structure, highlighting missing views, undefined components, invalid validation rules, and other issues before you run your application.

**Missing files — views, components, Livewire components, features, and invalid validation rules — are reported as errors** to catch issues early. (These existence checks are always errors; the `diagnostics.severity` setting only controls the query-chain diagnostics below.)

```php
return view('users.dashboard');
//          ^^^^^^^^^^^^^^^^^ ❌ View not found: resources/views/users/dashboard.blade.php

Route::middleware('admin-only')->group(...);
//                ^^^^^^^^^^^^ ❌ Middleware not found

$request->validate([
    'email' => 'required|emal|unique:users',
    //                   ^^^^ ❌ Unknown validation rule: 'emal'
]);

Feature::active('undefined-feature');
//               ^^^^^^^^^^^^^^^^^^ ❌ Feature not found: app/Features/UndefinedFeature.php
```

```blade
<x-dashboard-widget />
{{-- ^^^^^^^^^^^^^^^^ ❌ Component not found --}}

<livewire:admin-panel />
{{--       ^^^^^^^^^^^ ❌ Livewire component not found --}}

@extends('layouts.missing')
{{--      ^^^^^^^^^^^^^^^^ ❌ View not found --}}

@feature('undefined-feature')
{{--      ^^^^^^^^^^^^^^^^^^ ❌ Feature not found --}}
```

**Query-chain typos** are caught against your real schema — unknown columns, relations, and tables each get a Levenshtein "did you mean" suggestion:

```php
User::where('emial', $value);
//          ^^^^^ ⚠️ Unknown column 'emial' on users — did you mean 'email'?

User::with('postz');
//         ^^^^^ ⚠️ Unknown relation 'postz' on User — did you mean 'posts'?

DB::table('userz')->get();
//        ^^^^^ ⚠️ Unknown table 'userz' — did you mean 'users'?

User::whereEmial($value);
//   ^^^^^^^^^^^ ⚠️ Unknown column 'emial' (dynamic where) — did you mean 'whereEmail'?
```

When joins put a bare column on more than one accessible table, it's flagged as **ambiguous** so you can qualify it:

```php
DB::table('orders')->join('users', ...)->where('id', 1);
//                                              ^^ ⚠️ Ambiguous column 'id' — exists on orders and users
```

These diagnostics **under-warn on purpose**: they stay silent on a cold or absent schema, unresolved receivers, qualified/aliased/expression literals, and raw SQL — a missing squiggle never means "this is definitely fine," only "we couldn't prove it's wrong." Severity is configurable via `diagnostics.severity` (`warning` / `error` / `info` / `off`); see [Configuration](#️-configuration). A working database connection is required — column and relation linting silently disables when the schema can't be introspected.

### ⚡ Quick Actions

Fix problems with a single click. When you see a warning, press `Cmd+.` to open quick actions. The extension offers to create missing files with the correct Laravel structure—views, components, middleware, translations, and more.

```php
return view('users.dashboard');
//          ^^^^^^^^^^^^^^^^^ ❌ View not found
//                            ⚡ Create view: users.dashboard

Route::middleware('admin-only')->group(...);
//                ^^^^^^^^^^^^ ❌ Middleware not found
//                             ⚡ Create middleware: admin-only
```

```blade
<x-dashboard-widget />
{{-- ❌ Component not found
     ⚡ Create component (anonymous)
     ⚡ Create component with class --}}

<livewire:admin-panel />
{{-- ❌ Livewire component not found
     ⚡ Create Livewire component --}}
```

Query-chain diagnostics carry their own fixes:

```php
User::where('emial');
//          ^^^^^ ⚡ Rename to 'email'
//                ⚡ Create migration: add column 'emial' to users

DB::table('orders')->join('users', ...)->where('id', 1);
//                                              ^^ ⚡ Qualify as 'orders.id'
//                                                 ⚡ Qualify as 'users.id'
```

The "Create migration" action scaffolds a timestamped `database/migrations/*.php` using your project's `migration.update.stub` (custom → vendor → built-in fallback, the same resolution `php artisan make:migration` uses), so your own stub format is honoured.

**Available quick actions:**
- 📄 Create missing views
- 🧩 Create Blade components (anonymous or with class)
- ⚡ Create Livewire components
- 🛡️ Create middleware
- 🚩 Create Laravel Pennant feature classes
- 🌐 Add translations to existing files
- 🔐 Add environment variables to `.env`
- 🗄️ Rename a mistyped column / relation / table to the suggested name
- 🆕 Create a migration to add a missing column
- 🏷️ Qualify an ambiguous column as `table.column`

### 🎨 Blade Editing Support

#### Directive Autocomplete

Type `@` to see the Blade directives available in *your* project. The list is discovered live — Laravel's built-in directives are read from your installed framework, and **custom directives registered via `Blade::directive()`** (in your app or in packages) are picked up too, so a directive like `@feature` or your own `@money` shows up without us hardcoding it. A full Laravel app typically surfaces 100+; a built-in fallback set keeps completion working if the project can't be scanned.

```blade
@fo
  ↳ @foreach  Loop through collection
  ↳ @for      For loop
  ↳ @forelse  Loop with empty fallback
```

Block directives automatically include their closing tags:
```blade
@if($condition)
    █
@endif
```

#### Smart Bracket Expansion

Type `{` and select from snippet completions:

```blade
{
  ↳ {{ ... }}      Echo (escaped)
  ↳ {!! ... !!}    Echo (unescaped)
  ↳ {{-- ... --}}  Blade comment
```

#### Closing Tag Navigation

Cmd+Click works on both opening AND closing tags:

```blade
<x-button>Submit</x-button>
{{-- ^^^^^^           ^^^^^^ Both navigate to component --}}

<livewire:counter></livewire:counter>
{{--      ^^^^^^^            ^^^^^^^ Both navigate to Livewire class --}}
```

> **Note:** Blade syntax highlighting is provided by the separate [**Laravel Blade**](https://github.com/bajrangCoder/zed-laravel-blade) Zed extension. Install it alongside this extension for full Blade support. For enhanced directive highlighting — including correct coloring of custom directives that tree-sitter doesn't recognize — enable semantic tokens in your settings. See the [Configuration](#️-configuration) section above.

### 🗺️ Outline Panel

When LSP outlines are enabled (see [Configuration](#️-configuration)), Zed's outline panel and breadcrumbs surface Laravel-aware structure that no PHP language server can see.

**Route files** show each definition with HTTP verb, URI, and route name. Nested `Route::group(...)` calls become hierarchical containers labelled with the group's prefix and name; prefix and route-name chains inherit from the enclosing group:

```
routes/web.php
├─ GET  /                       [name=home]
├─ POST /login                  [name=login]
└─ group [prefix=/admin, name=admin.]
   ├─ GET  /users               [name=admin.users.index]
   ├─ POST /users               [name=admin.users.store]
   └─ group [prefix=/settings, name=admin.settings.]
      └─ GET /profile           [name=admin.settings.profile]
```

Recognised Route methods: `get`, `post`, `put`, `patch`, `delete`, `options`, `any`, `match`, `view`, `redirect`, `permanentRedirect`, `fallback`, `livewire`, `resource`, `apiResource`, `singleton`, `apiSingleton`.

**Blade templates** show the section / push / yield hierarchy alongside the modern tag-based component syntax. Paired tags nest their content; self-closing tags appear as leaves with ` />` so the source shape is visible at a glance:

```
resources/views/components/card.blade.php
├─ @props title
└─ <x-card>
   ├─ <x-slot:header>
   ├─ <x-card-body>
   └─ <livewire:card-footer />
```

```
resources/views/layouts/app.blade.php
├─ @extends layouts.master
├─ @include partials.nav
├─ @section content
│  ├─ @yield title
│  └─ @push scripts
└─ @section sidebar
```

Recognised constructs: `@extends`, `@section`/`@endsection`, `@push`/`@endpush`, `@prepend`/`@endprepend`, `@stack`, `@yield`, `@component`/`@endcomponent`, `@slot`/`@endslot`, `@props`, `@include`, `@includeIf`, `@includeWhen`, `@includeUnless`, `@includeFirst`, plus `<x-*>`, `<livewire:*>`, `<flux:*>`, `<x-slot:name>`, and `<x-slot name="...">`.

**PHP class files** (controllers, models, Livewire components, jobs, services, helpers) get their outline from whichever PHP language server you have installed — Intelephense, Phpactor, or PhpTools. We deliberately don't emit symbols for these files because Zed merges document-symbol responses across all LSPs serving a file, so any output from us would render twice in the outline panel. The PHP LSPs already provide semantically-rich PHP outlines that a tree-sitter walker can't match.

If your PHP class outline isn't appearing, make sure:

1. The official [**PHP**](https://github.com/zed-extensions/php) Zed extension is installed and one of its LSPs (Intelephense, Phpactor, PhpTools) is active for the file.
2. Your settings include `"document_symbols": "on"` under `languages.PHP` (see [Configuration](#️-configuration)).

## 🚧 Planned Features

**Rename — remaining work** (the class-backed kinds and the Eloquent model-class engine shipped; variables didn't):

- ✏️ **More PHP class kinds** — the FQCN rename engine (use-statement updates, type-hint / static-call / `new` / `::class` rewrites, docblocks, file move) now powers Eloquent model rename; extending it to controllers, jobs, services, and form requests as first-class symbols is the follow-up.
- 📝 **Blade variable rename** — scope-aware within a template (`@foreach`, `@php`, etc.), plus cross-file via the `view('x', ['key' => …])` / `compact('key')` linkage from controller into view.
- 🔧 **PHP variable rename** — scope-aware function-local. Class properties (`$this->foo`) are out of scope for this round and folded into a future class-property rename.

**Framework integrations:**

- 🎨 **Inertia.js support** — go-to-definition and autocomplete for `Inertia::render('Page')` calls
- 📁 **Folio page routing** — surface Folio's filesystem-routed pages in goto-definition / completion / find-references

## 🤝 Contributing

### Project Structure

```
zed-laravel/
├── src/lib.rs           # Zed extension (binary download/management)
├── extension.toml       # Extension manifest
├── laravel-lsp/         # Laravel Language Server (the actual LSP)
│   ├── src/main.rs      # LSP server implementation
│   ├── src/queries.rs   # Tree-sitter pattern extraction
│   └── tests/           # Integration tests
└── test-project/        # Laravel fixture for testing
```

### Local Development

1. **Clone and build the LSP:**

   ```bash
   git clone https://github.com/GeneaLabs/zed-laravel.git
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

### Running Tests

```bash
cd laravel-lsp

# Run all tests
cargo test

# Run with output
cargo test -- --nocapture

# Run specific test
cargo test test_view_resolution
```

### Code Style

```bash
# Format code
cargo fmt

# Run linter
cargo clippy
```

---

<p align="center">
<a href="https://github.com/GeneaLabs/zed-laravel/blob/main/LICENSE">MIT</a> · <a href="https://github.com/GeneaLabs">GeneaLabs</a>
</p>
