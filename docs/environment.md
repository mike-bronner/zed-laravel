# 🌱 Environment files (`.env`)

[← Back to README](../README.md)

Open a `.env` in Zed and every single line lights up with a warning:

> `APP_NAME appears unused. Verify use (or export if used externally)`

Nothing is wrong with your file, and it isn't this extension. The message is shellcheck's [SC2034](https://www.shellcheck.net/wiki/SC2034). Zed classifies `.env` files as the **Shell Script** language and runs its bundled shell language server, which calls shellcheck. SC2034 flags any variable that's assigned but never *referenced in the same file* — and a `.env` is nothing but assignments that Laravel reads at runtime through `config()` / `env()`, never inside the file itself. So shellcheck flags **every line**. It's shell-script linting applied to a data file.

This is Zed's own built-in behavior — there's no extension of yours in the loop and nothing to uninstall. There are two ways to quiet it, and they trade off against each other:

| | **Approach 1** — silence the rule | **Approach 2** — reclassify `.env` |
|---|---|---|
| `.env` keeps bash highlighting | ✅ yes (stays a shell file) | ❌ no (becomes Ini / Plain Text) |
| Your real `.sh` scripts | ⚠️ also lose SC2034 (within scope) | ✅ untouched |
| What you change | a shellcheck arg, or a `.shellcheckrc` | one `file_types` mapping |

Pick **Approach 1** if you want `.env` to keep shell highlighting and you don't rely on SC2034 in your own scripts. Pick **Approach 2** if you'd rather leave shellcheck fully intact for real scripts and only change how `.env` is treated.

## Approach 1 — silence SC2034 (keep `.env` as a shell file)

### Zed setting (no project file)

Pass `--exclude=SC2034` to shellcheck through Zed's shell language server in your `settings.json`:

```json
{
  "lsp": {
    "bash-language-server": {
      "settings": {
        "bashIde": {
          "shellcheckArguments": ["--exclude=SC2034"]
        }
      }
    }
  }
}
```

> ⚠️ **The `bashIde` wrapper is required.** Unlike some Zed servers (e.g. Intelephense, where Zed adds the namespace for you), the bash server does **not** wrap your settings — it only reads config from a `bashIde` section, so you must nest it yourself. Drop the wrapper and the setting is silently ignored. (`shellcheckArguments` takes an array; the server adds `--shell` / `--format` on its own.)

This applies to every shell file Zed lints, in every project. It takes effect without restarting — if it doesn't, the wrapper is almost always the reason.

### `.shellcheckrc` (project-scoped, editor-agnostic)

Drop a `.shellcheckrc` in your project root (or `~/.shellcheckrc` for all projects) containing:

```
disable=SC2034
```

shellcheck reads this file directly, so it works in any editor — not just Zed. Scope is the whole project tree, so real `.sh` scripts there also stop getting SC2034.

### Per-file directive (surgical)

Add a comment to the top of a specific file to scope the suppression to just that file:

```bash
# shellcheck disable=SC2034
```

The cost is a stray comment line in each `.env` you apply it to — fine for one file, tedious across `.env`, `.env.example`, etc.

## Approach 2 — reclassify `.env` away from Shell Script

Map `.env` files to a non-shell language in `settings.json`. A `.env` is `KEY=value` with `#` comments — structurally INI — so the **Ini** language highlights it cleanly and never invokes shellcheck:

```json
{
  "file_types": {
    "Ini": [".env*"]
  }
}
```

Install the Ini extension (`zed: extensions`, search "INI") if you don't already have it; it's tiny. To avoid any extra install, map to the built-in **Plain Text** instead — the warnings disappear, but `.env` renders without highlighting:

```json
{
  "file_types": {
    "Plain Text": [".env*"]
  }
}
```

The `.env*` glob covers `.env` plus every variant (`.env.local`, `.env.production`, `.env.example`). For a stricter match that won't also catch unrelated files like `.envrc` or `.environment`, use `[".env", ".env.*"]`.

## Per-project

Any of the `settings.json` blocks above also work in `.zed/settings.json` at the project root, scoping the change to one project instead of all of Zed. (The `.shellcheckrc` approach is already project-scoped by nature.)

## Why this extension can't do it for you

Every lever here lives in *your* settings or *your* project — none of it is something a Zed extension can ship:

- **Approach 1** lives in your Zed `settings.json` or a `.shellcheckrc` in your repo. The extension's WASM sandbox can't write either, and silently dropping files into your project would be worse than the warning.
- **Approach 2** depends on Zed's language-detection precedence. Zed resolves a file's language by tier — a `file_types` setting (`UserConfigured`) outranks a language's `path_suffixes` (`PathOrContent`). Zed's *default settings* already claim `.env.*` for Shell Script at the `UserConfigured` tier, and the Zed extension manifest has **no `file_types` field** — so no extension can register a competing claim. Only your own `file_types` (user settings beat default settings) can override it. That's why this is one line in *your* config, not something the extension does automatically.
