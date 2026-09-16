---
title: Coming from Laravel
nav_order: 3
---

# Coming from Laravel
{: .no_toc }

Larust borrows Laravel's vocabulary and directory layout on purpose - the
goal is that you can open a generated app and know where everything is
before you've read a single page of these docs. This page is the honest
version of that promise: what maps directly, what maps with a twist, and
what's deliberately not here at all.

1. TOC
{:toc}

## What maps directly

| Laravel | Larust |
|---|---|
| `composer create-project laravel/laravel` | `xr new` |
| `php artisan <command>` | `xr <command>` |
| `app/Http/Controllers/PostController.php` | `app/Http/Controllers/post_controller.rs` |
| `app/Models/Post.php` | `app/Models/post.rs` |
| `routes/web.php` / `routes/api.php` | `routes/web.rs` / `routes/api.rs` |
| `database/migrations/*.php` | `database/migrations/*.sql` |
| `$table->timestamps()` + Eloquent's auto-touch | `#[timestamps]` on the matching `#[derive(Model)]` struct |
| `resources/views/*.blade.php` | `resources/views/*.blade.xr` |
| `.env` | `.env` (same format, same purpose) |
| `Route::get(...)->name(...)` | `Route::get(...).name(...)` |
| `Route::resource(...)` | `Route::resource(...)` - same 7 actions, same names |
| `Route::middleware('auth')->group(...)` | `.group("", \|r\| r.middleware(...))` |
| Form Requests, `$request->validated()` | `#[derive(FormRequest)]`, `request.validated()` |
| `@extends`/`@section`/`@yield`/`@if`/`@foreach` | Identical directive names in `.blade.xr` |
| `@csrf` | `@csrf` |
| `@push`/`@stack` | `@push`/`@stack` |
| Policies (`Gate`/`authorize()`) | `Policy<U>` + `authorize()` |
| `@can('edit-post', $post) @endcan` | `@can(Permission::EditPosts) @endcan` (role: `@role(Role::Admin) @endrole`) |
| `bezhansalleh/filament-shield` | `larust-shield` - resource-scoped permission bundles, narrower scope (see [Authentication & Authorization](../digging-deeper/authentication-and-authorization#resource-scoped-permission-bundles-larust-shield)) |
| `hasMany`/`belongsTo`/`belongsToMany` | `#[has_many]`/`#[belongs_to]`/`#[belongs_to_many]` |
| Eager loading (`with(...)`) | `load_*` batch methods, checked in tests, not assumed |
| Sanctum | `larust-sanctum` |
| Notifications (database channel) | `larust-notifications` |
| Events & Listeners | `larust-events` |
| Jobs & Queues | `larust-queue` (SQLite or Redis) |
| Task Scheduling (`Schedule::daily()`, ...) | `larust-scheduler` - same method names |
| `Artisan::command('name', $closure)` | `larust-console` - `Command` trait + `xr make:command` |
| Mail (`Mail::to(...)->send(...)`) | `mail().to(...).send(...)` |
| Storage (`Storage::disk('public')`) | `storage::public()`, or `config::filesystems::config().disk('name')` for your own |
| Cache (`Cache::remember(...)`) | `cache::remember(...)` |
| `__('messages.welcome')` / `resources/lang/{locale}.json` | `lang::t("messages.welcome")` / `resources/lang/{locale}.json` - see [Localization](../digging-deeper/localization) |
| Livewire | `@wire(...)` components |
| Broadcasting | `@live(...)` + `larust-reverb` |
| Sitemap packages | `larust-sitemap` |
| Socialite | `larust-socialite` |

## What maps, with a real difference

**Controllers are plain structs with `async fn` methods, not classes with
inherited behavior.** `PostController` doesn't extend a base `Controller`
class - there's no base class to extend, and no `$this->middleware(...)`
called from inside one. Middleware attaches at the *route* (via
`.group(...)`/`.middleware(...)`), not the controller.

**Eloquent's dynamic magic is gone; the compiler is the trade you're
making for it.** `#[derive(Model)]` generates real, typed struct fields
and real `CREATE`/`find`/`update`/`delete` methods at compile time -
there's no `$post->whatever_column` dynamic property access, because
there's no runtime property bag to intercept in the first place. If a
column doesn't exist, `cargo build` tells you immediately, not a
production request six months from now.

**Validation rules are Rust attributes, not strings.** Laravel's
`'title' => 'required|min:3'` becomes `#[validate(required, min = 3)]` on
a struct field. Same rules, same shape, checked by the compiler instead of
parsed at runtime - a typo'd rule name is a compile error, not a silently
ignored no-op.

**`config('app.name')` exists, but it's the exception, not the rule.**
Larust has a real, Laravel-shaped `config(key)` helper (see
[The Basics](../the-basics/) for the full list of keys it covers), but it
only reaches a small, fixed set of framework-known keys. Anything else -
your own app's settings - is a plain Rust function in `config/your_file.rs`
returning a typed value, called directly (`config::blog::posts_per_page()`)
rather than looked up by an arbitrary string key. There's no config-file
autodiscovery and no dot-path into arbitrary nested config the way
Laravel's `config()` can reach anything under `config/*.php`.

**`larust-support` is genuinely called "the facade,"** but it isn't
Laravel's kind of facade. A Laravel facade (`Route::get(...)`,
`Cache::remember(...)`) is a static-looking call that resolves a real
object out of the service container at runtime - it's how Laravel gets
away with `Auth::user()` reading like a static method despite `Auth`
being a stateful, swappable, testable object underneath. `larust-support`
is a facade in the older, plainer sense: **one crate that re-exports
everything from every other framework crate**, so a generated app's
`Cargo.toml` only ever needs one framework dependency. There's no service
container, no runtime binding/resolution, and nothing to swap at runtime -
`larust_support::cache::remember(...)` is a direct function call to a
function that's always exactly the function it looks like.

**Middleware is a function, not a string alias.** There's no
`Kernel.php`-style named middleware registry - you pass the actual
function (`axum::middleware::from_fn(require_auth)`) at the route or
group where you want it. There's nothing to look up by name, and nothing
that can silently fail to resolve.

**Migrations are forward-only.** There's no `down()` method and no
`migrate:rollback` - every migration in this codebase is a plain,
hand-written `.sql` file applied once, in order. `xr migrate:fresh` (drop
every table, reapply every migration) is the honest tool this framework
offers instead of a rollback story it can't actually guarantee; see
[Migrations & the Query Builder](../database/migrations-and-query-builder).

## What isn't here (yet, or at all)

**No service container, no reflection-based dependency injection.**
Nothing in Larust resolves a type out of a container by reflection -
there's no reflection, so there's nothing to build here in Laravel's own
shape. This isn't a gap waiting to be closed; it's load-bearing to the
whole design (see [Coming from Rust](../coming-from-rust#where-this-framework-is-deliberately-opinionated)).
Worth knowing: you're not actually without *any* equivalent - every route
is real Axum underneath, and Axum's own `Extension<T>`/`State<T>`
extractors are real, working dependency injection (register a value once
at boot, any handler pulls it out by type) - Larust's own subsystems just
don't happen to use that mechanism (they use process-wide singleton
accessors like `larust_support::cache::remember(...)` instead, [by
design](../coming-from-rust#where-this-framework-is-deliberately-opinionated)).
Nothing stops your own app code from reaching for `State<T>` directly if
you want Axum-shaped DI for something app-specific.

**No `php artisan tinker`.** There's no Rust equivalent to a live REPL
against your app's own models yet - see [FAQ](../faq) for why, and what to
reach for instead (mostly: a real integration test via
[`TestClient`](../testing)).

**`@php` blocks and arbitrary Blade expressions don't exist.** Every
`.blade.xr` {% raw %}`{{ }}`/`{!! !!}`{% endraw %} interpolation is parsed as a real Rust
expression (`syn::parse_str::<syn::Expr>`) and spliced into generated
code - there's no way to drop into arbitrary PHP the way `@php ... @endphp`
lets you in Blade, because there's no PHP interpreter underneath to run it
on. See [Views & Templates](../the-basics/views-and-templates) for exactly
which expression shapes are supported.

## Bringing an existing Laravel app over

If you have a real Laravel app you want to move rather than a fresh start,
`xr convert` is built for exactly that - see [Converting a Laravel
App](../converting-a-laravel-app). It's honest about what it can and can't
do automatically: routes, migrations, config, validation rules, and
templates translate mechanically where it's safe to do so; anything it
can't safely translate is flagged in a report and left for you, never
silently guessed at.

## Next

[Getting Started](../getting-started/installation) to build something, or
[The Basics](../the-basics/) to see routing, controllers, and validation in
Larust's own terms.
