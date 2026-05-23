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

**🗄️ Database autocomplete** (`exists:`, `unique:` rules, Eloquent properties) requires a working database connection. Configure in your `.env`:

```env
DB_CONNECTION=mysql
DB_HOST=127.0.0.1
DB_DATABASE=myapp
DB_USERNAME=root
DB_PASSWORD=secret
```

Supports MySQL, PostgreSQL, SQLite, and SQL Server.

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

`document_symbols: on` for `PHP` unlocks both our route outline (for files under `routes/`) and your PHP LSP's class outline (for everything else). `document_symbols: on` for `Blade` unlocks our Blade outline. Editors that call `textDocument/documentSymbol` unconditionally (Helix, Neovim, Sublime/LSP, Kate) need no opt-in.

> **Quirks worth knowing** — Zed colors outline labels by word-matching them against the source buffer's tree-sitter highlights, which produces slightly inconsistent colors on multi-segment URLs (e.g., `/cra-details` may color `cra` and `details` differently if they match different tokens elsewhere in the file). Route names appear in the LSP `detail` field, which Zed's outline panel doesn't currently render (VSCode and Sublime/LSP do). Both are tracked upstream: [zed#57576](https://github.com/zed-industries/zed/issues/57576).

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

**Supported patterns:**
`view()` `View::make()` `@extends` `@include` `@component` `<x-*>` `</x-*>` `<livewire:*>` `</livewire:*>` `@livewire()` `route()` `to_route()` `config()` `Config::get()` `env()` `__()` `trans()` `@lang` `->middleware()` `app()` `resolve()` `asset()` `@vite` `app_path()` `base_path()` `storage_path()` `resource_path()` `public_path()` `Feature::active()` `Feature::inactive()` `Feature::value()` `@feature`

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

Features are discovered from `app/Features/*.php` class files. Both string keys (`'new-api'`) and class references (`NewApi::class`) are supported.

### ❌ Diagnostics

See problems in real-time as you type. The extension validates your Laravel code against your actual project structure, highlighting missing views, undefined components, invalid validation rules, and other issues before you run your application.

**Missing files are reported as errors** to catch issues early:

```php
return view('users.dashboard');
//          ^^^^^^^^^^^^^^^^^ ❌ View not found: resources/views/users/dashboard.blade.php

Route::middleware('admin-only')->group(...);
//                ^^^^^^^^^^^^ ⚠️ Middleware not found

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

### ⚡ Quick Actions

Fix problems with a single click. When you see a warning, press `Cmd+.` to open quick actions. The extension offers to create missing files with the correct Laravel structure—views, components, middleware, translations, and more.

```php
return view('users.dashboard');
//          ^^^^^^^^^^^^^^^^^ ⚠️ View not found
//                            ⚡ Create view: users.dashboard

Route::middleware('admin-only')->group(...);
//                ^^^^^^^^^^^^ ⚠️ Middleware not found
//                             ⚡ Create middleware: admin-only
```

```blade
<x-dashboard-widget />
{{-- ⚠️ Component not found
     ⚡ Create component (anonymous)
     ⚡ Create component with class --}}

<livewire:admin-panel />
{{-- ⚠️ Livewire component not found
     ⚡ Create Livewire component --}}
```

**Available quick actions:**
- 📄 Create missing views
- 🧩 Create Blade components (anonymous or with class)
- ⚡ Create Livewire components
- 🛡️ Create middleware
- 🚩 Create Laravel Pennant feature classes
- 🌐 Add translations to existing files
- 🔐 Add environment variables to `.env`

### 🎨 Blade Editing Support

#### Directive Autocomplete

Type `@` to see all 100+ Blade directives with descriptions:

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

- 📖 Hover documentation with resolved values
- 🎨 Inertia.js support (`Inertia::render('Page')`)
- 📁 Folio page routing
- ⚡ Volt component support

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
