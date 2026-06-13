# ✏️ Rename

[← Back to README](../README.md)

Press `F2` (or right-click → **"Rename Symbol"**) on a route name, config key, translation key, environment variable, view, Blade component, Livewire component, middleware alias, container binding, PHP class (Eloquent model, controller, job, service, form request, or any other project class), magic member (relationship / scope / accessor), database column, scope-aware Blade template variable, or controller→view binding key. The extension rewrites every call site AND the declaration site (or moves the backing file, or generates the migration) in one atomic operation.

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

Route group prefixes compose correctly — renaming a route nested in `Route::name('admin.')->group(…)` rewrites only the leaf segment at the `->name(...)` declaration (`users` → `dashboard`), while every call site still gets the full new dotted name (`admin.users` → `admin.dashboard`). The inherited `admin.` group prefix is left untouched, so the effective name stays `admin.dashboard` instead of doubling to `admin.admin.dashboard`.

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

**PHP classes** rename project-wide — Eloquent models, controllers, jobs, services, form requests, and any other first-class project class share one FQCN rename engine. Press `F2` on a class name and every reference rewrites in one pass — `use` imports, `User::`/`UserController::` static calls, `new User`, type hints (constructor injection, action arguments, return types), `::class` references (including `[UserController::class, 'index']` route actions), `extends`/`implements`, `instanceof`, and `@param`/`@return`/`@var` docblocks — and the backing `.php` file is renamed alongside (same directory, basename swapped). Aliased imports are respected (`use App\Models\User as U;` keeps `U`), and members that just happen to share the class's name are left untouched. Vendor-located classes refuse to rename. Same-namespace renames only — moving a class to a different namespace returns a status message rather than a half-applied move.

**Eloquent magic members** rename from their usage sites. Press `F2` on a relationship, scope, or accessor usage and the declaring method renames with the *inverse name transform* applied — every cached usage follows:

```php
// F2 on `active` in:
User::active()->get();

// renames the declaration with the scope prefix re-applied:
public function scopeActive(Builder $query)   →   public function scopeArchived(Builder $query)
// and every ->active() / User::active() call site becomes ->archived()
```

The transforms run both ways: `active` ↔ `scopeActive`, `full_name` ↔ `getFullNameAttribute` (new-style `fullName(): Attribute` accessors are handled too), and a relationship's usage name maps verbatim to its method (`$user->posts` ↔ `posts()` — property reads and method calls rename together). All rewrites land in one `WorkspaceEdit`, so the editor's multi-file diff shows everything before you commit to it. Dynamic finders (`whereEmail()`) aren't renameable — rename the underlying column instead, which is the operation actually being asked for.

**Scope call-site coverage:** direct calls (`User::active()`, `$user->active()`), `self::` / `static::` calls, builder chains (`User::query()->active()`, `User::where(…)->active()`), and `$query->active()` inside scope bodies all rename together. Sites that can't be resolved statically are left untouched — `parent::` receivers, `(new User)->active()`, relation-hopped chains (`$user->posts()->active()` belongs to Post), and builder closures (`whereHas(…, fn ($q) => $q->active())`) — so scan the multi-file diff before applying. Factory states that share a scope's name (`User::factory()->active()`) are deliberately never rewritten.

**Database columns** get the full treatment — a column lives in the database, not in any one method, so renaming `$user->email` → `$user->primary_email` touches four site classes atomically:

1. **A generated migration** — a new timestamped `database/migrations/*_rename_email_to_primary_email_in_users_table.php` with a reversible `Schema::table(…, renameColumn(…))` (`up` and `down`), created as part of the same edit.
2. **Property-form usages** — `$user->email`, `{{ $user->email }}` in Blade.
3. **Model array entries** — the `'email'` string in `$fillable`, `$casts`, `$hidden`, `$guarded`, `$dates`.
4. **Query-chain column literals project-wide** — `where('email', …)`, `orderBy('email')`, `pluck('email')`, … rewritten *only* when the chain resolves to the column's table (with an enclosing-model fallback for local scopes). A qualified literal `'users.email'` rewrites only the `email` segment and only when the qualifier matches; the database itself is never touched — you review the diff, then run the migration.

**Blade template variables** rename scope-aware, straight from the `.blade.php` file. Press `F2` on a `$variable` and only the occurrences in its *actual* scope are rewritten:

```blade
{{-- F2 on $item inside the loop renames only the loop's $item --}}
@foreach ($items as $item)
    {{ $item->name }}      {{-- renamed --}}
@endforeach
{{ $item }}                {{-- an unrelated, file-level $item — left alone --}}
```

A variable introduced by `@foreach` / `@forelse` / `@for` is block-scoped — the rewrite stops at the loop's open/close directives, and a nested loop that re-binds the same name is treated as a separate scope (rename one without clobbering the other). A variable that *isn't* loop-introduced (a controller-passed view variable, an inline `@php $x = …; @endphp`) is file-scoped, but still skips any nested loop that shadows the name. Occurrences inside `{{-- … --}}` comments and `@verbatim` blocks are never touched.

**Controller → view binding rename** follows the data linkage so the key and its in-view usages move together. Press `F2` on the binding key in a controller:

```php
// F2 on 'name' in the controller:
return view('users.profile', ['name' => $user->name]);
//                             ^^^^ renamed to 'fullName'
// → resources/views/users/profile.blade.php: every file-scoped $name becomes $fullName
```

The same works for `view('users.profile', compact('name'))` — the `compact('name')` string AND the enclosing method's local `$name` are renamed alongside the in-view usages, so the controller stays valid (compact binds the view key *by the local's name*). When a view is rendered from several controllers under different key names, each rename touches only its own key's usages — different key names never cross-contaminate. The array value expression (`$user->name` above) is the controller's own data and is left untouched.

> ⚠️ **Intelephense overlap on relationship renames.** A relationship's usage name equals its method name, so Intelephense *also* understands `posts()` as a renameable method — on F2 both language servers may contribute edits for the declaration and the call-form sites. Scopes, accessors, and columns don't overlap (Intelephense can't connect `->active()` to `scopeActive()`), so this extension owns those cleanly. There's no surgical way to disable just Intelephense's rename — it has no `rename.enable` setting, and Zed has no per-capability toggle — though `intelephense.rename.exclude` can narrow its scope by glob. Tracked in [#74](https://github.com/mike-bronner/zed-laravel/issues/74).

**Same parser-classified guarantee as [Find References](find-references.md)** — only positions the parser has tagged as the matching kind are mutated. A random string `'home'` in an unrelated literal is never touched.

**Vendor-located files refuse to rename** — never moves a Composer-installed view, component, or Livewire class, and never rewrites a middleware alias or binding registered inside `vendor/`. You'll see a toast explaining why instead of a silent no-op.

**Not yet renameable** (out of scope for this round, planned follow-up): PHP function-local variables (a plain `$local` in a controller method that isn't a view-binding key), and class properties (`$this->foo`). `prepare_rename` returns nothing for these so F2 silently does nothing.
