# ℹ️ Hover

[← Back to README](../README.md)

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
