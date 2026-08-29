# Proc-macros

All three of Larust's proc-macros live in `crates/larust-macros/src/`, one
module each (`form_request.rs`, `view.rs`, `model.rs`), registered in
`lib.rs`. `#[controller]` — mentioned as an option in `rust-laravel.md` —
was deliberately **not** built; every milestone that could have used it
(routing, request handling, route model binding) turned out not to need it,
and building it speculatively would have meant guessing at its shape ahead
of real pressure to define one. If a real need shows up later, add it then.

Every macro in this file follows the same rule: **generated code paths are
always `::larust_support::...`, never a more specific internal crate.** See
[ARCHITECTURE.md](ARCHITECTURE.md) for why.

## `#[derive(FormRequest)]`

Source: `crates/larust-macros/src/form_request.rs`.

Parses `#[validate(...)]` attributes on each field (`required`, `email`,
`string` — a recognized no-op, since raw form values are already strings —
`length(min = N, max = N)`, and `confirmed` — checks the field against a
`{field}_confirmation` field read directly from the same raw form data,
Laravel's own convention (`password`/`password_confirmation`); `unique(...)`
is a **compile error** pointing at "requires M4" — er, at database access,
since it needs a live query).
Multiple `#[validate(...)]` attributes on the same field are merged (not
"last one wins" — an earlier bug let a second attribute silently override
the first), and duplicate rules within the merged set are deduplicated.

Generates:

- An `impl axum::extract::FromRequest<S> for YourStruct` (fully qualified as
  `::larust_support::axum::extract::FromRequest`), whose body:
  1. Reads the body via `axum::body::to_bytes` with a **hard 2 MiB cap**
     (this cap exists because the first version used `usize::MAX` — an
     unbounded-memory DoS vector caught in review; axum's own built-in
     extractors default to 2 MiB via `DefaultBodyLimit`, so this matches
     that rather than being arbitrary).
  2. Parses it as `application/x-www-form-urlencoded` via the
     `form_urlencoded` crate (re-exported through
     `larust_support::validation::form_urlencoded`).
  3. Runs every field's rules against the raw string value, collecting
     *all* failures (not stopping at the first) into a
     `larust_validation::ValidationErrors`.
  4. Returns `Err(errors)` (→ HTTP 422, Laravel-shaped JSON body) if
     anything failed, before the handler ever runs; otherwise constructs
     `Self` from the raw values.
- `impl YourStruct { pub fn validated(self) -> Self { self } }` — trivial
  today (the struct's fields already *are* the validated data by
  construction), kept for call-site parity with Laravel's
  `$request->validated()`.

The generated `impl` is annotated `#[::larust_support::axum::async_trait]`.
This isn't stylistic — see [GOTCHAS.md](GOTCHAS.md) for why a native
`async fn` in the impl fails with a confusing `E0195` lifetime error instead.

## `view!("template.name", { context... })`

Source: `crates/larust-macros/src/view.rs`, parser in `crates/larust-view/`.

