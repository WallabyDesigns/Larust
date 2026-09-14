---
title: Architecture Overview
nav_order: 12
---

# Architecture Overview
{: .no_toc }

A lighter-weight tour of how Larust fits together, for anyone curious
what's under the hood. For the full engineering-diary-level detail behind
every decision here - including every non-obvious bug this codebase has
hit along the way - see
[`docs/ARCHITECTURE.md`](https://github.com/wallabydesigns/Larust/blob/main/docs/ARCHITECTURE.md)
in the repository.

1. TOC
{:toc}

## One dependency surface

Every generated app's `Cargo.toml` depends on `larust-core`,
`larust-http`, `larust-support` (plus `tokio` and `sqlx`), and nothing
else framework-related. `larust-support` is a **facade crate**: it
depends on every other Larust crate and re-exports exactly what app code
and macro-generated code need, under one consistent `larust_support::...`
path.

This isn't just tidiness - it's load-bearing for the proc-macros.
`#[derive(Model)]`/`#[derive(FormRequest)]`/`view!` generate code that
gets spliced into *your app's* crate, not `larust-macros`'s own crate, so
every fully-qualified path in generated code has to resolve from your
app's dependency graph. Every path in generated code is routed through
`larust_support`, with no exceptions - which is what makes "just depend
on `larust-support`" actually true rather than aspirational.

```text
                     ┌──────────────────┐
                     │  larust-support   │  ← apps depend on this
                     │   (the facade)    │
                     └────────┬──────────┘
          ┌──────────┬────────┼────────┬───────────┬─────────────┐
          │          │        │        │           │             │
    larust-core  larust-http  │  larust-orm  larust-validation  larust-view
                               │
                        larust-macros  ← proc-macros; generates code
                                          referencing ::larust_support::...
```

## Crate map

| Crate | Responsibility |
|---|---|
| `larust-core` | `Application` bootstrap, config, logging, `AppError`, error pages, zero-downtime lifecycle |
| `larust-http` | `Route`/`Router` DSL, sessions, CSRF, middleware, plugins |
| `larust-orm` | `QueryBuilder`, connection pool, migrations over `sqlx::AnyPool` |
| `larust-validation` | `ValidationErrors`, the runtime side of `#[derive(FormRequest)]` |
| `larust-view` | The `.blade.xr` parser - directives, expressions, layouts |
| `larust-macros` | Every proc-macro: `Model`, `FormRequest`, `view!` |
| `larust-auth` | Password hashing, `Authenticatable`, guards, `Auth<U>`, `Policy<U>` |
| `larust-sanctum` / `larust-permissions` / `larust-socialite` | API tokens / roles / OAuth login |
| `larust-mail` / `larust-notifications` | `Mailable`, database notifications |
| `larust-events` / `larust-queue` / `larust-scheduler` | Pub/sub, durable jobs, cron-style scheduling |
| `larust-cache` / `larust-storage` | Key-value caching, two fixed filesystem disks |
| `larust-live` | `@wire` reactive components + `@live` server push |
| `larust-reverb` | A generic WebSocket pub/sub server (Laravel Reverb's counterpart) |
| `larust-spa` | `@spa` fetch-and-swap navigation |
| `larust-db` | Embedded key-value store + the `/xr-db` SQL admin dashboard |
| `larust-sitemap` | `sitemap.xml` generation helpers |
| `larust-mssql` | SQL Server support, outside `AnyPool` (see [Database](../database/)) |
| `larust-repository` | The storage-agnostic `Repository<T>` contract every SQL-family backend implements |
| `larust-convert` | The `xr convert` Laravel-to-Larust tool, built on `tree-sitter-php` |
| `larust-testing` | `TestClient`, `test_db`/`test_transaction`, `Mail::fake()` |
| `larust-support` | The facade - what every generated app actually depends on |
| `larust-cli` | The `xr` binary |

## Design principles worth knowing

- **Compile error over silent gap.** Every core trait
  (`Mailable`/`Job`/`Authenticatable`/`Notification`/`Policy<U>`) has zero
  default methods where Laravel's PHP equivalent would have an optional
  one - a missing implementation is a build failure, not a runtime
  no-op. `xr convert` follows the same instinct: an unsupported construct
  is flagged loudly, never silently guessed at.
- **Process-wide singletons for the things that genuinely are.** One
  database pool, one config, per process - via `OnceLock`, mirroring how
  a real app actually runs. This is why [testing](../testing) needs its own
  task-local pool-override machinery rather than just constructing a
  second pool inline.
- **No dynamic runtime registries.** Middleware is a function reference,
  not a string alias; plugins are a compiler-verified trait, not a
  runtime plugin loader; role/permission names are your own enum, not a
  string. Nothing in this framework does reflection-based resolution,
  because Rust doesn't have reflection to do it with.
- **Real code, real tests, real review.** Every milestone in this
  project's history shipped with tests and an independent review pass -
  see [`MILESTONES.md`](https://github.com/wallabydesigns/Larust/blob/main/MILESTONES.md)
  for the full, chronological record, and
  [`GOTCHAS.md`](https://github.com/wallabydesigns/Larust/blob/main/docs/GOTCHAS.md)
  for every non-obvious bug found along the way and its actual fix - not
  just the ones that were convenient to write up.

## Next

[FAQ](../faq) for quick answers, or back to [Home](../) for the full map.
