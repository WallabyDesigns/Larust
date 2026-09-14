---
title: Home
layout: home
nav_order: 1
permalink: /
---

<div class="lr-hero">
  <h1 class="lr-display">Larust</h1>
  <p class="lr-lead">A Laravel-shaped web framework for Rust. Real, compiled, type-checked Rust underneath - the directory layout, routing style, validation, templates, and CLI a Laravel developer already knows.</p>
  <div class="lr-hero-actions">
    <a class="lr-button" href="getting-started/installation">Get started</a>
    <a class="lr-button-secondary" href="https://github.com/Costigan-Stephen/Larust">View on GitHub</a>
  </div>
  <div class="lr-feature-grid">
    <div class="lr-feature">
      <strong>Familiar, on purpose</strong>
      <p><code>app/Http/Controllers</code>, <code>routes/web.rs</code>, Blade-flavored templates, an <code>xr</code> CLI shaped like <code>artisan</code>.</p>
    </div>
    <div class="lr-feature">
      <strong>Real Rust underneath</strong>
      <p>Axum, sqlx, tower-sessions - no interpreter, no magic methods, no runtime you have to trust to catch your mistakes.</p>
    </div>
    <div class="lr-feature">
      <strong>Reactive, without a build step</strong>
      <p><code>@wire(...)</code> components and <code>@live(...)</code> server push - Livewire-shaped, with a vendored, dependency-free client runtime.</p>
    </div>
  </div>
</div>

---

## The pitch

Open a generated Larust app and you should recognize almost everything if
you've ever worked in Laravel: the directory layout, the routing style,
form validation, Blade-flavored templates, the ORM's vocabulary, the `xr`
command-line tool. The names are deliberately familiar.

What's underneath is not. There is no PHP, no interpreter, no `$this`, no
magic methods, and no runtime you have to trust to catch your mistakes for
you. Every route, every model, every validated field is real, compiled,
type-checked Rust - built on [Axum](https://github.com/tokio-rs/axum) for
HTTP, [sqlx](https://github.com/launchbadge/sqlx) for the database, and
[tower-sessions](https://github.com/maxcountryman/tower-sessions) for
sessions. If it compiles, whole categories of runtime surprise Laravel
developers are used to bracing for - a typo'd array key, a `null` where an
object was expected, a route that silently 500s in production because a
relationship wasn't eager-loaded - simply cannot happen the same way.

Larust is for two audiences at once, and it's written to be legible to
both:

- **Coming from Laravel?** Read [Coming from Laravel](coming-from-laravel)
  first - it maps everything you already know onto its Larust equivalent,
  and is explicit about the handful of things that are deliberately
  *not* carried over (magic, mostly).
- **Coming from Rust?** Read [Coming from Rust](coming-from-rust) first -
  it explains why this framework makes the opinionated choices it does
  (a batteries-included web framework, not a pile of composable crates
  you assemble yourself), and where the Laravel-shaped naming might throw
  you if you already know Axum or sqlx directly.

Either way, [Getting Started](getting-started/installation) is the fastest
path to a real, running app.

## A map, if you already know Laravel

| Laravel | Larust |
|---|---|
| `php artisan` | `xr` |
| `composer create-project laravel/laravel` | `xr new` |
| `routes/web.rs` / `routes/api.rs` | `routes/web.rs` / `routes/api.rs` (same idea, real Rust) |
| `Route::get(...)->name(...)` | `Route::get(...).name(...)` |
| `app/Http/Controllers` | `app/Http/Controllers` |
| Form Requests (`$request->validated()`) | `#[derive(FormRequest)]` + `request.validated()` |
| Eloquent models | `#[derive(Model)]` + `QueryBuilder` |
| `hasMany`/`belongsTo`/`belongsToMany` | `#[has_many(...)]`/`#[belongs_to(...)]`/`#[belongs_to_many(...)]` |
| Blade (`.blade.php`) | `.blade.xr` (parsed by `view!`, not a runtime template engine) |
| Policies | `Policy<U>` + `authorize()` |
| Sanctum | `larust-sanctum` |
| Jobs / Queues | `larust-queue` (SQLite or Redis) |
| Task Scheduling | `larust-scheduler` |
| Notifications | `larust-notifications` |
| Events & Listeners | `larust-events` |
| Livewire | `@wire(...)` components (`larust-live`) |
| Broadcasting | `@live(...)` + `larust-reverb` |
| `php artisan tinker` | not yet - see [FAQ](faq) |
| `.env` | `.env` (same format, same idea) |

See [Coming from Laravel](coming-from-laravel) for the full, honest
version of this table - including what's different on purpose.

## What's actually in the box

- **Routing** - a `Route`/`Router` DSL over Axum, named routes, resource
  routing, group-scoped middleware, route model binding.
- **Validation** - `#[derive(FormRequest)]`, 422 JSON responses before
  your handler ever runs.
- **Templates** - a from-scratch Blade-inspired parser (`.blade.xr`,
  `@extends`/`@section`/`@if`/`@foreach`/`@push`/`@stack`, components,
  layouts) compiled to real Rust at build time, not interpreted per request.
- **The ORM** - `#[derive(Model)]`, a `QueryBuilder`, migrations, and every
  relationship kind with eager loading, over SQLite, MySQL, Postgres, and
  (partially) SQL Server.
- **Auth** - password hashing, session guards, `Auth<U>`, `Policy<U>`,
  API tokens (`larust-sanctum`), roles/permissions, social login
  (`larust-socialite`).
- **Reactivity, without a SPA build step** - `@wire(...)` components
  (Livewire-shaped, server-state-backed) and `@live(...)` for genuine
  server-pushed real-time updates, both with a vendored, dependency-free
  client runtime.
- **Everything else Laravel apps end up needing** - mail, notifications,
  events, queues (SQLite or Redis), a scheduler, file storage, caching
  (SQLite or Redis), an embedded key-value store with an admin dashboard,
  sitemaps, and a plugin trait for packaging your own routes.
- **The `xr` CLI** - `new`, `dev` (rebuild + restart + browser auto-reload
  on save, zero downtime), `deploy`, `make:*` generators, `route:list`,
  `migrate`, `queue:work`, `schedule:work`, `convert` (bring an existing
  Laravel app over), `audit`, `upgrade`.

See the [CLI reference](cli-reference) and the [Digging
Deeper](digging-deeper/) section for the full tour, or
[docs/ARCHITECTURE.md](https://github.com/Costigan-Stephen/Larust/blob/main/docs/ARCHITECTURE.md)
in the repository for the engineering-diary-level detail behind every one
of these decisions.

## Status

Every milestone in the project's history is implemented, covered by
tests, and has been through an independent review pass - see
[docs/MILESTONES.md](https://github.com/Costigan-Stephen/Larust/blob/main/MILESTONES.md)
in the repository for the full, chronological build log. This site is the
reference documentation; that file is the changelog.

Larust isn't published to crates.io yet, so every generated app currently
resolves the framework's own crates as local path dependencies against a
checkout of this repository - see [Installation](getting-started/installation)
for exactly what that means in practice.
