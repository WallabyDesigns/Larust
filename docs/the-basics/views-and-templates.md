---
title: Views & Templates
parent: The Basics
nav_order: 3
---

# Views & Templates
{: .no_toc }

1. TOC
{:toc}

## `.blade.xr`: Blade-shaped, compiled, not interpreted

Larust's template language uses the same directive names and the same
`@directive` syntax as Laravel's Blade, deliberately - but it's a
from-scratch parser (`larust-view`), not a port of Blade's own engine,
and it compiles a `.blade.xr` file **at build time** into a real Rust
function that builds a `String`. There is no template *interpreter*
running per request, and no PHP anywhere underneath it.

```rust
Ok(view!("posts.show", { title: post.title, author_name, comments }))
```

`view!(name, { field, field2: expr, ... })` is a function-like macro:
`name` resolves to `resources/views/<name-with-dots-as-slashes>.blade.xr`
(`"posts.show"` → `resources/views/posts/show.blade.xr`), and the object
literal binds every variable that template's {% raw %}`{{ }}`{% endraw %}/`@if`/`@foreach`
expressions reference. A template referencing a field you didn't pass, or
an expression that doesn't type-check, is a `cargo build` error - not a
blank spot in the rendered page.

## A real page, end to end

```
@extends('layouts.app')

@section('content')
    @globals
        title = "Create Post | Larust"
        canonical = "/posts/create"
    @endglobals

    <main class="page-narrow">
        <h1>Put the idea on the page.</h1>
        <wire:post-form />
    </main>
@endsection
```

- `@extends('layouts.app')` / `@section('content')...@endsection` -
  identical to Blade: this page's content is spliced into
  `layouts/app.blade.xr`'s own `@yield('content')`.
- `@globals ... @endglobals` - sets page-specific overrides for
  layout-level placeholders (see [Globals](#globals-and-page-level-overrides)
  below).
- `<wire:post-form />` - mounts a reactive component. See [Reactive
  Components](../../digging-deeper/reactive-components).

## Every directive

| Directive | What it does |
|---|---|
| `@extends('layout.name')` | This template's content fills the named layout's `@yield`s |
| `@section('name') ... @endsection` | Defines a named block for the layout to `@yield` |
| `@yield('name')` | In a layout: renders the child template's matching `@section` |
| `@if(cond) ... @elseif(cond) ... @else ... @endif` | Real Rust boolean expressions |
| `@foreach(expr as item) ... @endforeach` | Iterates any `IntoIterator` expression |
| `@csrf` | Expands to a hidden `_csrf_token` input - see [Middleware, Sessions & CSRF](../../the-basics/middleware-sessions-and-csrf) |
| `@push('name') ... @endpush` | Appends content to a named stack |
| `@stack('name')` | Renders everything pushed to that stack, from anywhere - including across a `<wire:...>` mount boundary |
| `@global(name, fallback)` | Reads a page-level override, or `fallback` if none was set |
| `@globals ... @endglobals` | Sets one or more page-level overrides (`persist name = value` keeps a value across `@wire` re-renders) |
| `@resource('name', { prop: expr }) ... @endresource` | Static component inclusion with props and a slot - see below |
| `<resource:name attr=".." :attr2="..">...</resource:name>` | The same thing, tag-flavored syntax |
| `@wire('name', { prop: expr })` / `<wire:name attr=".." />` | Reactive (Livewire-style) component - see [Reactive Components](../../digging-deeper/reactive-components) |
| `@live(channel_expr) ... @endlive` | Server-pushed real-time updates - see [Realtime & Broadcasting](../../digging-deeper/realtime-and-broadcasting) |
| `@loadonce ... @endloadonce` | Compile-time sugar for `wire:ignore` - colocate a component's own static assets |
| `@js(expr)` | Safely-escaped JSON, for handing server data to client-side JS |
| `@larustscripts` | Injects the `@wire`/`@live` client runtime scripts a page actually needs - put once in your shared layout |
| `@spa` | Opts a layout into SPA-style navigation - see [SPA Mode](../../digging-deeper/realtime-and-broadcasting#spa-mode) |

{% raw %}
## Interpolation: `{{ }}` and `{!! !!}`

```
<p>{{ post.title }}</p>          <!-- HTML-escaped -->
<div>{!! post.rendered_html !!}</div>   <!-- raw, unescaped -->
```

Everything inside `{{ }}`/`{!! !!}` is parsed as a **real Rust
expression** (`syn::parse_str::<syn::Expr>`) - not a restricted template
sub-language. Property access, method calls, comparisons, arithmetic,
string formatting: whatever's a valid Rust expression is valid here,
checked by `rustc` exactly as if you'd written it by hand. This includes
Larust's Rust-flavored conditional-value form (Blade has no equivalent):

```
<span>{{ if is_authenticated { "Sign out" } else { "Sign in" } }}</span>
```

`{{ }}` HTML-escapes the result automatically; `{!! !!}` doesn't - use it
only for content you've already sanitized (e.g. through
`larust_support::sanitize_rich_text`), the same caution Blade's own
`{!! !!}` demands.
{% endraw %}

{: .warning }
This full-expression support is a property of `view!` itself - the
templates you write by hand. **`xr convert`'s automatic PHP→Rust
translator is intentionally more restricted**: it only translates a safe
subset it can verify won't silently produce the wrong Rust (property
chains, literals, comparisons, ternaries, `empty()`/`isset()`). Anything
outside that subset is flagged and left for you, never guessed at. See
[Converting a Laravel App](../../converting-a-laravel-app).

