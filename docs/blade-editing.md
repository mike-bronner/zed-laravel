# 🎨 Blade Editing Support

[← Back to README](../README.md)

## Directive Autocomplete

Type `@` to see the Blade directives available in *your* project. The list is discovered live — Laravel's built-in directives are read from your installed framework, and **custom directives registered via `Blade::directive()`** (in your app or in packages) are picked up too, so a directive like `@feature` or your own `@money` shows up without us hardcoding it. A full Laravel app typically surfaces 100+; a built-in fallback set keeps completion working if the project can't be scanned.

```blade
@fo
  ↳ @foreach  Loop through collection
  ↳ @for      For loop
  ↳ @forelse  Loop with empty fallback
```

Block directives automatically include their closing tags:
```blade
@if($condition)
    █
@endif
```

## Smart Bracket Expansion

Type `{` and select from snippet completions:

```blade
{
  ↳ {{ ... }}      Echo (escaped)
  ↳ {!! ... !!}    Echo (unescaped)
  ↳ {{-- ... --}}  Blade comment
```

## Closing Tag Navigation

Cmd+Click works on both opening AND closing tags:

```blade
<x-button>Submit</x-button>
{{-- ^^^^^^           ^^^^^^ Both navigate to component --}}

<livewire:counter></livewire:counter>
{{--      ^^^^^^^            ^^^^^^^ Both navigate to Livewire class --}}
```

> **Note:** Blade syntax highlighting is provided by the separate [**Laravel Blade**](https://github.com/bajrangCoder/zed-laravel-blade) Zed extension. Install it alongside this extension for full Blade support. For enhanced directive highlighting — including correct coloring of custom directives that tree-sitter doesn't recognize — enable semantic tokens in your settings. See the [Configuration](../README.md#️-configuration) section in the README.
