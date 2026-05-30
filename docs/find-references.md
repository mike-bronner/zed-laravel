# 🔍 Find References

[← Back to README](../README.md)

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