## Layouts and sections

Standard Blade inheritance: a layout declares `@yield('content')`
wherever a child template's own content should land; a page
`@extends('layouts.app')` and fills that spot with `@section('content')
... @endsection`. Nest as deep as you need - a page can extend a layout
that extends another.

## Globals and page-level overrides

A layout's `<title>`, canonical URL, and similar single-value,
page-overridable spots use `@global(name, fallback)` in the layout and
`@globals ... @endglobals` in the page:

```
<!-- layouts/app.blade.xr -->
<title>@global(title, "Larust")</title>
<html data-theme="@global(theme, "dark")">
```

```
<!-- posts/create.blade.xr -->
@globals
    title = "Create Post | Larust"
@endglobals
```

`persist name = value` (seen in a real layout's own `@globals` block) is
the one variant worth calling out: it keeps a value set once even across
a `@wire` component's own later re-renders, where an ordinary global
would otherwise reset.

## `@push`/`@stack`

```
<!-- layouts/app.blade.xr's <head> -->
@stack('head')
```

{% raw %}```
<!-- a component that needs to contribute to <head> -->
@push('head')
    <title>{{ post.title }}</title>
@endpush
```{% endraw %}

Both work the way Blade's do, plus one thing Blade never needed: a
`@push` from inside a `<wire:...>`-mounted component (rendered as a
genuinely separate `view!` call, with no shared template tree) still
reaches a `@stack` in the surrounding page - both on the page's initial
render and on a later live re-render of just that component, patched into
the already-rendered `<head>` over the wire. See
[`docs/ARCHITECTURE.md`](https://github.com/Costigan-Stephen/Larust/blob/main/docs/ARCHITECTURE.md)
if you want the mechanism behind that.

## Static component inclusion: `@resource(...)`

Laravel's `@component`/`@endcomponent` equivalent - non-reactive,
compile-time inclusion with props and a slot:

```
@resource('components.panel', { heading: "Account" })
    <p>Slot content, rendered in the caller's own scope.</p>
@endresource
```

or the tag-flavored spelling (identical AST, purely a readability
choice for a component with a substantial slot):

```
<resource:components.panel heading="Account">
    <p>Slot content.</p>
</resource:components.panel>
```

Props become real `let` bindings inside the included template - there's
no serialization boundary to cross, unlike `@wire(...)`'s components (see
[Reactive Components](../../digging-deeper/reactive-components)), which are
session-state-backed and can be updated live without a full page reload.

## Next

[Middleware, Sessions & CSRF](../../the-basics/middleware-sessions-and-csrf) covers what
`@csrf` and {% raw %}`{{ csrf_token }}`{% endraw %} above are actually doing.
