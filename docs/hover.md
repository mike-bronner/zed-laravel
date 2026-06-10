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

**Eloquent magic members** get semantic cards explaining what the magic actually is — the classification, the declaring class, and the method source that backs it:

```php
$user->posts
//     ^^^^^ hover →  Eloquent relationship — `posts` on `App\Models\User`
//                    public function posts() { return $this->hasMany(Post::class); }
//                    (the body reveals the target model)

User::active()
//    ^^^^^^ hover →  Eloquent scope — `active` on `App\Models\User`
//                    the scopeActive() query body

$user->email
//     ^^^^^ hover →  Database column — `email` on `App\Models\User`
//                    Type `string` (cast-aware: migrations first, live DB as fallback)
```

Scopes, accessors, relationships, columns, and dynamic finders (`whereEmail()`) are all covered. When the receiver's type had to be inferred rather than proven, the card says so (*receiver type inferred*). Plain properties Intelephense already understands get **no card** — duplicating its hover would just add noise.

**Artisan command strings** show the declaring `Command` class and its `$signature`:

```php
Artisan::call('emails:send');
//             ^^^^^^^^^^^ hover →  App\Console\Commands\SendEmails
//                                  protected $signature = 'emails:send {--queue}'
```

**Hovered patterns:** views, Blade components (anonymous *and* class-backed), Livewire components, routes, config keys, env vars, translations (including `vendor::namespace.key`), middleware aliases, container bindings, assets (`asset()`, `Vite::asset()`, `mix()`, `public_path()`, …), `url()`, Blade variables, Eloquent magic members, and Artisan command strings. The bottom-line source path renders as a `file://` link, so the whole card is click-to-open in any LSP client that supports markdown links.

Class-backed components and Livewire components show the `class Foo extends Component` signature and link to the PHP class; anonymous components fall back to the `@props([...])` declaration from the `.blade.php` template. Patterns without a meaningful target (directives, controller actions, Pennant features) stay silent rather than showing an empty card.
