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
| `hasMany`/`belongsTo`/`belongsToMany` | `#[has_many]`/`#[belongs_to]`/`#[belongs_to_many]` |
| Eager loading (`with(...)`) | `load_*` batch methods, checked in tests, not assumed |
| Sanctum | `larust-sanctum` |
| Notifications (database channel) | `larust-notifications` |
| Events & Listeners | `larust-events` |
| Jobs & Queues | `larust-queue` (SQLite or Redis) |
| Task Scheduling (`Schedule::daily()`, ...) | `larust-scheduler` - same method names |
| Mail (`Mail::to(...)->send(...)`) | `mail().to(...).send(...)` |
| Storage (`Storage::disk('public')`) | `storage::public()` |
| Cache (`Cache::remember(...)`) | `cache::remember(...)` |
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

**No service container, no dependency injection, no auto-resolution.**
Nothing in Larust resolves a type out of a container by reflection -
there's no reflection. Everything you use, you call or construct
explicitly. If you're used to type-hinting a dependency into a controller
method and trusting Laravel to hand you the right instance, the Larust
equivalent is: pass it explicitly, or reach for it as a direct function
call the way `larust_support::cache::remember(...)` does.

**No `php artisan tinker`.** There's no Rust equivalent to a live REPL
against your app's own models yet - see [FAQ](../faq) for why, and what to
reach for instead (mostly: a real integration test via
[`TestClient`](../testing)).

**No Artisan-style named command registry.** `Artisan::command('name',
$closure)` has no Larust equivalent - `routes/console.rs` is specifically
for [task scheduling](../digging-deeper/events-queues-and-scheduling)
declarations, not a general "register a CLI command by name" mechanism.
Deliberately out of scope for now; see
[docs/ARCHITECTURE.md](https://github.com/wallabydesigns/Larust/blob/main/docs/ARCHITECTURE.md)
for the reasoning if you want the full context.

**No localization / lang files.** There's no `__('messages.welcome')`,
no `resources/lang/`, no locale-negotiation middleware. If your app needs
this today, you're on your own for now - it's a real, open gap, not a
deliberately-rejected feature.

**No arbitrary multi-disk storage or multi-guard auth.** `storage::local()`/
`storage::public()` are two fixed disks, not a config-driven registry you
can add a third named disk to. Auth is single-guard (one `Authenticatable`
type per app) - there's no `guard('admin')` concept for running two
independent auth systems side by side.

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
