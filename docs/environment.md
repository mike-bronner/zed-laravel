# 🌱 Environment files (`.env`)

[← Back to README](../README.md)

Open a `.env` in Zed and every single line lights up with a warning:

> `APP_NAME appears unused. Verify use (or export if used externally)`

Nothing is actually wrong with your file, and it isn't this extension. The message is shellcheck's [SC2034](https://www.shellcheck.net/wiki/SC2034), and it appears because **Zed classifies `.env` files as the _Shell Script_ language**, then runs its bundled shell language server, which calls shellcheck. SC2034 flags any variable that's assigned but never *referenced in the same file* — and a `.env` is nothing but assignments that Laravel reads at runtime through `config()` / `env()`, never inside the file itself. So shellcheck flags **every line**. It's shell-script linting applied to a data file.

This is Zed's own built-in behavior — there's no extension of yours in the loop and nothing to uninstall. The fix is to tell Zed that `.env` files aren't shell scripts.

## The fix

Add a `file_types` mapping to your Zed `settings.json` that points `.env` files at a non-shell language:

```json
{
  "file_types": {
    "Ini": [".env*"]
  }
}
```

A `.env` is `KEY=value` with `#` comments — structurally INI — so the **Ini** language highlights it cleanly and, crucially, doesn't run shellcheck. If you don't already have it, install the Ini extension (Zed → `zed: extensions`, search "INI"); it's tiny.

Reload (`Cmd+Shift+P → zed: reload extensions`, or just reopen the file) and the warnings are gone.

### No extra extension

If you'd rather not install anything, map `.env` files to the built-in **Plain Text** language instead:

```json
{
  "file_types": {
    "Plain Text": [".env*"]
  }
}
```

The warnings disappear, but you lose syntax highlighting — `.env` renders monochrome.

## Per-project

To scope this to a single project instead of all of Zed, put the same block in `.zed/settings.json` at the project root rather than your global settings.

## Why a settings change is the only fix

It's reasonable to expect this extension to just handle it. It can't — and the reason is how Zed resolves a file's language. It uses **precedence tiers**, highest wins:

| Tier | Comes from | Example |
|---|---|---|
| `UserConfigured` | a `file_types` setting | your `settings.json`, **or Zed's own defaults** |
| `PathOrContent` | a language's `path_suffixes` | an extension claiming `*.blade.php` |

Zed's **default settings** ship `"file_types": { "Shell Script": [".env.*"] }` — a `UserConfigured`-tier claim on every `.env.*` variant. Nothing an extension declares can outrank it: extensions can only register `path_suffixes` (the lower `PathOrContent` tier), and the Zed extension manifest has no field for shipping `file_types` defaults at all. The only thing that sits in the same tier and can override Zed's default is **your own `file_types`** — user settings beat default settings. That's why this is one line in *your* settings and not something the extension can do for you.

> The `.env*` glob covers `.env` plus every variant (`.env.local`, `.env.production`, `.env.example`) in one pattern. For a stricter match that won't also catch unrelated files like `.envrc` or `.environment`, use `[".env", ".env.*"]` instead.
