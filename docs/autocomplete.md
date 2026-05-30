# 💡 Autocomplete

[← Back to README](../README.md)

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

## 🎭 Eloquent Cast Types

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

## 🏗️ Eloquent Model Properties

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

## 🔗 Eloquent Query Chains

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

> 🗄️ Column and relation completion needs a working database connection (see [Configuration](../README.md#️-configuration)). Without one, the chain still parses — you just won't get column suggestions.

## 🧰 Query Builder Methods

Type `Model::wher` and the extension surfaces the query-builder methods that PHP routes through `Model::__callStatic` — `where`, `whereIn`, `find`, `first`, `firstOrFail`, and dozens more. Laravel's `Model.php` carries no `@method` or `@mixin` tags for these, so most PHP language servers miss them at the static-call position; we fill the gap by parsing the `Builder` / `Query\Builder` classes (and their composed traits) from *your* installed `vendor/laravel/framework`:

```php
Portfolio::wher
//        ^ 🧰 where, whereIn, whereNot, whereBetween…   (Builder<static>)
//        🎯 active, popular…                            (your scopeActive / scopePopular methods)
//        🪄 whereName, whereEmail, orWhereCreatedAt…    (dynamic where{Column}, synthesized per column)
```

Model **scopes** (`scopeActive` → `active`) and **dynamic `where{Column}` finders** are included. The dynamic finders are synthesized from the model's columns — `$fillable`, `$casts`, conventions, and the live schema — and only when no real method of that name already exists, mirroring exactly what PHP's magic methods would route at runtime. Scoped to the `Model::` static position; instance chains (`->`) yield to your PHP LSP, which already sees them via `Builder`'s `@mixin`. Every item we add is attributable in its docs panel header, so you can tell ours apart from your PHP LSP's.

## 📝 Blade Variables

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

## 🔄 Loop Variables (Scope-Aware)

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

## 🎰 Slot Variables (Components)

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

## 🚩 Laravel Pennant Feature Flags

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
