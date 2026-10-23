---
title: Optional Features
nav_order: 8
has_children: true
---

# Optional Features
{: .no_toc }

Every optional Tier-1 shim crate, each with its own page covering
everything there is to know about it. This page is just the index - how
to add any of them, and where to find each one.

1. TOC
{:toc}

## Why opt-in

Each feature below stands in for a genuinely optional third-party
Laravel package (`laravel/sanctum`, `spatie/laravel-permission`, ...),
not core framework surface - an app that never uses one shouldn't pay to
compile it. `larust-support`'s own `Cargo.toml` is the single source of
truth for the exact list and how each feature wires into it.

## How to add one

Three equivalent ways to turn a feature on, whether you're starting a
new app or retrofitting an existing one:

```bash
# 1. At scaffold time - comma-separated for more than one
xr new myapp --features sanctum,permissions

# 2. The interactive wizard - asks one at a time instead of requiring
#    you to already know every flag
xr new

# 3. Retrofit onto an already-scaffolded app
xr add sanctum

# ...or bare `xr add` for a wizard over every optional feature at once
xr add
```

All three do the same thing underneath: add the feature's name to
`larust-support`'s own `features = [...]` list in your app's
`Cargo.toml`. Editing that list by hand works too - `xr add`/`xr new`
just save you from having to know the exact crate-feature wiring
yourself.

## The full list

| Feature | Laravel equivalent | Docs |
| --- | --- | --- |
| [`db`](db) | - | Embedded key-value store plus a phpMyAdmin/Adminer-style SQL admin dashboard |
| [`passport`](passport) | `laravel/passport` | A full OAuth2 authorization server |
| [`pennant`](pennant) | `laravel/pennant` | Persisted, scope-aware boolean feature flags |
| [`permissions`](permissions) | `spatie/laravel-permission` | Roles & permissions, plus `@can`/`@role` template directives |
| [`reverb`](reverb) | `laravel/reverb` | Generic WebSocket pub/sub broadcasting |
| [`sanctum`](sanctum) | `laravel/sanctum` | API bearer-token authentication for non-browser clients |
| [`scout`](scout) | `laravel/scout` | Full-text search against a Meilisearch instance |
| [`shield`](shield) | `bezhansalleh/filament-shield` | Resource-scoped permission bundles on top of `permissions` |
| [`sitemap`](sitemap) | `spatie/laravel-sitemap` | XML sitemap builder |
| [`socialite`](socialite) | `laravel/socialite` | OAuth "Sign in with GitHub/Google" social login |

`shield` implies `permissions` automatically (its own `ResourcePermission`
type is a `larust_permissions::PermissionName` underneath, so it's
meaningless on its own). Every other feature is independent - pick any
combination.

## Picking a database driver

Every feature above that touches SQL (all but `db`'s own embedded
key-value half, `reverb`, and `socialite`) shares your app's one
configured database - there's no separate driver choice to make per
feature. `xr new` defaults to `sqlite`; pass `--features mysql` or
`--features postgres` alongside whichever Tier-1 features you want to
also pick that driver instead (`xr convert` detects an existing Laravel
app's own driver and picks it for you automatically).

## Deliberately not on this list

A handful of popular Laravel ecosystem packages have no Larust
equivalent, and aren't tracked as future Tier-1 shim candidates either -
see the project's own [`rust-laravel.md`](https://github.com/wallabydesigns/Larust/blob/main/rust-laravel.md)
founding document for the full reasoning. In short: **Horizon**, **Telescope**,
**Pulse**, and **Nova**/**Filament** all need a real dashboard UI, which is a
different, much larger kind of feature than a crate-shaped add-on -
intentionally out of scope. **Cashier** (billing) and a fuller **Dusk**
(browser E2E) integration remain open, untracked candidates.
