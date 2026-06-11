# 🔢 Code Lens

[← Back to README](../README.md)

Reference-count lenses above the declaration sites this extension indexes accurately — including the Eloquent magic members no generic PHP language server can count. Click a lens to open the full reference list (the same results as [Find References](find-references.md)).

```php
class User extends Model
{
    // 4 references
    public function posts(): HasMany { ... }

    // 2 references
    public function scopeActive(Builder $query): void { ... }

    // 0 references — possibly dead code?
    public function getInitialsAttribute(): string { ... }
}
```

**Opt-in while the feature matures.** Lenses (and the unused-symbol diagnostic below) are off by default — enable them in your Zed `settings.json`:

```json
{
  "lsp": {
    "laravel-lsp": {
      "settings": {
        "codeLens": {
          "enabled": true
        }
      }
    }
  }
}
```

## What gets a lens

| Declaration | File | Counted references |
|---|---|---|
| Eloquent magic members — relationships, scopes, accessors, public properties | model classes | property-form (`$user->posts`), call-form (`->active()`, `User::whereEmail()`), and Blade usages |
| Livewire / Volt component members — `#[Computed]` methods + public properties | component classes, Volt SFC front-matter | template + class usages |
| Route names — `->name('x')` / `->as('x')` | `routes/*.php` | every `route('x')` call site |
| Config keys — leaf *and* intermediate | `config/*.php` | every `config('file.key')` call site |
| Translation keys | `lang/<locale>/*.php` | every `__()` / `trans()` / `@lang` call site |
| Env keys — `KEY=value` lines | `.env*` | every `env('KEY')` call site |
| The whole template (file-level lens) | `*.blade.php` | its view + component + Livewire identities, summed |

Details worth knowing:

- **Route lenses compose prefixes correctly** — a `->name('users')` nested in `Route::name('admin.')->group(…)` counts `route('admin.users')` usages, and a route file loaded under several external prefixes gets one lens per resulting route name.
- **Translation lenses are locale-agnostic** — `auth.failed` is the same key in every locale, so any locale's file shows the same counts. (JSON translation files are a planned follow-up, [#66](https://github.com/mike-bronner/zed-laravel/issues/66).)
- **Config and translation counts are chain-aware** — `config('reporting.redshift_sync.enabled')` reaches *through* the `redshift_sync` array, so it counts toward the lens on `redshift_sync` (and `reporting`'s file) as well as on `enabled` itself. A parent key whose children are all referenced is never branded dead code.
- **Simple dynamic keys resolve** — `config("{$config}.export_connection")` counts toward the real key when `$config` is assigned a single string literal in the same scope (`$config = 'reporting.redshift_sync';`). Keys built from properties, parameters, or reassigned variables stay invisible to counting — and are exempted from "not found" diagnostics rather than guessed at.
- **A Blade file can be two things at once** — a template under `components/` is reachable both as `<x-name>` and as `view('components.name')`; the file-level lens sums both so neither set of references is lost.
- **Counts resolve lazily** (`codeLens/resolve`) — the document never blocks on counting.
- **Both access forms count** — a relationship's lens sums property reads (`$user->posts`) and method calls (`$user->posts()`); a scope's lens counts its `->active()` / `User::active()` call sites.

**Deliberately scoped:** plain PHP method calls and class references get no lens — Intelephense and friends already cover those, and this extension only lenses what it counts *accurately* (the same suppress-duplicates-at-the-source policy as [Hover](hover.md)).

## ❓ Unused-symbol diagnostic

With `codeLens.enabled` on, any lensed symbol with **zero non-test references** also gets a warning:

```php
public function scopeArchived(Builder $query): void { ... }
//              ^^^^^^^^^^^^^ ⚠️ No references found — possibly dead code?

public function getSlugAttribute(): string { ... }
//              ^^^^^^^^^^^^^^^^ ⚠️ Referenced only in tests — possibly dead code?
```

It under-warns on purpose:

- **Worded as a question, severity warning** — a zero count can also mean dynamic or by-convention usage static analysis can't see (a URL-hit route, a `$model->$dynamic` access).
- **Vendor-aware** — before flagging a member, the inheritance chain (parents + traits, including `vendor/`) is checked for framework reads, so a model's `$timestamps` (read by `HasTimestamps`, never by app code) is not flagged.
- **Silent until the project index finishes warming** — a half-built index would flag everything.
