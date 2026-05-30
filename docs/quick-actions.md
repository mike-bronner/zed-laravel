# ⚡ Quick Actions

[← Back to README](../README.md)

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