1. **Parses the macro invocation itself** via a hand-written `syn::Parse`
   impl (`ViewInput`): a string literal, then a brace-delimited list of
   `ident` or `ident: expr` entries (the bare form is sugar for
   `ident: ident`, matching Rust's own struct-init shorthand).
2. **Resolves the template file**: `"posts.index"` → dots become path
   separators → `{CARGO_MANIFEST_DIR}/resources/views/posts/index.blade.xr`.
   `CARGO_MANIFEST_DIR` is read at macro-expansion time, so this always
   resolves relative to whichever crate is actually calling `view!` (the
   app), not `larust-macros`'s own directory.
3. **Reads and parses** the file via `larust_view::parse` (pure text →
   `Node` AST — see below), then **resolves layout inheritance**
   (`@extends`/`@section`/`@yield`) via `larust_view::resolve`, which
   recursively loads parent templates through the same `load_template`
   function, threading a `HashSet<String>` of already-visited template
   names to detect `@extends` cycles (a template extending itself, directly
   or through a longer chain, is a compile error — not a stack overflow;
   see GOTCHAS.md for how that was originally not the case).
4. **Codegens** the resolved `Node` tree into Rust statements that append
   into a `String` buffer (`__larust_view_out`):
   - `Node::Text(s)` → `__larust_view_out.push_str(#s);`
   - `Node::Interpolate { expr, escape }` → `expr` (a **raw string** in the
     `Node`, since `larust-view` has no `syn` dependency) is parsed via
     `syn::parse_str::<syn::Expr>` *here*, at codegen time, and spliced in
     directly. This is what makes `{{ user.name }}` a genuine, type-checked
     Rust expression rather than a custom template language — an
     undeclared context variable is a real `rustc` E0425 error pointing at
     the `view!` call site, not a runtime template error. `escape: true`
     wraps the result in `larust_support::view::escape` (a 5-character
     HTML-entity substitution); `escape: false` (`{!! !!}`) doesn't.
   - `Node::If`/`Node::Foreach` → real `if`/`for` statements, with `cond`/
     `iter`/`binding` similarly parsed via `syn::parse_str`.
   - `Node::Csrf` (the `@csrf` directive) → a literal
     `<input type="hidden" name="_csrf_token" value="...">`, reading a bare
     `csrf_token` identifier that must be present in the `view!` context —
     enforced the same way any other undeclared variable is (a compile
     error, not a runtime lookup). The field name literal here
     (`"_csrf_token"`) must match `larust_http::csrf::FIELD_NAME` — they're
     duplicated across crates rather than shared, since `larust-macros`
     doesn't otherwise depend on `larust-http`.
5. **Emits `include_str!(#path)` for every template file touched**
   (child + every ancestor layout) as dead code inside the generated block.
   This is a deliberate trick, not an accident: a proc-macro reading a file
   via `std::fs::read_to_string` during expansion does **not** register
   that file as a build dependency on its own, so editing a `.blade.xr`
   file wouldn't trigger a rebuild without this. `include_str!` is a
   compiler builtin that *does* register the dependency correctly.

`larust-view`'s parser (`crates/larust-view/src/parser.rs`) is a
hand-rolled recursive-descent scanner over the raw template string — no
external parser-combinator dependency. It only treats `@word` as a directive
when `word` matches a fixed keyword list *and* is followed by a
non-identifier character, so literal `@` in HTML content (email addresses,
etc.) is left alone. `@if(...)`/`@foreach(...)`'s argument extraction is a
balanced-paren, string-literal-aware scanner (so `@if(name == "a)b")`
doesn't get confused by the `)` inside the string).

`@elseif(cond)` chains any number of times before an optional trailing
`@else`. It's parsed by desugaring: `@if(a) X @elseif(b) Y @else Z @endif`
builds *exactly* the same `Node::If` tree as hand-nesting
`@if(a) X @else @if(b) Y @else Z @endif @endif` would (the `@elseif`'s
condition becomes a nested `Node::If` inside the outer one's
`else_branch`) — so `codegen_node`'s `Node::If` arm needed zero changes to
support it; it just recurses into `else_branch` regardless of what's in
there, same as it always has.

**`@code ... @endcode`** executes trusted Rust statements in the generated
view function and renders nothing by itself. It is intended for small local
derivations used by later markup, for example:

```blade
@code
let heading = if is_member { "Welcome back" } else { "Welcome" };
@endcode
<h1>{{ heading }}</h1>
```

The block is parsed as Rust tokens at compile time, so it is type-checked in
the same scope as the `view!` context. It is not a PHP interpreter and must
not be fed Laravel `@php` contents unchanged. Keep I/O, database access,
authorization, and mutation in handlers or `WireComponent` methods; use
`@code` only for pure, presentation-local values.

**Conditional values without `@if`/`@else` at all**: `{{ }}`/`{!! !!}`
interpolations parse their contents as an arbitrary `syn::Expr`, and
Rust's `if`/`else` is itself an expression — so a Laravel ternary
(`{{ $x ? "a" : "b" }}`) has a direct equivalent that already works with
no framework feature needed:
```
<a class="{{ if nav_active == "home" { "nav-link is-active" } else { "nav-link" } }}">
```
This is usually the better fit than `@if`/`@else`/`@elseif` for a single
conditional *value* (a CSS class, an attribute) — reach for the
directives when the branches produce structurally different markup, not
just a different string.

**`@push('name') ... @endpush`** / **`@stack('name')`** — page-specific
contributions to a spot the layout controls, e.g. a per-page `<title>`
override, canonical URL, or an extra `<script>` tag a specific page needs
without every other page paying for it. The key difference from
`@section`/`@yield`: a section is a single body, last write wins; a stack
*accumulates* — any number of `@push`es to the same name all show up at
the matching `@stack`, concatenated in the order they were written
(across the *entire* `@extends` chain, not just the immediate child —
both a layout-level default push and a page-level one to the same stack
name both land in the output). Resolved as a genuinely separate pass in
`larust_view::resolve` from `@section`/`@yield`: every `@push` in the
whole chain is collected *before* any `@stack` is substituted, precisely
so a `@stack` in the base-most layout can see contributions from every
level, not just whichever level's section/yield substitution happened to
reach it first. A `@push` that never reaches a `@stack` (or a `@stack`
with nothing pushed to it) silently renders nothing at its own position —
same as an unresolved `@yield` — not inline where it was written.

**`@push` inside `@foreach` is a compile error, not supported.**
`@push`/`@stack` resolve once, statically, at macro-expansion time —
unlike Laravel's own imperative, output-buffered Blade compiler, there's
no per-iteration runtime step here that could make a push inside a loop
contribute once per item. Rather than silently rendering the pushed
content exactly once (or, if it references the loop variable at all,
failing to compile with a confusing "cannot find value" error pointing at
generated code far from the actual template), `resolve()` detects this
and rejects it with a clear message naming both directives. Build the
string yourself inside the loop instead if you need per-item output.

**`@global(name)` / `@global(name, fallback)`** / **`@globals ... @endglobals`**
— a lighter-weight alternative to `@push`/`@stack` for a *single value*
override (a page `<title>`, a canonical URL) rather than a block of
markup. A layout places a named placeholder:
```
<title>@global(title, "Larust")</title>
```
and any page in the `@extends` chain sets it with one or more
`name = expr` assignment lines:
```
@globals
title = post.title
canonical = "https://example.com/my-page"
@endglobals
```
`expr` is a raw string in the AST, parsed as a real `syn::Expr` at codegen
time — same convention as `{{ }}` — so it can be **any Rust expression**,
not just a string literal: a bare context variable (`title = post_title`),
a field/method access (`title = post.title`), a ternary-style if-expression
(`title = if is_admin { "Admin" } else { "User" }`), anything. It's spliced
into the *same* generated function as the rest of the resolved template, so
it sees whatever context variables the page's own `view!(...)` call declared
(`post.title`, a computed value, etc.), exactly like a `{{ }}`
interpolation would. Always HTML-escaped — there's no raw/unescaped form.

`@global(name)`'s argument is a **bare identifier, not a quoted string** —
unlike `@section`/`@yield`/`@push`/`@stack`'s quoting convention. This is
deliberate: it lets the placeholder and its setter use the exact same
literal token (`title` in both `@global(title)` and `title = "..."`), with
no quote-mark mismatch between the two. The optional second argument
(`@global(name, fallback)`) is a fallback expression, used only when no
`@globals` block anywhere in the chain sets `name` — an unset global with
no fallback renders empty, same convention as an unset `@stack`.

**Resolution shape mirrors `@push`/`@stack`, not `@section`/`@yield` — and
this is a real correctness choice, not an arbitrary one.** `@section`/
`@yield` resolve *eagerly, per level*: a `@yield` with no matching
`@section` at the level currently being processed is replaced with
nothing right then, permanently. That's invisible in a 2-level chain
(`page` extends `layouts.app`, done), but in a 3+-level chain, if the
*middle* layout doesn't happen to set a `@section` for a name the
outermost layout `@yield`s, that yield is blanked before a leaf page's own
`@section` of the same name ever gets a chance — even though the middle
layout never touched that name at all. `@global`/`@globals` needs to
survive exactly that scenario (a page overriding a grandparent layout's
title through an indifferent middle layout), so it uses `@push`/`@stack`'s
two-pass shape instead: `collect_globals` walks the *entire* chain first
(child-most level collected first), then `substitute_globals` runs exactly
once, at the very end. Collecting child-first and merging with
`entry(...).or_insert(...)` (only if absent) is also what gives a page
precedence over an ancestor layout that sets the same name — the more
specific (child-ward) value always wins. A side effect of following this
shape rather than section/yield's: `@global`/`@globals` work correctly
even in a template with **no `@extends` at all** (a single standalone
template can set and read its own global), since `collect_globals` runs
unconditionally, not gated behind an `@extends` check.

Same `@foreach` restriction as `@push`: a `@globals` block inside a
`@foreach` is a compile error, for the same reason — resolved once,
statically, not once per loop iteration, so a per-item value has no
coherent meaning.

**A `@globals` block inside `@if`/`@elseif`/`@else` is also a compile
error** — a different failure mode than `@foreach`'s, but the same root
cause. `collect_globals` walks both branches of every `@if` unconditionally
(it has to — resolution happens once, at compile time, with no notion of
which branch a runtime condition would actually select), so allowing a
`@globals` inside a branch would mean whichever branch happens to be
collected *last* always wins, silently, regardless of the condition's
actual runtime value. Set the global unconditionally, or — since `expr`
already accepts any Rust expression — compute the value conditionally
inline instead: `title = if is_admin { "Admin" } else { "User" }`.

## `@wire('name')` / `@wire('name', { prop: expr, ... })`

Source: `crates/larust-view/src/{ast,parser}.rs` (parsing), `crates/
larust-macros/src/view.rs` (codegen). Mounts a server-state-backed reactive
component (`larust-live`'s `WireComponent`) at this position in the
template — Larust's Livewire equivalent. Full design rationale lives in
`docs/ARCHITECTURE.md`'s "Reactive components" section; this entry covers
just the macro mechanics.

```blade
@wire('search-box')
@wire('search-box', { query: "", limit: 10 })
```

The component name is a quoted string (`parse_quoted_string`, the same
scanner `@extends`/`@yield`/`@stack` use). The optional props object is a
brace-delimited, comma-separated `key: expr` list, parsed by
`parse_prop_entries` — a new scanner, since no existing helper balances
`{ }` (`scan_to_matching_close_paren` only balances `(` `)`). It tracks one
*combined* nesting depth over `(`/`{`/`[` so a prop's own expression can
freely contain nested calls, arrays, or struct literals (`{ items:
vec![1, 2], meta: Foo { a: 1 } }`) without prematurely closing the outer
object. Each entry is stored as a raw `(String, String)` pair in `Node::Wire
{ name, props }` — same "raw strings only" convention as `Node::Globals`'
`(name, expr)` pairs; real `syn::Expr` parsing happens only at codegen
time, in `larust-macros`.

**No `resolve.rs` changes at all** — `Node::Wire` isn't a compile-time
collection pass the way `@push`/`@global` are; it passes through every
existing tree-walk's catch-all arm unchanged. This also means, unlike
`@push`/`@globals`, **`@wire(...)` is allowed inside `@foreach`/`@if`** —
mounting a component is an ordinary runtime statement (real Rust `for`/
`if`), not a static, resolved-once collection, so it composes naturally:
`@foreach(post in posts) @wire('post-widget', { id: post.id }) @endforeach`
mounts one independent, interactive instance per iteration correctly, with
no special-casing needed anywhere.

Codegen (`view.rs`'s `Node::Wire` arm) parses each prop's raw expression
via `syn::parse_str::<syn::Expr>` (same pattern `Node::Interpolate` already
uses), JSON-encodes it (`.expect()`s on serialization failure — props are
simple, author-controlled values, never end-user JSON, so a failure here is
a programmer bug, not a runtime-data problem, matching this codebase's
existing tolerance for near-certain-infallible calls), and emits a call to
`larust_support::wire::mount(session, name, props).await?`.

This is the **first directive whose codegen needs `.await`/`?`** — a
template using `@wire(...)` gains an implicit contract on its `view!(...)`
call site: the context must bind `session: &Session`, and the call site
must be inside an `async fn` returning a `Result` (`AppError: Into<E>`).
Exactly `@csrf`'s existing `csrf_token`-must-be-in-context contract, just
one binding richer. `expand()` checks for this **eagerly** — if the
resolved tree contains any `Node::Wire` (`contains_wire`, walking into
`@if`/`@foreach`/`@section`/`@push` bodies) but `session` isn't a context
entry, it's a `syn::Error` at the `view!` call site (same eager-error
pattern `resolve.rs` already uses for `@push`/`@globals` misuse), instead
of a confusing "cannot find value `session`" or "`.await` only allowed
inside `async`" error pointing at generated code far from the actual
template.

Rendered output is always `<div data-wire-id="{opaque-id}">{component's own
render() output}</div>` — no `data-wire-name` in the markup; the server
resolves id → component name from session storage, so the client only ever
needs to address `/__larust_wire/{id}`.

### Tag syntax: `<wire:name attr="literal" :attr2="expr" />`

An alternate, HTML-tag-flavored spelling of `@wire('name', { ... })` —
Livewire's own `<livewire:counter />` convention, and the same treatment
`@resource(...)` got (see its own tag-syntax subsection below) for the
same reason: it's not a separate feature, both forms parse to the
identical `Node::Wire`, so a template is free to mix both.

```blade
<wire:search-box />
<wire:post-form :post_id="post.id" />
```

**Always self-closing.** Unlike `<resource:name>`, `@wire(...)` has never
had a body/slot concept at all — a mounted component renders entirely from
its own template, so there's nothing a tag-syntax block form would even
mean. A stray `<wire:name>` with no `/>` is a parse error naming the
problem directly ("must be self-closing"), not silently treated as an
empty body.

Attributes follow the exact same convention `<resource:...>` uses (and
share its scanner, `parse_tag_attrs`): plain `attr="value"` is a literal
string prop, a leading `:` marks the value as a raw Rust expression —
`:post_id="post.id"` is `{ post_id: post.id }`.

Demo example: `demo/resources/views/posts/create.blade.xr` mounts
`<wire:post-form />` (previously `@wire('post-form')`).

## `@larustscripts`

Source: same two files as `@wire(...)` above. Livewire's `@livewireScripts`
equivalent — written once, in a shared layout (conventionally right before
`</body>`), instead of every page that mounts a `@wire(...)` component
needing its own `<script src="/__larust_wire/runtime.js">` tag.

```blade
@yield('content')
@larustscripts
</body>
```

Parses as `Node::LarustScripts` (no arguments, no body — same shape as
`Node::Csrf`/`Node::Stack`). Passes through `resolve.rs` unchanged, same as
`Node::Wire`. The interesting part is entirely in codegen: `expand()`
already computes `contains_wire(&resolved)` once, for `@wire(...)`'s own
"`session` must be in context" check (see above) — `codegen_nodes`/
`codegen_node` now take that same boolean as an `emit_wire_scripts`
parameter, threaded through every recursive call (`If`/`Foreach`/
`Section`), and `Node::LarustScripts`'s codegen arm checks it directly: if
`true`, emit the literal `<script src="/__larust_wire/runtime.js"
defer></script>` tag; if `false`, emit nothing. This is a **compile-time**
decision made once per template, not a runtime branch — a template that
has no `@wire(...)` anywhere in its resolved tree (itself, or, via
`@extends`, whatever page renders through it) gets a `@larustscripts` that
always expands to nothing, so pages with zero wire components pay zero
runtime cost and load zero extra script tags, even though they share the
exact same layout as pages that do use `@wire(...)`. Proven directly in
`crates/larust-macros/tests/view_larustscripts.rs`: one layout, two child
pages extending it (one mounting a component, one not), asserting the
script tag appears on exactly one of them.

The script path (`/__larust_wire/runtime.js`) is a literal in this codegen
arm, not a constant shared with `larust-live`'s own route registration —
same "duplicated rather than adding a cross-crate dependency just for one
string" reasoning `Node::Csrf`'s hardcoded `"_csrf_token"` field name
already established.

`@live(...)`'s own `emit_push_scripts` flag, and `@spa`'s own
`emit_spa_scripts` flag (see that directive's own section below), are
computed and checked exactly the same way, independently — a page can use
any combination of `@wire`/`@live`/`@spa` and `@larustscripts` emits
exactly the scripts it actually needs, never more.

## `@spa ... @endspa`

Source: `crates/larust-view/src/{ast,parser,resolve}.rs` (parsing +
resolution), `crates/larust-macros/src/view.rs` (codegen, and the
`@larustscripts` hook), `crates/larust-spa/assets/spa-runtime.js` (client),
`crates/larust-spa/src/lib.rs` (the `runtime_js()` handler). Hotloaded,
SPA-style page navigation: clicking a same-origin link or submitting a form
swaps in the new page's content via `fetch` + `DOMParser` instead of a full
browser reload, updating `<title>` and the URL via the History API — with
**no client-side routing framework and no new server-side rendering path
at all**. Every navigated-to page still renders through the exact same
`view!(...)` call, producing the exact same full HTML document a hard
reload would get; the client extracts what changed itself.

```blade
<body>
    @spa
    @yield('content')
    @endspa

    @larustscripts
</body>
```

**Deliberately a paired `@spa ... @endspa` block, not a bare single tag**
(unlike `@csrf`'s "just emit something at this position" shape, which the
directive's own name might suggest at a glance): `@spa` needs to *wrap* a
region — a fixed sentinel `<div id="__larust_spa_root">...</div>` — and
`resolve.rs`'s `substitute_yields` only knows how to replace a bare
`Node::Yield(name)` inline, with no "wrap this yield's own output" concept
to lean on. Inventing one would also have to guess which yield is "the"
content region, when the yield name (`content` above, by convention only)
is just whatever the app itself picked. So the layout author places the
wrapper explicitly instead, the same open-body-close shape
`@loadonce`/`@resource`/`@live`/`@can`/`@role` already use. Parses as
`Node::Spa(Vec<Node>)` — same single-field shape as `Node::LoadOnce` — and
is threaded through the same 7 `resolve.rs` functions `Node::LoadOnce`
already participates in (`collect_pushes`/`contains_push`/
`collect_globals_into`/`contains_globals`/`substitute_globals`/
`substitute_stacks`/`substitute_yields`); the last one is load-bearing —
it's what actually reaches into `@spa`'s body and replaces the
`Node::Yield("content")` sitting inside it with whatever the current page's
own `@section('content')` provided.

**No `session`/`user`/`cookies` binding required** — unlike
`@wire`/`@can`/`@role`/a `persist` `@globals` entry, `@spa` touches none of
session/permissions/cookie state at codegen time; it emits only static
markup and (via `@larustscripts`) a static `<script>` tag, the same "no
scope dependency at all" shape `Node::Vitex` already has. What *is* checked
eagerly, at `view!` expansion time, is a **count**: a resolved tree with
more than one `@spa` block is a compile error (`count_spa`'s own check in
`expand()`) — the sentinel id can't be duplicated, and there's exactly one
region to swap per page.

**`@larustscripts` picks this up as a third, independent flag**
(`emit_spa_scripts`, computed from `count_spa(&resolved) == 1` exactly
parallel to `emit_wire_scripts`/`emit_push_scripts`), emitting
`<script src="/__larust_spa/runtime.js" defer></script>` only on pages
whose resolved tree actually uses `@spa` — any combination of `@wire`/
`@live`/`@spa` on the same page gets exactly the scripts it needs, never
more, proven in `crates/larust-macros/tests/view_spa.rs` (including a
`@wire(...)` mount nested *inside* an `@spa` block, to prove neither
directive's own detection scan misses the other when nested).

**Route registration is the app's own explicit job**, same convention
`/__larust_wire/*`/`/__larust_push/*`/`/__larust_reverb/*` already use —
nothing is auto-mounted:

```rust
.get("/__larust_spa/runtime.js", larust_support::spa::runtime_js)
```

Unlike wire/push/reverb, there's no second route at all — no
`/__larust_spa/{id}` endpoint of any kind, because (per the "no new
server-side rendering path" design above) ordinary page routes already
serve everything `@spa` needs.

**Strictly opt-in — never scaffolded by default**, unlike the
`view-transition` meta tag (`crates/larust-cli/src/scaffold.rs`'s generated
layout includes that one unconditionally, since it's a zero-behavior-change
progressive enhancement). `@spa` globally intercepts every same-origin
link click and form submit once added, which is real behavior an app
author should choose deliberately — the same reasoning `@wire`/`@live`
already follow (no default mount, no default route registration either).

**Client runtime (`spa-runtime.js`) behavior, in brief** — see the file's
own header comment for the full account:
- Intercepts same-origin `<a href>` clicks and `<form>` submits (GET and
  mutating alike), skipping modifier-clicks, `target`, `download`,
  cross-origin links/actions, and anything under a `data-spa-ignore`
  attribute (placeable on the element itself or any ancestor container).
- **`multipart/form-data` forms are always a real, native, un-intercepted
  submission** — file uploads are explicitly out of scope for v1.
- Every fetch uses `redirect: "follow"`, so Laravel/Larust's own
  redirect-after-POST pattern (including "validation failed, redirected
  back with flashed errors") works with no special-casing — it's
  structurally identical to a success redirect from this script's own
  point of view, just different final HTML.
- The swap is a **whole-subtree `innerHTML` replace, not a diff-patch** —
  deliberately not `wire-runtime.js`'s own positional DOM patcher, which is
  built for one structurally-stable component subtree, not swapping
  between two unrelated pages' content.
- Any non-2xx *final* response (after redirects) or network error falls
  back to a real browser navigation/submission — "fail safe to normal
  behavior," never tries to render an error page via JS.
- `<script>` tags inside the swapped region **are re-executed on every
  swap** — plain `.innerHTML` assignment alone leaves them inert (standard,
  spec'd behavior), so each one is cloned into a freshly created `<script>`
  element to force it to run, the same technique Turbo/htmx use. A `src`
  script only ever fetches-and-runs once per unique absolute URL for the
  page's whole lifetime (tracked from the initial document onward), so a
  shared library tag repeated in every page's content region doesn't get
  re-downloaded on every navigation; inline scripts have no such identity
  to dedupe on and re-run every time they appear, matching the intent of
  page-specific init code living directly in the page's own content.
- Dispatches two `CustomEvent`s on `document` around every swap — the
  extension points for app-level JS that needs to react to a navigation:
  `larust:spa:navigating` just *before* the old content is replaced (a
  widget holding live references into the outgoing DOM — a map, a chart —
  should tear itself down here, since only listeners on the discarded
  element itself vanish for free; anything registered on `window`/
  `document` leaks otherwise), and `larust:spa:navigated` just *after* the
  new content lands, for (re)initialization.

**Stated v1 limitations, exhaustively** (matching this codebase's own
convention of stating limitations plainly rather than glossing over them):
1. No `<head>` reconciliation beyond `<title>` — a page-specific
   `@push('head')` meta/stylesheet won't update on an `@spa` navigation.
2. Whole-subtree replace, no diff-patch — anything with client-managed
   state living inside the swapped region (a rich text editor, an open
   `<details>`, scroll position, focus, unsaved input) is destroyed and
   rebuilt from scratch on every navigation. `<script>` tags themselves
   *do* re-execute (see above), but each run starts from a clean slate —
   module-level variables/timers a previous run set up are not carried
   forward, matching ordinary full-reload semantics rather than attempting
   to preserve JS state across a navigation the way Livewire's whole-page
   morph tries (and is a known source of its own breakage) to do.
3. At most one `@spa` block per page (compile error otherwise).
4. `multipart/form-data` forms are always a real, native submission.
5. A non-2xx final response on a *mutating* form falls back to a real,
   native resubmission, carrying a small, accepted risk of double-
   submitting if the original fetch attempt had already partially
   succeeded server-side before returning an error status.
6. No response-caching interaction, by construction, not as a worked-around
   hazard — `crates/larust-http/src/responsecache.rs` caches by URL alone
   with no `Vary`-by-header support, but since every request (SPA-navigated
   or not) gets the identical full-page response, there's nothing to vary
   on in the first place.
7. No prefetching, no client-side route table — every navigation is a real
   fetch at click/submit time.
8. Scroll position resets to top on a pushed navigation; left to the
   browser's own best effort on `popstate` (back/forward).
9. **No automated test coverage for the client runtime itself** — this
   codebase has no browser/JS test harness anywhere (confirmed: no
   Playwright, no headless-browser setup); `spa-runtime.js`'s actual
   interception/fetch/swap/history behavior is verified only by manual/E2E
   testing against a real browser, the same as `wire-runtime.js`/
   `push-runtime.js` are today. `view_spa.rs`'s own integration tests prove
   the server-rendered markup and script-tag emission are correct, not the
   client script's runtime behavior.
10. Strictly opt-in (see above) — never auto-mounted, never scaffolded by
    default.
11. A `<resource:...>`-included template's own `@spa` isn't reached by
    `count_spa`'s scan (it only walks a `<resource:...>` call site's own
    `slot`, not the included template's own body) — the same accepted
    boundary `contains_wire`'s own doc comment already documents for that
    class of check, not a new gap this feature introduces.
12. `src` script dedupe is a single global set, keyed on absolute URL, that
    only ever grows for the page's whole lifetime — a script tag whose
    `src` is meant to serve different content on different requests behind
    the same URL (unusual, but possible with a query-string-free dynamic
    endpoint) only ever executes on the first page that references it.

## `@loadonce ... @endloadonce`

Source: same three files as `@wire(...)`/`@larustscripts` above (`ast.rs`,
`parser.rs`, `view.rs`). Sugar for wrapping a block in
`<div wire:ignore>...</div>` — a colocated way to put static assets (a
component's own `<link rel="stylesheet">`/`<script>` tags) directly inside
a `@wire(...)`-mounted component's own template, safe against that
component's fragment re-rendering on every `wire:model`/`wire:submit` sync.

```blade
@loadonce
<link rel="stylesheet" href="/styles/trix.css">
<script src="/scripts/trix.umd.min.js"></script>
@endloadonce
```

The name is a little misleading on its own: the content inside still
renders into **every** server response for that component, including every
AJAX fragment — nothing is actually omitted server-side. What makes it
"load once" is entirely client-side: `wire:ignore` (see `@wire(...)`'s
`wire:ignore` support above) tells `wire-runtime.js`'s DOM patcher to skip
diffing that subtree at all once it's been mounted, so the wrapped
`<link>`/`<script>` tags are never touched again after the real, initial
page parse. A design where the *server* stopped emitting this block on
later renders was considered and rejected: the client's DOM patcher matches
children by position, so a block silently missing from one fragment but
present in the next would misalign every sibling after it — a real
correctness hazard, not just a missed optimization. Always-render-but-
client-ignores sidesteps that entirely.

Parses as `Node::LoadOnce(Vec<Node>)` — a block node with a body, same
shape as `Node::Section`/`Node::Push`. Unlike `Node::Wire`/
`Node::LarustScripts`, this one *does* need `resolve.rs` changes: it's
threaded through `collect_pushes`/`contains_push`/`collect_globals_into`/
`contains_globals`/`substitute_globals`/`substitute_stacks`/
`substitute_yields` exactly like `Section`'s own body is, so a `@push`/
`@global`/`@yield` nested inside a `@loadonce` block still resolves
correctly through an `@extends` chain instead of being silently ignored.
Codegen wraps the block's own `codegen_nodes(...)` output between two
literal `<div wire:ignore>`/`</div>` string pushes — no new runtime
concept, no session/`.await` requirement, safe to use on any template
whether or not it mounts a `@wire(...)` component at all (`wire:ignore` is
simply inert markup on a page with no client runtime patching it).

One sharp edge worth knowing: the parser has no concept of HTML/JS comments
or string literals — it matches `@word` directives by scanning raw text, so
writing a literal `@loadonce`/`@push(...)`/`@wire` inside a `<script>`
comment (e.g. explaining this exact feature in a code comment) gets parsed
as a real directive. See `docs/GOTCHAS.md`.

## `@resource('name', { prop: expr, ... }) ... @endresource`

Source: `crates/larust-view/src/{ast,parser,resolve}.rs` (parsing +
resolution), `crates/larust-macros/src/view.rs` (codegen). Static,
non-reactive template inclusion with props and a slot — Laravel's Blade
`@component`/`@endcomponent` equivalent, the counterpart to `@wire(...)`'s
reactive one. No session storage, no client JS, no round-trip: everything
happens once, at render time, in the same request that renders the page
around it.

```blade
@resource('components.panel', { title: "Your profile.", subtitle: "..." })
<form method="POST" action="/profile">...</form>
@endresource
```

Always a block — `@endresource` is required even when nothing meaningful
goes between the tags — deliberately unlike `@wire(...)`'s self-closing
form, so the parser never needs lookahead to decide whether a body
follows; matches Blade's own `@component`/`@endcomponent` shape, which is
also always paired.

**Props become real `let` bindings, not a serialized payload.** This is
the one place `@resource(...)` is *simpler* than `@wire(...)`: since
nothing here crosses a session or JSON boundary, each `key: expr` entry
(parsed by the same `parse_prop_entries` scanner `@wire(...)` uses) just
becomes `let key = expr;` in the included template's own scope at codegen
time — full type inference, no `serde_json` round-trip, no
`.expect()`-on-serialize-failure escape hatch needed at all.

**The slot.** `@resource(...)`'s captured body — everything between the
opening tag and `@endresource` — is stored as `Node::Resource`'s own
`slot: Vec<Node>` field, parsed exactly like any other block body (reuses
`parse_nodes`, so it can contain `@if`/`@foreach`/interpolations/even a
nested `@resource(...)`, same as `@loadonce`'s body can). The key design
decision: **the slot's expressions resolve against the *caller's* own
scope, not the included template's.** Codegen renders the slot first
(`codegen_nodes(slot, ctx)`, using the caller's own in-scope variables)
into an isolated `String` buffer, and binds that as a plain `slot`
variable — the included template places it anywhere via the *already-
existing* `{!! slot !!}` raw-interpolation mechanism (raw, not escaped,
since it's the app's own already-rendered markup, not untrusted input).
No new "slot placeholder" AST concept was needed on the receiving side at
all — from the included template's point of view, `slot` is just another
context variable.

Codegen for `Node::Resource { name, props, slot }`:
1. Each prop becomes `let #ident = #expr;` (an error here — a malformed
   key that isn't a valid Rust identifier, or an expression that doesn't
   parse — surfaces via `syn::Error::to_compile_error()` at that exact
   point, same pattern every other arm in this file uses).
2. `slot` is codegen'd into its own buffer and bound as a `String`.
3. The named template is loaded via the *same* `load_template` helper
   `expand()` itself uses for the root template (registering it as a real
   `include_str!` build dependency too, so editing the included file
   triggers a rebuild) — then its own resolved node list is codegen'd
   *directly into the same output buffer* as the caller, exactly like
   `@if`/`@foreach` already do (no separate buffer, no runtime dispatch).
All three are wrapped in one `{ }` block scope, so the prop/slot bindings
can't leak into or collide with the caller's own variables.

**Known v1 limitations, both accepted rather than solved:**
- An included template does **not** get its own `larust_view::resolve()`
  pass — no `@extends`/`@push`/`@globals` chain of its own. It's meant to
  be a small, self-contained partial, not a full page; `@extends` inside
  one silently renders as nothing (same existing fallback behavior a
  standalone top-level template with no `@extends` already has — not a
  new gap introduced here).
- `@wire(...)` used *directly inside* an included template's own file
  (not inside a slot, which is part of the caller's tree and is scanned
  normally) won't be detected by `@larustscripts`'s `contains_wire` check,
  since that scan never loads the included file. Static components aren't
  meant to host reactive ones; if a real need for that shows up, this
  would need `contains_wire` to become file-loading-aware.
- Since the slot renders in the caller's scope but the included template's
  own body renders in *its own* `let`-bound scope, a resource template's
  `@csrf` (which reads a bare `csrf_token` identifier) only works if
  `csrf_token` happens to already be in scope in the caller's `view!`
  context — same implicit-capture behavior as any other variable the
  included template references but never receives as an explicit prop.

Demo example: `demo/resources/views/components/panel.blade.xr` (`title`/
`subtitle`/`extra_class` props, a slot) wraps both `<section
class="form-card">` blocks on `/profile` — see
`demo/resources/views/profile/show.blade.xr`.

### Tag syntax: `<resource:name attr="literal" :attr2="expr">...</resource:name>`

An alternate, HTML-tag-flavored spelling of the exact same directive above
— not a separate feature, and not a replacement: both forms parse to the
identical `Node::Resource`, so `resolve.rs` and codegen can't tell them
apart, and a template is free to mix both. Added specifically because a
component with a substantial slot reads more like ordinary markup this
way than as a `@resource(...) ... @endresource` pair.

```blade
<resource:components.badge label="New" />

<resource:components.panel title="Your profile." :subtitle="tagline">
    <form method="POST" action="/profile">...</form>
</resource:components.panel>
```

Self-closing (`<resource:name ... />`) for an empty slot — the tag-syntax
equivalent of `@resource('name', { ... })@endresource`; block form
(`<resource:name ...>...</resource:name>`) otherwise, with the closing tag
repeating the full name. Unlike a `@endresource` closer (which, like every
other `@endXxx`, closes whichever `@resource(...)` most recently opened,
with no name of its own to check), a closing `</resource:name>` **is**
checked against its opening tag's name — a mismatch (a rename that only
updated one side) is a parse error naming both the expected and found tag,
not a silent misparse.

**Attributes, not a props object.** Plain `attr="value"` is a **literal**
prop — the raw attribute text is escaped and wrapped in a Rust string
literal at parse time, so `title="Your profile."` becomes exactly the same
prop `{ title: "Your profile." }` would. A leading `:` marks the value as a
**raw Rust expression** instead — `:subtitle="tagline"` is `{ subtitle:
tagline }` — Blade's own `<x-alert :message="$message">` convention (the
same convention this framework's `x-alert`-equivalent components were
always meant to follow). Both forms feed the identical `props: Vec<(String,
String)>` the directive syntax already builds, via the same shared
`parse_quoted_string` scanner for each attribute's quoted value.

Implemented entirely in `larust-view/src/parser.rs` — two new marker kinds
(`<resource:` opens, `</resource:` closes) recognized alongside `@word`/
`{{ }}`/`{!! !!}` in the same scan `next_marker` already does, plus
`parse_resource_tag`/`parse_resource_tag_attrs`. No changes anywhere else
in the pipeline (`ast.rs`, `resolve.rs`, `larust-macros/src/view.rs`) were
needed, since the output is literally `Node::Resource` — proven directly in
`crates/larust-macros/tests/view_resource_tag.rs`, which asserts the tag
form renders byte-for-byte identically to the directive form.

Same sharp edge as every other marker this parser recognizes (see
`docs/GOTCHAS.md`): no comment/string awareness, so literal text containing
`<resource:` or `</resource:` (inside a `<script>` comment explaining this
exact feature, say) parses as a real tag.

Demo example: `demo/resources/views/profile/show.blade.xr` uses this form
for both `<resource:components.panel>` blocks — all-literal props (`title`,
`subtitle`, `extra_class`), each with a substantial `<form>` slot.

## `@live(channel_expr) ... @endlive`

Source: `crates/larust-view/src/{ast,parser,resolve}.rs` (parsing +
resolution), `crates/larust-macros/src/view.rs` (codegen), `crates/
larust-live/src/push.rs` (server-side broadcast + WebSocket route),
`crates/larust-live/assets/push-runtime.js` (client). Genuine
server-*pushed* real-time updates — the directive name `@live`/`@endlive`
was deliberately freed up for this by renaming the old reactive-component
directive to `@wire(...)` (see above); full rationale for the three-way
split (`@wire` / `@resource` / `@live`) lives in `docs/ARCHITECTURE.md`.
No component trait, no session state, no server-side struct at all —
this is deliberately the simplest of the three: a thin
render-once-then-patch-on-broadcast primitive, not a stateful component.

```blade
@live("posts.count")
    @resource('components.post-count-ticker', { count: count })
    @endresource
@endlive

@live(format!("post.{}.comments", post.id))
    <span>{{ comment_count }} comments</span>
@endlive
```

Unlike `@wire(...)`/`@resource(...)`'s `name`, which is always a quoted
string parsed by `parse_quoted_string`, `@live`'s `channel` argument is an
**arbitrary Rust expression**, parsed via `parse_paren_expr` — there's no
compile-time file/registry lookup keyed on it (unlike `@wire`'s component
name or `@resource`'s template path), so nothing stops it from being
dynamic. One consequence worth remembering: a plain string channel must
still be double-quoted (`@live("ticker")`), since Rust's single-quote
syntax means a `char`/lifetime, not a string — `@live('ticker')` fails to
parse as a valid `syn::Expr` at macro-expansion time, not silently.

`@live`'s body — everything between the opening tag and `@endlive` —
parses into `Node::Live { channel, body }`'s own `body: Vec<Node>` via the
same `parse_nodes` block-body machinery `@loadonce`/`@resource`'s slot use,
so it can contain `@if`/`@foreach`/interpolations/even a nested
`@resource(...)` (the demo's own usage above composes `@live` directly
around a `@resource` call for exactly this reason — see below). Threaded
through every `resolve.rs` recursive pass (`collect_pushes`/`contains_push`/
`collect_globals_into`/`contains_globals`/`substitute_globals`/
`substitute_stacks`/`substitute_yields`) identically to `Node::LoadOnce`'s
own body, so a `@push`/`@global`/`@yield` nested inside still resolves
correctly through an `@extends` chain.

Codegen renders `body` once, inline, **in the caller's own scope** — no
separate buffer, no runtime dispatch, no registry, same "codegens directly
into the caller's output" shape `@resource`'s included-template body uses
— wrapped in a `<div data-live-channel="{escaped channel}">...</div>`. No
`.await`/`?`/session requirement: unlike `@wire(...)`, mounting doesn't
touch session storage at all, so a template using only `@live` (no
`@wire`) needs no session in its `view!(...)` context.

**Server side (`larust-live::push`)**: a process-wide
`OnceLock<Mutex<HashMap<String, tokio::sync::broadcast::Sender<String>>>>`
channel registry, created lazily per channel name on first use (no
upfront registration, unlike `@wire`'s `LiveRegistry` — channels are just
strings, not typed components). `push::broadcast(channel, html)` publishes
a new HTML fragment to every current subscriber (a no-op, not an error, if
nobody's listening — fire-and-forget). `push::wrap(channel, inner_html)`
produces the *exact* `<div data-live-channel="...">...</div>` shape the
`@live` directive itself renders, so a broadcast payload and the page's own
initial render can never drift out of the shape the client's DOM patcher
expects. `push::socket` is the `GET /__larust_push/{channel}` WebSocket
upgrade handler; `push::runtime_js` serves the vendored client script at
`GET /__larust_push/runtime.js`. Both routes are registered **explicitly**
by the app, same as `@wire`'s `/__larust_wire/*` routes — nothing is
auto-mounted.

**`@larustscripts` emits both scripts independently.** A new
`contains_live()` scan (parallel to `@wire`'s existing `contains_wire()`)
computes `emit_push_scripts`, threaded through `CodegenCtx` alongside
`emit_wire_scripts`. A page using only `@live` gets just the push runtime
script; a page using only `@wire` gets just the wire runtime script; a page
using both (like the demo's home page, which composes `@live` around a
`@resource`-rendered fragment but also has `@wire('post-form')` elsewhere
in the same layout) gets both, independently — proven in
`crates/larust-macros/tests/view_larustscripts.rs`.

**Client side (`push-runtime.js`)**: connects to `/__larust_push/
{encodeURIComponent(channel)}` over WebSocket for every
`[data-live-channel]` element on the page, reconnecting on close after a
fixed 2000ms delay. Its DOM patcher (`larustPushPatch`) is a **deliberate,
near-verbatim duplicate** of `wire-runtime.js`'s own patcher, not a shared
module — the two files are independently vendored and served, with no
bundler between them, so sharing code would mean introducing build
tooling neither one currently needs.

**Why `@live` doesn't reuse `@wire`'s machinery.** The two directives
solve opposite-direction problems: `@wire` is *client-initiated*
(something the user does in *this* browser tab triggers a request, gets a
response, patches *this* tab) — `@live` is *server-initiated* (something
that happened anywhere — another user's request, a background job, an
event listener — pushes to *every* subscribed tab, including ones where
nobody did anything at all). Bolting push delivery onto `@wire`'s
session-keyed, per-component state would have meant either fanning a
broadcast out to every session's stored component (awkward, and session
storage was never designed to be iterated) or inventing a second identity
scheme just for push targets — a plain named channel, decoupled from any
session or component instance, is simpler and matches what the feature
actually needs.

**Known v1 limitation, deliberately not solved by the framework:** nothing
enforces that a channel's initial render (via `@live` + whatever's inside
it) and its broadcast payload (built server-side, wherever
`push::broadcast` is called) stay in the same shape — the app has to keep
them in sync itself. The mitigation, demonstrated in the demo: use the
*same* `@resource`-included template for both, so there's only one place
that shape is defined at all (see below).

Demo example: `demo/resources/views/welcome.blade.xr`'s home-page post
counter — `@live("posts.count")@resource('components.post-count-ticker',
{ count: count })@endresource@endlive`. `demo/src/main.rs`'s `PostCreated`
event listener re-queries the count and broadcasts a fresh fragment
rendered from the *exact same* `components.post-count-ticker.blade.xr`
template via `larust_support::view!(...).into_html()` +
`larust_support::push::wrap(...)` — structurally preventing the initial
render and the broadcast from ever drifting apart. End-to-end proof (a
real WebSocket client, a real created post, an asserted incoming
broadcast) in `demo/tests/live_ticker_test.rs`.

## `@can(expr) ... @endcan` / `@role(expr) ... @endrole`

Source: `crates/larust-view/src/{ast,parser,resolve}.rs` (parsing +
resolution — `Node::Can`/`Node::Role`), `crates/larust-macros/src/view.rs`
(codegen), `crates/larust-permissions/src/lib.rs` (the actual
`has_permission_to`/`has_role` checks, re-exported through
`larust_support::permission`, behind the optional `permissions` feature on
`larust-support` — see that crate's own doc comment for the full
roles/permissions design, a narrowed `spatie/laravel-permission`
equivalent). The template-level half of that crate's own permission model:
before this existed, a route handler had to pre-compute a bool and pass it
into `view!(...)` as a plain context variable; these two directives let a
template check directly.

```blade
@can(Permission::EditPosts)
    <a href="/posts/{{ post.id }}/edit">Edit</a>
@else
    <span class="text-muted">Read only</span>
@endcan

@role(Role::Admin)
    <a href="/admin">Admin panel</a>
@endrole
```

`expr` is an **arbitrary Rust expression** — same "no compile-time name
lookup, purely textual" argument shape `@live`'s `channel`/`@js`'s `$expr`
use, parsed via `parse_paren_expr` — not a quoted string. This is the whole
point, not an incidental choice: `larust_permissions`'s own doc comment
names its hybrid design as "names are compile-checked, assignment is
not" — a plain string `@can('edit-posts')` would silently compile a typo'd
permission name into an always-false lookup, exactly the "silently-always-
false runtime lookup" `larust_auth::Policy`'s own doc comment says this
codebase avoids everywhere else. `@can(Permission::EditPosts)` makes the
same typo (`EditPost`, missing the `s`) a `rustc` error at the template's
own call site instead, same as any other misspelled identifier in this
codebase.

Both directives support an optional `@else ... @endcan`/`@endrole` — no
`@elsecan`/`@elserole` chaining (unlike `@if`/`@elseif`): a permission or
role check only ever tests one name, so there's nothing for an "else if" to
chain against. Parsed by `parse_else_tail`, a smaller sibling of `@if`'s own
`parse_if_tail` kept deliberately separate rather than generalizing that
function, so `@if`'s `@elseif`-desugaring logic stays untouched.

**Requires a `user: &U` (`U: Authenticatable`) binding in the `view!`
context, and an async, `Result`-returning call site** — checked eagerly by
`expand()`'s `contains_can_or_role` scan, the same "requires a binding,
checked eagerly, before codegen ever reaches this arm" shape `@wire(...)`'s
own `session` requirement already established (see that directive's own
docs above). A permission/role check is a real DB round trip, not a
compile-time or pure-runtime-data lookup, so it needs `.await` the same way
`@wire(...)`'s `mount()` call does:

```rust
async fn edit_post(user: AuthUser, Path(id): Path<i64>) -> Result<View, AppError> {
    let post = Post::find(id).await?.ok_or(AppError::NotFound)?;
    Ok(view!("posts.show", { user: &user, post }))
}
```

Codegen emits `if ::larust_support::permission::has_permission_to(user,
#expr).await? { ... } else { ... }` (and the `has_role` equivalent for
`@role`) — **`.await?`, not `.unwrap_or(false)`**: a DB error while
checking propagates as a real error, matching this codebase's "never
silently swallow errors" convention, not silently rendered as "no
permission." Using either directive in a template whose own `larust-support`
dependency doesn't have the `permissions` feature enabled fails with an
ordinary Rust "unresolved module `permission`" compile error — the same
"the compiler tells you what's missing" outcome any other feature-gated
`larust_support` re-export already has when used without its feature; there
is no bespoke friendlier diagnostic for this specific case.

Threaded through every `resolve.rs` recursive pass identically to
`Node::If`'s own `then_branch`/`else_branch` — `collect_pushes`/
`contains_push`, `contains_globals`, `substitute_globals`/
`substitute_stacks`/`substitute_yields` — so `@push`/`@global`/`@yield`
nested inside still resolve correctly through an `@extends` chain. One
exception, matching `@if`'s own: a `@globals` *block* (declaring a new
value) directly inside `@can`/`@role` is rejected at compile time with a
clear error, for the same reason `@globals` inside `@if` is — the value
would be resolved once, unconditionally, from every branch, regardless of
whether the current user actually has the permission/role at render time.
Reading an already-declared value via `@global(name)` inside `@can`/`@role`
is unaffected — only *declaring* one there is rejected.

## `{{-- comment --}}`

Source: `crates/larust-view/src/parser.rs` (`MarkerKind::Comment`), `crates/
larust-convert/src/blade/scan.rs` (Laravel Blade comment carry-through
during `xr convert`). Blade's own comment syntax, produces no `Node` at
all — consumed and discarded at parse time, matching Laravel's real
`{{-- ... --}}` semantics (zero output, not a visible `<!-- -->` HTML
comment).

```blade
{{-- TODO: revisit this once the new pricing page ships --}}
<p>{{ post.title }}</p>

{{-- @if(feature_flag) <div>old layout</div> @endif --}}
```

**Consumed as one atomic span**, before the normal `{{`/`{!!`/`@`-scanning
in `parse_nodes`'s loop ever looks inside it — the same reason
`{{--`/`Marker::Comment` is checked *before* the plain `{{`/`Escaped`
marker in `next_marker` (both match at the same starting offset for a real
comment; the more specific one has to win). This is what makes it safe for
a comment to contain its own `{{ }}`/`{!! !!}`/`@directive` syntax — the
common "temporarily disable this block" pattern — without any of it
resurfacing as live, active template syntax: the whole comment, delimiters
included, is skipped over as dead text in a single scan, never fed back
through this same loop the way a naive plain-HTML-comment passthrough
would be. Not nestable, matching real Blade: the *first* `--}}` found
always closes a comment, full stop.

**`xr convert` carries a Laravel Blade comment straight through as this
same syntax, verbatim and untranslated** (`blade::scan`'s own
`Marker::BladeComment` handling) — a real fix, not the original design: an
earlier version of this converter produced no output at all for a Blade
comment, reasoning that faithfully matching Laravel's own "renders
nothing" semantics was enough. That was correct about the render-time
behavior but wrong about the source-level one — it also silently discarded
comments a developer wrote on purpose, including the "temporarily disable
this block" pattern above, which a Laravel developer relies on being able
to find and re-enable later. Once `.blade.xr` gained this same atomic
comment token, carrying the original text through verbatim became simply
correct: the exact "nested `{{ }}`/`@directive` syntax resurfacing as
live code" risk that motivated dropping the content in the first place is
now structurally impossible, regardless of what the comment contains.

## `@js($expr)`

Source: `crates/larust-view/src/ast.rs` (`Node::Js`), `crates/larust-view/
src/parser.rs` (parsing), `crates/larust-view/src/runtime.rs` (the `js`
encoder), `crates/larust-macros/src/view.rs` (codegen), `crates/
larust-convert/src/blade/scan.rs` (Laravel `@js(...)` recognition during
`xr convert`). Larust's equivalent of Laravel/Livewire's own `@js()`
directive: drops an arbitrary Rust value into inline JavaScript as
safely-escaped JSON, for handing server data to client-side code without
hand-writing a `<script>` block full of manual escaping.

```blade
<script>
  const post = @js(post);
  Livewire.dispatch('post-loaded', @js(post.id));
</script>
```

`$expr` is an **arbitrary Rust expression**, parsed via `parse_paren_expr`
— the same "no compile-time name lookup, purely textual" argument shape
`@live`'s `channel` uses (see above), not a quoted-string argument like
`@wire`/`@resource`'s `name`. Codegen requires only that the expression's
type implement `serde::Serialize` — there is no other constraint, and
**that requirement is enforced by rustc's own post-expansion type-checking,
not by the macro itself**: `view!` has no access to resolved types at
expansion time (it works purely on `syn`-parsed expression text), so a
non-`Serialize` value fails to compile with an ordinary trait-bound error
pointing at the generated call site, the same enforcement shape
`@wire(...)`'s own prop serialization already relies on.

**Escaping is JS-safe, not HTML-safe — deliberately a different transform
than `{{ }}`.** `larust_view::runtime::js` mirrors Laravel's own
`Illuminate\Support\Js::from()` two-layer mechanism faithfully:

1. JSON-encode the value, then hex-escape `<`, `>`, `&`, `'` in the result
   (`JSON_HEX_TAG`/`JSON_HEX_AMP`/`JSON_HEX_APOS` equivalents) — what makes
   it safe to embed inside an HTML attribute or a `<script>` block without
   a string value like `"</script>"` breaking out of context.
2. Wrap that hex-escaped string as `JSON.parse('...')`, JSON-re-encoding it
   (escaping backslashes/newlines/quotes, including the `\u...` sequences
   step one just introduced) and stripping the outer `"..."` quotes that
   pass adds — what makes the whole token safe to embed directly inside a
   single-quoted JS string context with no caller-added quoting.

A value's own type never needs to change to be `@js(...)`-able — any
`#[derive(Serialize)]` struct already used for an API response or a
`@wire(...)` prop works here unchanged. `.expect()`-ed on serialization
failure (the same "programmer bug, not runtime-data territory" tolerance
`@wire(...)`'s own prop serialization already applies) — a panic here
degrades to a request-scoped 500 via `CatchPanicLayer`, not a process
crash.

**`xr convert` recognizes Laravel's own `@js(...)`** as a leaf construct —
like `{{ }}`, not like a paired block — so an expression it can't translate
degrades in place (a numbered placeholder + a `CONVERSION_REPORT.md` note)
rather than rejecting the whole file.

### Matching TypeScript types via `ts-rs` (opt-in convention, not built-in
tooling)

Larust does not generate `.d.ts` files itself, and does not need to: any
struct already carrying `#[derive(Serialize)]` for `@js(...)` can also
derive [`ts-rs`](https://crates.io/crates/ts-rs)'s `TS` trait and get a
matching hand-verifiable TypeScript type for free —

```rust
#[derive(Serialize, ts_rs::TS)]
#[ts(export)]
struct Post {
    id: i64,
    title: String,
}
```

— add `ts-rs` to the **app's own** `Cargo.toml` (never Larust's workspace
`Cargo.toml` — no frontend build tooling exists in any Larust scaffold
today, so this stays strictly opt-in per app). Running `cargo test`
generates the corresponding `.ts` file under `bindings/`, which the
frontend build imports directly. This is a convention, not enforced
tooling: nothing checks that a `@js(...)`'d struct has a matching
`#[ts(export)]`, and nothing needs to — the safety boundary is
`Serialize` + `runtime::js`'s JS-safe encoding above, which holds
regardless of whether `ts-rs` is used at all. `ts-rs`'s generated type is
purely additive information for the frontend.

## `@globals ... @endglobals` / `@global(name, fallback)` / `persist`

Source: `crates/larust-view/src/ast.rs` (`Node::Globals`/`GlobalEntry`/
`Node::Global`/`Node::PersistGlobal`), `crates/larust-view/src/parser.rs`
(parsing), `crates/larust-view/src/resolve.rs` (`collect_globals`/
`substitute_globals`), `crates/larust-macros/src/view.rs` (codegen).
Laravel has no direct equivalent — closest in spirit to a combination of
`@stack`/`@push` (whole-`@extends`-chain, collect-then-substitute
resolution) and a view-composer-set variable, but resolved entirely at
**compile time**, not per-request.

A page overrides a named placeholder a layout reads:

```blade
{{-- layouts/app.blade.xr --}}
<title>@global(title, "Larust")</title>

{{-- posts/show.blade.xr --}}
@extends('layouts.app')
@globals
    title = format!("{} | Larust", post.title)
@endglobals
```

One or more `name = expr` lines per `@globals` block; `expr` is a real,
arbitrary Rust expression (any `ToString`-yielding value, not just string
literals), stored as raw text and only parsed into a `syn::Expr` at
codegen time — same convention as `{{ }}`. Resolution is whole-chain,
collect-then-substitute (not per-level, unlike `@section`/`@yield`): every
`@globals` block anywhere in the `@extends` chain is gathered first, child
page wins over an ancestor layout for the same name, *then* every
`@global(name, fallback)` placeholder is substituted — so an indifferent
middle layout that doesn't touch `title` at all doesn't block a leaf
page's value from reaching a shared base layout. `@globals`/`@push` inside
`@if`/`@foreach` is a compile error (resolved once, unconditionally,
which can't express per-branch/per-iteration semantics) — set the value
unconditionally, or compute it with a conditional expression instead
(`title = if cond { "A" } else { "B" }`).

An unset global (no `@globals` block anywhere sets that name) renders
`fallback` if given, or nothing at all — same "unset becomes empty"
convention as an unset `@stack`.

### `persist` — cookie-backed, not compile-time-literal

A `persist`-prefixed entry (`persist theme = "dark"`) is fundamentally
different from every other `@globals` entry: its value isn't known at
*compile* time at all — it's read from a long-lived browser cookie at
*request* time, falling back to the given expression only when that
cookie is absent. This is what backs a genuinely persistent user
preference (dark/light theme is the driving case) that survives page
navigation, browser restarts, and login/logout — unlike `Session`, which
has no configured expiry by default (dies when the browser closes) and
rotates its ID on every login (`Session::cycle_id()`, session-fixation
prevention) — the wrong lifetime for "this never resets."

```blade
@globals
    persist theme = "dark"
@endglobals
<html data-theme="@global(theme, "dark")">
```

**Read-only from Rust's side, by design.** There is no server-side write
endpoint — the write path is the browser setting `document.cookie`
directly (e.g. a theme-toggle button flips `data-theme` instantly and
sets the cookie in the same click handler), so the *next* server render
picks it up automatically with no round trip needed for the toggle
itself. See `larust_http::preferences`'s own doc comment for the cookie
naming convention (`larust_pref_{name}`) and why it's deliberately
unsigned (tampering with your own preference has no security
consequence).

**Requires a `cookies: &CookieJar` binding in the `view!` context** —
checked eagerly at compile time (mirroring `@wire(...)`'s own `session`
requirement) against the *resolved* tree, so a page using a `persist`
global anywhere in its `@extends` chain fails to compile with a clear
message at the `view!` call site if the controller doesn't supply one:

```rust
use larust_support::preferences::CookieJar;

async fn show(session: Session, cookies: CookieJar, ...) -> ... {
    Ok(view!("posts.show", { cookies: &cookies, ... }))
}
```

Since `persist` typically lives in a shared base layout (theme is a
whole-site concern), this binding requirement propagates to **every**
controller rendering any page through that layout — mechanical, and
compiler-verified (nothing can be silently missed), but real: expect to
touch every affected controller once when introducing the first
`persist` global in a shared layout.

**Deliberately out of scope**: no server-side write endpoint (see
above); no signed/encrypted cookies (`axum_extra`'s `PrivateCookieJar`/
`SignedCookieJar` — unnecessary key-management overhead for a value with
no security consequence); no OS-`prefers-color-scheme` detection on a
first visit with no cookie yet — the app's own fallback expression is
what renders instead.

## `#[derive(Model)]`

Source: `crates/larust-macros/src/model.rs`.

Requires `#[table("name")]` on the struct and exactly one field marked
`#[primary_key]` (currently must be `i64` — a documented, deliberate
limitation, not an oversight). Every other field is "insertable."

Generates:

- `New{StructName}` — a companion struct with just the insertable fields
  (Diesel's own naming convention for the same concept), used as
  `Model::create`'s argument. If there are zero insertable fields (a model
  with only a primary key), the generated `INSERT` uses `DEFAULT VALUES`
  instead of an empty `(...) VALUES (...)` list — SQLite rejects the empty
  form outright, so this needed an explicit special case.
- `TABLE` and one `SCREAMING_SNAKE` constant per field (`Post::TITLE`,
  matching the doc's `User::ACTIVE` example) — used both by app code and by
  `QueryBuilder::where_eq(Post::TITLE, ...)`.
- `query()`, `all()`, `find(id)`, `create(data)`, `delete(id)` — thin
  wrappers over `larust_support::orm::QueryBuilder`/raw `sqlx` calls.
- **Route model binding**: `impl axum::extract::FromRequestParts<S> for
  StructName`. The route parameter name defaults to
  `to_snake_case(struct_name)` (`Post` → `"post"`, matching Laravel's own
  convention that `{post}` binds to a variable/param named `post`).
  `#[route_key("slug")]` (an optional struct attribute) changes the lookup
  column from the primary key to the named field instead — validated
  against the struct's actual field names **at macro-expansion time**, so a
  typo'd column name is a compile error, never a runtime SQL failure. The
  default (primary-key) lookup path parses the raw path segment as `i64`
  and maps a parse failure to `AppError::NotFound` (a clean 404), not a
  propagated error — matching Laravel's own behavior for a non-numeric
  segment on an implicit binding.

**Every generated SQL string quotes table and column names** (`"posts"`,
`"title"`), even though they're always compile-time-known identifiers from
`#[table(...)]` or a field name, never runtime data. This isn't about
injection (values are always bound via `?` placeholders, never
string-interpolated) — it's because an unquoted identifier that happens to
collide with a SQL reserved keyword (a field named `order`, a table named
`group`) breaks every generated query. This was a real, review-caught bug —
see GOTCHAS.md.

Field/table name handling also strips a leading raw-identifier prefix
(`r#type` → `type`) via `field_name_str()` before using a name as either a
generated constant's value or an actual SQL identifier — without this, a
field literally named `type` (a very plausible column name, and only
writable in Rust as `r#type`) would panic the macro rather than compiling,
and even if it didn't panic, the *wrong* string (`"r#type"`, including the
Rust-only escape syntax) would leak into the generated SQL.

### Relationships: `#[has_many(...)]`, `#[has_one(...)]`, `#[belongs_to(...)]`

Source: `crates/larust-macros/src/relations.rs`. Struct-level attributes
recognized alongside `#[table(...)]`/`#[route_key(...)]`, all three
repeatable (a struct can declare more than one). No `QueryBuilder`/ORM
changes were needed for any of this — every relationship is a thin
delegation to machinery `#[derive(Model)]` already generates:

```rust
#[derive(Model, sqlx::FromRow)]
#[table("users")]
#[has_many(Post, foreign_key = "user_id")]
pub struct User {
    #[primary_key]
    pub id: i64,
    pub name: String,
}

#[derive(Model, sqlx::FromRow)]
#[table("posts")]
#[belongs_to(User, foreign_key = "user_id")]
pub struct Post {
    #[primary_key]
    pub id: i64,
    pub user_id: i64,
    pub title: String,
}
```

generates, in a new `impl #struct_name` block appended to the rest of
`#[derive(Model)]`'s output:

```rust
impl User {
    pub async fn posts(&self) -> Result<Vec<Post>, AppError> {
        Post::query().where_eq("user_id", self.id).get().await
    }
}
impl Post {
    pub async fn user(&self) -> Result<Option<User>, AppError> {
        User::find(self.user_id).await
    }
}
```

- **`belongs_to`** (the foreign key lives on *this* struct): delegates
  straight to `Related::find(self.#field)`.
- **`has_one`**/**`has_many`** (the foreign key lives on the *related*
  struct): `Related::query().where_eq(#foreign_key, self.#pk).first()/
  .get()` — identical query, differing only in `.first()` returning
  `Option<Related>` vs `.get()` returning `Vec<Related>`.

`foreign_key = "..."` is **required** on every relationship — never
guessed from naming convention the way Laravel does, matching this
macro's `#[route_key(...)]` precedent of an explicit string over inferred
magic that could silently guess wrong. For `belongs_to`, that string is
validated against the *current* struct's actual fields at macro-expansion
time (must exist, must be `i64` — same rigor as `#[route_key(...)]`'s
field check). For `has_one`/`has_many`, the foreign key lives on the
*related* struct, which this macro invocation has no visibility into, so
its column-name usage stays an unchecked literal (same trust level
`#[table("...")]` already has — a typo'd *column* name is still only a
runtime SQL error) — but it's now *also* required to be a legal Rust
identifier (validated via `parse_ident`, not left to panic `format_ident!`
— see GOTCHAS.md), since the batch loader below needs to read it back as
a real struct field.

The generated method's name defaults to the related type's name
(`to_snake_case`, pluralized for `has_many` via a small `pluralize`
ported — not reimplemented — from `xr make:model`'s own copy in
`crates/larust-cli/src/generate.rs`, so the vowel-detection bug documented
in GOTCHAS.md isn't reintroduced). An optional `method = "..."` override
handles a struct needing two relationships to the same related type (e.g.
`Post`'s `author`/`editor`, both `belongs_to(User, ...)`, which would
otherwise both default to a method named `user` and collide as a
duplicate-method compile error — deliberately left for rustc to catch
rather than reimplemented as a macro-level check). The override string is
validated as a legal Rust identifier *before* being hooked up via
`format_ident!`, which otherwise panics — not returns a `syn::Error` — on
illegal input; see GOTCHAS.md.

#### Batch (eager) loading: `load_*`

Every relationship *also* generates a static batch-loading method
alongside the lazy instance one, turning "N+1 queries" (one
`post.user().await?` per post in a loop) into "2 queries" (fetch the
posts, then one `where_in`-based query for every distinct author — the id
list is deduplicated via a `HashSet` before querying, so 100 posts by 3
authors sends 3 values to `where_in`, not 100):

```rust
impl User {
    pub async fn load_posts(rows: &[Self]) -> Result<HashMap<i64, Vec<Post>>, AppError> { ... }
}
impl Post {
    pub async fn load_user(rows: &[Self]) -> Result<HashMap<i64, User>, AppError> { ... }
}
```

Laravel's `->with(...)` works by mutating a loaded model's dynamic
property table — Rust has no equivalent (a struct's shape is fixed at
compile time), so rather than inventing an "attach the related rows back
onto the struct" mechanism, `load_*` returns a plain lookup map the caller
indexes into explicitly (`authors.get(&post.user_id)`). This is more
explicit than Laravel and intentionally so, matching the same "no
guessing" preference `foreign_key`/`route_key` already established — a
typo'd lookup is a missing `HashMap` entry (`.get()`/
`.unwrap_or_default()`), not a silent wrong answer.

`has_many`'s batch loader groups fetched related rows by `foreign_key`
(already known); `has_one`'s does the same but keeps only the first match
per group (`.or_insert(...)`, not `.or_default().push(...)`), matching its
"at most one related row" semantics. `belongs_to`'s batch loader is the
odd one out: it needs to group fetched related rows by *their own* primary
key, which this macro invocation has no visibility into — a new optional
`related_key = "..."` argument (defaulting to `"id"`, the primary key
field name every model in this codebase uses so far) supplies it, also
routed through `parse_ident`/`is_i64`-free (no type check needed here,
since it's only ever used as a `HashMap<i64, _>` key alongside the
existing `#[primary_key]`-is-`i64` constraint elsewhere). `related_key` is
rejected as an unrecognized argument on `has_many`/`has_one`, where it has
no meaning.

Built entirely on one new `QueryBuilder` primitive:
`where_in(column, values)` (`crates/larust-orm/src/query_builder.rs`) —
`WHERE column IN (?, ?, ...)`, with an empty `values` rendering as `1=0`
(SQLite rejects `"col" IN ()` as a syntax error, and an empty input slice
to a batch loader is a real case, not just a hypothetical).

A `foreign_key`/`related_key` that collides with a Rust keyword (`"type"`)
needs two different spellings in generated code — `r#type` as a
field-access expression, plain `type` as the SQL column string — and
`parse_ident` accordingly tries the plain form first, falling back to an
`r#`-prefixed raw identifier for a keyword rather than rejecting it
outright. The clean (never-prefixed) string and the parsed identifier are
always tracked as two separate values, never derived from one another via
`.to_string()` — an early version of this code got that backwards for
`related_key` and shipped a real, silent bug; see GOTCHAS.md.

Explicitly out of scope for now: a declarative `->with(...)`-style API
that decides which relationships to batch-load and attaches results back
onto structs automatically — not idiomatically possible in Rust's static
type system without real complexity; explicit `load_*` calls are the
permanent design here, not a stepping stone to something more magic.

### Many-to-many: `#[belongs_to_many(...)]`

Source: `crates/larust-macros/src/belongs_to_many.rs` — its own file, not
folded into `relations.rs`, since it needs a real `JOIN`, which
`QueryBuilder` deliberately doesn't support (single-table `SELECT` only).
Generated code hand-writes SQL and calls `sqlx::query`/`query_as`
directly, the same way `#[derive(Model)]`'s own `create`/`delete` already
bypass `QueryBuilder` for shapes it doesn't cover.

```rust
#[derive(Model, sqlx::FromRow)]
#[table("posts")]
#[belongs_to_many(Tag, through = "post_tag", foreign_key = "post_id", related_pivot_key = "tag_id")]
pub struct Post {
    #[primary_key]
    pub id: i64,
    pub title: String,
}
```

generates four methods: `tags()` (an `INNER JOIN` query returning every
related row), `attach_tag(id)`/`detach_tag(id)` (single pivot-row
insert/delete), and `sync_tags(&[id])` (replace the *entire* pivot set for
`self` in one transaction — delete every existing row for `self`, then
insert one per given id; a failure partway through — e.g. a duplicate id
violating the pivot table's own primary key — rolls the whole thing back
rather than leaving the set half-cleared, since the delete and every
insert run against the same `sqlx::Transaction`, only `commit()`ed on the
success path).

Four attribute arguments, all required except `related_key`:
`through`/`foreign_key`/`related_pivot_key` (the pivot table's name, this
struct's column in it, and the related struct's column in it — no
guessing, same stance as every other relationship kind) and `related_key`
(defaulting to `"id"`, the related struct's own primary key column, needed
for the `JOIN`'s `ON` clause — same name and meaning as `belongs_to`'s
existing `related_key`).

`attach_*` uses `INSERT OR IGNORE`, not a bare `INSERT` — attaching an
already-attached pair is a harmless no-op rather than a `UNIQUE`-
constraint error. This is more forgiving than Laravel's own `attach()`,
and it **only works as documented if the pivot table actually has a
`UNIQUE`/`PRIMARY KEY` constraint on `(foreign_key, related_pivot_key)`** —
`OR IGNORE` has nothing to ignore otherwise, and `attach_tag` would insert
a genuine duplicate pivot row every time. The framework doesn't create or
enforce that constraint for you; it has to be part of the pivot table's
own migration (see the composite `PRIMARY KEY (post_id, tag_id)` in
`crates/larust-macros/tests/model_belongs_to_many.rs`'s test migration).

Unlike `has_many`/`has_one`/`belongs_to`, none of `belongs_to_many`'s four
arguments are ever spliced as a Rust field-access expression — every one
is used only inside a hand-written SQL string, and query results are
mapped by `sqlx::FromRow` with no Rust-side column-name knowledge needed
at all. That's a deliberate scope boundary, not an oversight: it's also
exactly why `belongs_to_many` has **no `load_*` eager-loading form** —
building one would need to read a joined row's pivot foreign-key value
back as a real Rust field (the exact role that caused M13's raw-identifier
bug for the other relationship kinds), and `sqlx::FromRow`-derived structs
don't carry pivot columns at all. Solving that needs a synthetic wrapper
row type — a real, separate design problem, not a small extension of the
existing `load_*` pattern.
