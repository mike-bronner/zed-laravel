# 🗺️ Outline Panel

[← Back to README](../README.md)

When LSP outlines are enabled (see [Configuration](../README.md#️-configuration)), Zed's outline panel and breadcrumbs surface Laravel-aware structure that no PHP language server can see.

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
2. Your settings include `"document_symbols": "on"` under `languages.PHP` (see [Configuration](../README.md#️-configuration)).
