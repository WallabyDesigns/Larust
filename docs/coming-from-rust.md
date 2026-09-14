---
title: Coming from Rust
nav_order: 4
---

# Coming from Rust
{: .no_toc }

If you already know Axum, sqlx, and tokio, Larust will look familiar in
places and oddly-named in others. This page explains the trade-offs it
makes and why - so the Laravel-shaped naming reads as a deliberate
decision rather than an unfamiliar convention bolted onto tools you
already trust.

1. TOC
{:toc}

## It's Axum, sqlx, and tower-sessions underneath - not a reimplementation

Larust doesn't replace the Rust web ecosystem, it composes it under a
Laravel-shaped API:

- **HTTP** is [Axum](https://github.com/tokio-rs/axum). `Router` wraps
  `axum::Router`; `.into_axum_router()` gets you the real thing back at
  any point, and every handler is a normal Axum handler function using
  normal Axum extractors (`larust_http::session::Session` is a real
  `FromRequestParts` impl, same shape as any other Axum extractor you've
  written).
- **The database** is [sqlx](https://github.com/launchbadge/sqlx), via
  `sqlx::AnyPool` so the same generated model code runs against SQLite,
  MySQL, or Postgres (SQL Server, via `tiberius`, is deliberately kept
  outside this shared path - see [Models &
  Relationships](../database/models-and-relationships)). `larust_support::orm::pool()`
  hands you the real pool if you ever want to drop to a raw
  `sqlx::query(...)`.
- **Sessions** are [tower-sessions](https://github.com/maxcountryman/tower-sessions),
  with a hand-written `SessionStore` over `AnyPool` (see
  [Middleware, Sessions & CSRF](../the-basics/middleware-sessions-and-csrf)
  for why a third-party per-backend store crate doesn't fit here).
- **Async runtime** is plain tokio. Nothing here has its own executor or
  its own concurrency primitives.

You are never locked out of the underlying crate. If something Larust
doesn't wrap yet is easier to reach directly, reach for it directly -
that's expected, not a workaround.

## Why the Laravel-shaped naming

A Rust-idiomatic web framework would probably call this
`.route_layer(middleware)` or `.nest(...)`. Larust calls the equivalent
`.middleware(...)` and `.group(...)` on purpose - the API surface is
designed to be legible to a Laravel developer first, a Rust developer
second. If a method name reads slightly off compared to what you'd expect
from Axum or another idiomatic Rust crate, that's very likely why. The
underlying types and behavior are still exactly what you'd expect from
real Rust: no hidden dynamic dispatch, no runtime string-keyed lookup,
just names chosen for a different, cross-language audience.

## The proc-macros generate real code, not runtime reflection

Three derive/function-like macros do almost all of the "feels like magic"
work, and all three are ordinary `proc-macro2`/`syn`/`quote` code
generation, checked by `rustc` like anything else you'd write by hand:

- **`#[derive(Model)]`** generates `create`/`find`/`update`/`delete`,
  relationship accessor methods, and a real `sqlx::FromRow` impl from your
  struct's fields and attributes (`#[has_many(...)]`, etc.) - see
  [Models & Relationships](../database/models-and-relationships).
- **`#[derive(FormRequest)]`** generates a `validated()` method and an
  Axum `FromRequest` impl that runs validation and returns a 422 *before*
  your handler body ever executes - see [Controllers &
  Requests](../the-basics/controllers-and-requests).
- **`view!("template.name", { field, field2 })`** parses a `.blade.xr`
  file at compile time and expands to a plain Rust function body that
  builds a `String` - there is no template *interpreter* running per
  request. A template referencing a field you didn't pass, or a directive
  that doesn't parse, is a `cargo build` failure, not a 500 at 2am.

If you want to see exactly what any of these expand to,
[`cargo expand`](https://github.com/dtolnay/cargo-expand) works on
generated app code the same way it does on any other proc-macro. Nothing
here is off-limits to normal Rust tooling.

## Where this framework is deliberately opinionated

Unlike a minimal toolkit (raw Axum + whatever else you assemble), Larust
makes real choices for you, the same way Laravel does for PHP:

- **One database connection pool per process**, via a `OnceLock`-backed
  `larust_orm::pool()` - not something you construct and pass around.
  This is a real, documented constraint (see
  [`docs/GOTCHAS.md`](https://github.com/wallabydesigns/Larust/blob/main/docs/GOTCHAS.md)'s
  entry on it), and it's why [testing](../testing) needs its own
  `test_transaction`/`test_db` machinery instead of just constructing a
  second pool inline.
- **Sessions are DB-backed by design, with no in-memory option in the
  public API** - an in-memory store that quietly logs everyone out on
  every deploy is treated as a real footgun worth removing, not a
  configuration choice worth offering.
- **Migrations are forward-only.** No `down()`, no rollback story to
  half-support. See [Migrations & the Query
  Builder](../database/migrations-and-query-builder).
- **A `#[derive(FormRequest)]`'s 422 always returns JSON, unconditionally** -
  not something you toggle per route. If you need a different failure
  shape, you're expected to reach for it explicitly rather than flip a
  framework setting.

If you'd rather assemble each of these decisions yourself, that's a
reasonable choice - it's just a different framework. Larust's bet is that
most apps want Laravel's defaults, not another decision to make.

## What Rust gets you that Laravel apps don't have

- **A route that references an undefined controller method doesn't
  compile.** Neither does a template referencing a field your handler
  never passed, or a model relationship attribute pointing at a type that
  doesn't implement the trait it needs.
- **`Send`/`Sync` bounds are real and checked**, including across
  `async fn` in traits (`Job::handle`, `Notification::via`) - if your
  implementation genuinely isn't `Send`, you find out at compile time, not
  under production load the first time two requests interleave badly.
- **No runtime type coercion.** A validated `i64` form field is an `i64`,
  not a string that happens to look like a number until it doesn't.

## Where to look for the engineering detail

This site (`/docs` in the repository) is the user-facing reference.
Alongside it, three files written for framework *contributors* go much
deeper on the "why," including every non-obvious bug this codebase has
hit and how it was fixed:

- [`ARCHITECTURE.md`](https://github.com/wallabydesigns/Larust/blob/main/docs/ARCHITECTURE.md) -
  crate-by-crate design rationale.
- [`GOTCHAS.md`](https://github.com/wallabydesigns/Larust/blob/main/docs/GOTCHAS.md) -
  every sharp edge found so far (Rust-specific, Windows-specific, and
  Cargo-specific alike), and its fix.
- [`MILESTONES.md`](https://github.com/wallabydesigns/Larust/blob/main/MILESTONES.md) -
  the full, chronological build log.

They're written for someone reading the source, not someone building an
app - but if you want to know *why* a particular API looks the way it
does, they almost always say so explicitly.

## Next

[Getting Started](../getting-started/installation) to build something real,
or [The Basics](../the-basics/) to see the request lifecycle end to end.
