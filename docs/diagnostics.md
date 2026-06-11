# ❌ Diagnostics

[← Back to README](../README.md)

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

**Dynamic strings never produce phantom warnings.** An interpolated key like `config("{$config}.export_connection")` is either *resolved* — when `$config` is a single same-scope string-literal assignment, the full key is reconstructed and validated normally — or *skipped entirely*. The literal fragment (`.export_connection`) is never mistaken for a complete key, so keys built at runtime can't trigger a false "Config not found." The same skip applies to interpolated view, route, translation, env, and asset strings.

These diagnostics **under-warn on purpose**: they stay silent on a cold or absent schema, unresolved receivers, qualified/aliased/expression literals, and raw SQL — a missing squiggle never means "this is definitely fine," only "we couldn't prove it's wrong." Severity is configurable via `diagnostics.severity` (`warning` / `error` / `info` / `off`); see [Configuration](../README.md#️-configuration). A working database connection is required — column and relation linting silently disables when the schema can't be introspected.

**Unused-symbol warnings** (opt-in, via `codeLens.enabled`) flag lensed symbols — magic members, route names, config / translation / env keys — that have zero non-test references, with the same under-warning philosophy: vendor-aware (a model's `$timestamps`, read only by the framework, is never flagged) and worded as a question, since a zero count can also mean dynamic usage static analysis can't see. Details in [Code Lens](code-lens.md).
