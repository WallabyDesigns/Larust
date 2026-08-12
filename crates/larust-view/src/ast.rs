/// A parsed `.blade.xr` template, before layout resolution.
///
/// Expression text (`expr`, `cond`, `iter`) is kept as raw strings here —
/// this crate is pure text parsing with no `syn`/`proc-macro2` dependency.
/// `larust-macros` parses those strings into real `syn::Expr`s at codegen
/// time, which is also what makes `{{ user.name }}` a genuine, type-checked
/// Rust expression rather than a custom mini-language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Text(String),
    Interpolate {
        expr: String,
        /// `{{ }}` (HTML-escaped) vs `{!! !!}` (raw).
        escape: bool,
    },
    If {
        cond: String,
        then_branch: Vec<Node>,
        else_branch: Vec<Node>,
    },
    Foreach {
        /// A single identifier binding (no destructuring patterns in M3).
        binding: String,
        iter: String,
        body: Vec<Node>,
    },
    Extends(String),
    Section {
        name: String,
        body: Vec<Node>,
    },
    Yield(String),
    /// `@push('name') ... @endpush` — unlike `@section` (single body,
    /// last-write-wins), multiple `@push`es to the same name *accumulate*;
    /// see `larust_view::resolve` for how they're collected across an
    /// `@extends` chain and spliced into the matching `@stack`.
    Push {
        name: String,
        body: Vec<Node>,
    },
    /// `@stack('name')` — renders every `@push` body pushed to that name,
    /// concatenated in the order they were collected. Laravel's own
    /// pairing: `@stack` sits in the layout (usually `<head>` or right
    /// before `</body>`), `@push` calls live in whichever child templates
    /// have page-specific `<script>`/`<meta>`/etc. to contribute.
    Stack(String),
    /// `@csrf` — a hidden CSRF-token input field. Expands to a fixed
    /// `<input>` reading a `csrf_token` variable that must be present in
    /// the view's context (a compile-time-checked convention, same as any
    /// other undeclared-variable-becomes-an-error interpolation).
    Csrf,
    /// `@global(name)` or `@global(name, fallback)` — a page-overridable
    /// named placeholder, usually in a layout
    /// (`<title>@global(title, "Larust")</title>`). Single value,
    /// last-write-wins per name across the whole `@extends` chain — see
    /// `Globals` and `larust_view::resolve` for how a page sets it and why
    /// resolution is a whole-chain collect-then-substitute pass (same shape
    /// as `Push`/`Stack`), not a per-level one (unlike `Section`/`Yield`).
    /// `fallback`, like `Globals`' own `expr` values, is a raw Rust
    /// expression string, used only when no `@globals` block anywhere in
    /// the chain sets `name` — if absent, an unset global renders empty,
    /// same convention as an unset `@stack`.
    Global {
        name: String,
        fallback: Option<String>,
    },
    /// `@globals ... @endglobals` — one or more `name = expr` assignment
    /// lines on a page; each overrides the matching `@global(name)`
    /// placeholder anywhere in whichever layout(s) this page's `@extends`
    /// chain reaches. `expr` is stored raw (same convention as
    /// `Interpolate::expr`) and is substituted into a real `Interpolate`
    /// node at resolve time, so it's a genuine, type-checked Rust
    /// expression — any type implementing `ToString`, not just string
    /// literals.
    Globals(Vec<(String, String)>),
    /// `@live('name')` or `@live('name', { prop: expr, ... })` — a mount
    /// point for a server-state-backed reactive component (see
    /// `larust-live`). `props` are raw Rust expression strings, same
    /// convention as `Globals`' `(name, expr)` pairs, evaluated and
    /// JSON-encoded at codegen time in `larust-macros`. Unlike
    /// `Section`/`Push`/`Global`, this node needs no `resolve.rs` pass at
    /// all — it renders positionally wherever it appears (including inside
    /// `If`/`Foreach`, deliberately unrestricted, since mounting is an
    /// ordinary runtime statement, not a compile-time collection pass).
    Live {
        name: String,
        props: Vec<(String, String)>,
    },
    /// `@larustscripts` — Livewire's `@livewireScripts` equivalent: a
    /// layout-placed marker (conventionally right before `</body>`) that
    /// expands to the `<script src="/__larust_live/runtime.js" defer>`
    /// tag *only if* this page actually mounts a `@live(...)` component
    /// somewhere in its resolved tree (itself or, via `@extends`, the
    /// layout it's rendered through) — otherwise it expands to nothing.
    /// Written once in a shared layout so no individual page needs its own
    /// `<script>` tag for `wire:model`/`wire:click`/`wire:submit` to work;
    /// see `larust_macros::view::expand`'s `contains_live` check for how
    /// "only if" is decided at compile time, not a runtime branch.
    LarustScripts,
    /// `@loadonce ... @endloadonce` — sugar for wrapping `body` in a
    /// `<div wire:ignore>...</div>`. Content inside still renders on
    /// *every* server response (including every `@live(...)` component
    /// re-render fragment) — the "once" is enforced client-side, by
    /// `wire:ignore` telling the DOM patcher to skip that subtree entirely
    /// after its first mount, not by the server omitting it. That
    /// distinction matters: if the server instead omitted this block from
    /// later fragments, the client's *positional* child diffing (see
    /// `larust-live/assets/live-runtime.js`) would misalign every sibling
    /// after it — this node exists specifically so callers get the
    /// "contained, load-once" ergonomics without hand-writing `wire:ignore`
    /// and without that correctness trap. Safe to use even outside a
    /// `@live(...)` template: `wire:ignore` is simply inert markup on a
    /// page with no client runtime patching it.
    LoadOnce(Vec<Node>),
}
