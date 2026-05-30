# 🔧 Tuning Intelephense

[← Back to README](../README.md)

If you use Intelephense as your PHP language server, goto-definition on a class name (e.g. `User`) can land you in a Zed multi-buffer with several unrelated files — the actual class, plus generated stubs from [barryvdh/laravel-ide-helper](https://github.com/barryvdh/laravel-ide-helper), `.phpstorm.meta.php`, and `class User` stub templates that packages like Jetstream ship under `vendor/*/stubs/`. Intelephense indexes all of them by default.

Zed merges goto responses across every running LSP and only dedupes identical locations — distinct paths from the same logical click stay separate. So even though `laravel-lsp` returns nothing for bare class references (we don't claim that pattern), Intelephense's noisy results show through unfiltered.

## Safe baseline

Tell Intelephense to skip `vendor/*/stubs/` — those are scaffold templates (Jetstream, Filament, etc.), never loaded at runtime. Excluding them costs nothing.

```json
{
  "lsp": {
    "intelephense": {
      "initialization_options": {
        "licenceKey": "~/intelephense/licence.txt"
      },
      "settings": {
        "files": {
          "exclude": ["**/stubs/**"]
        }
      }
    }
  }
}
```

Two things about this shape that catch people:

- **`initialization_options` vs `settings` are siblings.** `initialization_options` is the right home for the four startup-only keys Intelephense accepts (`licenceKey`, `clearCache`, `storagePath`, `globalStoragePath`); drop the block entirely if you don't have a licence. Everything else (including `files.exclude`) belongs in `settings`. Nesting `settings` inside `initialization_options` makes Intelephense silently ignore it.
- **No `intelephense` namespace inside `settings`.** Most VSCode-style Intelephense docs show keys nested under an `intelephense` object (e.g. `intelephense.files.exclude`). Don't do that here — Zed's PHP extension wraps your `settings` block inside `{ "intelephense": ... }` before sending it to the server. Adding your own `intelephense` key creates `intelephense.intelephense.files.exclude`, which the server silently ignores. Put `files.exclude` directly under `settings`.

After saving, restart Intelephense (`Cmd+Shift+P → lsp: restart`). The stub-template results disappear from class-name goto.

> ⚠️ **Cache caveat.** Intelephense keeps a persistent symbol index on disk, so already-indexed symbols can still surface after you add excludes. To force a rebuild, add `"clearCache": true` to `initialization_options` for one startup (then remove it — leaving it `true` re-indexes from scratch every launch). As a fallback, wipe `~/Library/Application Support/intelephense/` (macOS) while Zed isn't running.

## More aggressive (with trade-offs)

If `_ide_helper_models.php` and `.phpstorm.meta.php` still cluttering goto bothers you more than the Intelephense fidelity they enable, add them to the exclude array:

```json
"exclude": [
  "**/stubs/**",
  "**/_ide_helper*.php",
  "**/.phpstorm.meta.php"
]
```

What you trade:

| Excluded | Intelephense loses | `laravel-lsp` covers |
|---|---|---|
| `_ide_helper*.php` | Eloquent dynamic attributes/methods (`$user->name`, `User::find()`), plus extra methods added to facades by third-party packages (e.g. Scout, Telescope, Spatie permissions) | Eloquent completion from the actual DB schema (more accurate than ide-helper docblocks). **Core framework facade resolution (`Auth::user()`, `Cache::get()`, `Route::get()`, etc.) keeps working without ide-helper** — Intelephense reads the `@method` PHPDoc tags directly off the facade source files at `vendor/laravel/framework/src/Illuminate/Support/Facades/*.php`. |
| `.phpstorm.meta.php` | Container-binding type narrowing (`app('cache')` → `CacheManager`) | Container-binding goto via the `Binding` pattern. |

If you use packages that extend Laravel facades with their own methods (Scout adds `Searchable` methods, Telescope extends `Gate`, etc.), keep `_ide_helper*.php` in to retain completion for those package-added methods. Core Laravel facades resolve fine without it.

## Per-project

Drop the same exclude patterns into an `.intelephense.json` at your project root. Unlike the Zed `settings` block, this file is read directly by Intelephense, so it uses the standard namespaced shape:

```json
{
  "files.exclude": [
    "**/stubs/**",
    "**/_ide_helper*.php",
    "**/.phpstorm.meta.php"
  ]
}
```
