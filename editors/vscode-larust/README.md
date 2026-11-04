# Larust Blade

Syntax highlighting and indentation for Larust's `.blade.xr` templates -
a real language of its own, not Laravel's Blade (though inspired by
it). Built on top of VS Code's bundled HTML grammar, so ordinary markup
keeps rendering exactly as it already does; this extension only adds
rules for what's actually different:

- **Directives**: structural, block-pair ones - `@if`/`@elseif`/`@else`/
  `@endif`, `@foreach`/`@endforeach`, `@section`/`@endsection`,
  `@push`/`@endpush`, `@globals`/`@endglobals` (including
  `persist key = expr` inside), `@loadonce`/`@endloadonce`,
  `@resource`/`@endresource`, `@live`/`@endlive`, `@code`/`@endcode`,
  `@can`/`@endcan`, `@role`/`@endrole`, `@spa`/`@endspa` - get a
  distinct `keyword.control` scope from simple, non-block ones -
  `@extends`, `@yield`, `@csrf`, `@global`, `@wire`, `@larustscripts`,
  `@stack`, `@vitex`, `@js` - which get `support.function` instead,
  mirroring the real split `onecentlin.laravel-blade`'s own grammar
  makes between its block keywords and its function-like directive
  catch-all.
- **Interpolation**: `{{ expr }}` (escaped) and `{!! expr !!}` (raw) -
  the expression inside is real Rust, highlighted as such.
- **Comments**: `{{-- ... --}}`.
- **Tags**: `<Image ... />`, `<wire:name ... />`,
  `<resource:name ...>...</resource:name>`.
- **Attribute values**: always get a real, distinct string scope -
  guaranteed independent of whatever the base HTML grammar does for an
  *ordinary* tag's own attributes, so they never visually collapse into
  the same color as plain element text content.
- **Dynamic attributes**: `:attr="rust_expr"` on any tag - the quoted
  value is highlighted as Rust, not a literal string.

## Embedded Rust highlighting

Every Rust expression inside `{{ }}`/`{!! !!}`/directive arguments/
`:attr=` values is highlighted using VS Code's own `source.rust` grammar
when a Rust language extension is active (e.g. the official `rust-lang.
rust-analyzer` extension - note that extension itself focuses on LSP
features and may not contribute a grammar; a dedicated syntax extension
like `dustypomerleau.rust-syntax` does). If none is active, this
extension falls back to its own basic highlighting (strings, numbers, a
handful of common keywords/types) so these regions never go completely
unstyled - confirmed directly against the real `vscode-textmate`/
`vscode-oniguruma` engine, not assumed: a rule whose only pattern is an
unresolvable grammar `include` renders nothing at all, not just "no
Rust-specific coloring," so the fallback exists specifically to avoid
that failure mode.

**Single-quoted strings never go through `source.rust` at all.** This
templating language's own directive-argument strings are conventionally
single-quoted (`@extends('layouts.app')`, `@wire('search-box')`), but
real Rust has no single-quoted *string* syntax - a single quote there
means a char literal (`'a'`) or a lifetime (`'static`, no closing
quote). Reported directly, via VS Code's own scope inspector:
`source.rust` was misinterpreting `'layouts.app'` as a lifetime
(`'layouts`, stopping at the period), leaving `.app'` completely
unscoped - a visibly broken, half-colored result. Single-quoted strings
are now claimed as a real string scope *before* `source.rust` ever sees
them, everywhere both can appear.

## What this doesn't do

No formatter/auto-reformatter - this only provides editor-level
indentation behavior (auto-indent after an opening directive/tag, dedent
on its closer, via `language-configuration.json`), never rewrites
existing file content. See `docs/MACROS.md` for this templating
language's own grammar reference.

**Known limitation**: `{{ expr }}` written *inside* an ordinary HTML
attribute's own quoted value (e.g. `<div class="{{ expr }}">`, not one
of this templating language's own `:dynamic="expr"` attributes) doesn't
get interpolation highlighting - the base HTML grammar's own attribute-
value matching claims the whole quoted string first. Not seen in any of
this repo's own real `.blade.xr` templates (the actually-used pattern,
`:attr="expr"`, highlights correctly); flagged here rather than left
silently unaddressed.

## Installing locally (not yet published to the Marketplace)

```bash
cd editors/vscode-larust
npx @vscode/vsce package
code --install-extension vscode-larust-0.1.0.vsix
```

Or, for active development: open this folder in VS Code and press `F5`
to launch an Extension Development Host with it loaded.
