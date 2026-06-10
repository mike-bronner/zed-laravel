# 🔗 Go-to-Definition

[← Back to README](../README.md)

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

**Eloquent magic members** resolve through the semantic index — the usage jumps to the declaration that actually backs it, even when the names don't match textually:

```php
$user->posts;
//     ^^^^^ → app/Models/User.php  (public function posts(): HasMany)

User::active()->get();
//    ^^^^^^ → app/Models/User.php  (public function scopeActive(...))

$user->full_name;
//     ^^^^^^^^^ → app/Models/User.php  (public function getFullNameAttribute())

$user->email;
//     ^^^^^ → database/migrations/..._create_users_table.php  ($table->string('email'))

User::whereEmail($value);
//   ^^^^^^^^^^ dynamic finder → the email column's migration line
```

Resolution is inheritance- and trait-aware — a member declared in a trait or a parent model jumps to the file that declares it. Plain properties and plain method calls are left to your PHP language server (no duplicate results). Dynamic finders classify against the model's source-visible column surface (`$casts`, `$fillable`, timestamps) — a `$guarded = []` model that declares neither won't resolve its finders.

**Artisan command strings** jump to the `Command` class declaring the matching `protected $signature` — across all four invocation patterns, with app-defined commands taking priority over same-named package/framework commands:

```php
Artisan::call('emails:send');
//             ^^^^^^^^^^^ → app/Console/Commands/SendEmails.php  (protected $signature)

$schedule->command('emails:send --queue')->daily();
//                  ^^^^^^^^^^^ → same — options/arguments after the name are ignored
```

**Supported patterns:**
`view()` `View::make()` `@extends` `@include` `@component` `<x-*>` `</x-*>` `<livewire:*>` `</livewire:*>` `@livewire()` `route()` `to_route()` `signed_route()` `URL::signedRoute()` `config()` `Config::get()` `Config::getMany()` `config()->string()` `env()` `Env::get()` `__()` `trans()` `@lang` `->middleware()` `app()` `resolve()` `App::bound()` `App::isShared()` `asset()` `@vite` `app_path()` `base_path()` `storage_path()` `resource_path()` `public_path()` `Feature::active()` `Feature::inactive()` `Feature::value()` `@feature` `Artisan::call()` `Artisan::queue()` `->command()` `->artisan()` · query-chain columns / relations / tables · magic members (relationships, scopes, accessors, columns, dynamic finders)
