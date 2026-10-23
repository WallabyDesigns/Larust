---
title: Optional Features
parent: Digging Deeper
nav_order: 10
---

# Optional Features
{: .no_toc }

Every optional Tier-1 shim crate in one place - what it is, what Laravel
package it stands in for, and where its full documentation lives. Each
one is opt-in: pick it with `xr new --features <name>` (comma-separated
for more than one), the interactive wizard (bare `xr new`), or by adding
it directly to `larust-support`'s own `features = [...]` list in an
existing app's `Cargo.toml`.

1. TOC
{:toc}

## Why opt-in

Each of these stands in for a genuinely optional third-party Laravel
package (`laravel/sanctum`, `spatie/laravel-permission`, ...), not core
framework surface - an app that never uses one shouldn't pay to compile
it. `larust-support`'s own `Cargo.toml` is the single source of truth for
the exact list and how each feature wires into it; this page exists so
there's one readable place to find them all, since nothing else in these
docs rounds every one of them up on its own.

## The full list

| Feature | Laravel equivalent | What it does | Full docs |
| --- | --- | --- | --- |
| `db` | - | Embedded key-value store (`redb`, pure Rust) plus a phpMyAdmin/Adminer-style SQL admin dashboard | [Key-Value Store & Sitemaps](key-value-store-and-sitemaps) |
| `passport` | `laravel/passport` | A full OAuth2 authorization server - Authorization Code + mandatory PKCE, Client Credentials, Refresh Token rotation | [OAuth2 Authorization Server](oauth2-authorization-server) |
| `pennant` | `laravel/pennant` | Persisted, scope-aware boolean feature flags | [Feature Flags & Search](feature-flags-and-search) |
| `permissions` | `spatie/laravel-permission` | Roles & permissions, plus `@can`/`@role` template directives | [Authentication & Authorization](authentication-and-authorization#roles--permissions-larust-permissions) |
| `reverb` | `laravel/reverb` | Generic WebSocket pub/sub broadcasting | [Realtime & Broadcasting](realtime-and-broadcasting#larust-reverb---a-generic-websocket-pubsub-server) |
| `sanctum` | `laravel/sanctum` | API bearer-token authentication for non-browser clients | [Authentication & Authorization](authentication-and-authorization#api-tokens-larust-sanctum) |
| `scout` | `laravel/scout` | Full-text search against a Meilisearch instance | [Feature Flags & Search](feature-flags-and-search) |
| `shield` | `bezhansalleh/filament-shield` | Resource-scoped permission bundles on top of `permissions` | [Authentication & Authorization](authentication-and-authorization#resource-scoped-permission-bundles-larust-shield) |
| `sitemap` | `spatie/laravel-sitemap` | XML sitemap builder | [Key-Value Store & Sitemaps](key-value-store-and-sitemaps#sitemaps-larust-sitemap) |
| `socialite` | `laravel/socialite` | OAuth "Sign in with GitHub/Google" social login | [Social Login & Plugins](social-login-and-plugins#social-login-larust-socialite) |

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

## Next

[Key-Value Store & Sitemaps](key-value-store-and-sitemaps),
[Feature Flags & Search](feature-flags-and-search), and
[OAuth2 Authorization Server](oauth2-authorization-server) cover the
features with no existing home elsewhere in these docs - or jump to
[Authentication & Authorization](authentication-and-authorization) for
`permissions`/`sanctum`/`shield`.
