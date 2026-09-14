---
title: FAQ
nav_order: 13
---

# FAQ
{: .no_toc }

1. TOC
{:toc}

## Why does `xr --version` show a commit hash, not a version number?

This workspace's `Cargo.toml` has stayed at `0.1.0` through every
milestone so far - there's no semantic-versioning discipline being
practiced yet, so a version number wouldn't actually tell you anything
true about freshness. The commit hash your `xr` binary was built from
does: `xr --version` prints it, and `xr upgrade` compares it against your
checkout's current `HEAD` to decide whether there's anything new to pull.

## Is there a `php artisan tinker` equivalent?

Not yet. There's no live REPL against your app's own models today. The
practical substitute is a real integration test via
[`TestClient`](../testing) - it gives you the same "poke at my app
interactively" workflow, just written down and repeatable instead of
typed at a prompt.

## Why is `app/Providers/` usually empty?

Laravel's service providers exist to register bindings into the service
container at boot. Larust has no service container and no runtime
dependency resolution - everything you use, you call or construct
explicitly (see [Coming from Rust](../coming-from-rust#where-this-framework-is-deliberately-opinionated)).

The folder exists for directory-layout familiarity; there's simply
nothing that needs to go in it for most apps. If you find yourself
wanting one, a plain function called once from `main.rs`/`lib.rs`'s own
boot sequence is the direct equivalent.

## Why port 34187, not 8000?

Two reasons, one practical and one not: 8000 is one of the most
commonly-already-taken ports on a real dev machine, and `34187` loosely spells "WALBY" (Wallaby) - a small nod to Wallaby Designs, the author of this framework baked in on purpose. Override it
per-run with `xr dev --port <port>`, or permanently via `APP_PORT` in
`.env`.

## Can I use MySQL or Postgres in production?

Yes - `DB_CONNECTION=mysql`/`mariadb`/`pgsql` in `.env`, plus the
matching `sqlx` feature in your `Cargo.toml` (see
[Database](../database/#configuration)). Every real model/query path is
already backend-agnostic through `sqlx::AnyPool`; SQLite is just the
zero-setup default for local dev, not a hard limitation.

## Is Larust ready for production use?

Every milestone shipped so far is implemented, tested, and independently
reviewed - and the [zero-downtime deploy](../deployment-and-desktop-apps)
machinery specifically is verified end to end under real traffic. That
said, this is a young, single-maintainer framework: it isn't published to
crates.io, there's no i18n/localization story, and some areas (SQL
Server support, multi-instance scaling for sessions specifically) are
documented as partial rather than complete - see [Coming from
Laravel](../coming-from-laravel#what-isnt-here-yet-or-at-all) for the honest
list. Evaluate it the way you'd evaluate any pre-1.0 framework: read
[`GOTCHAS.md`](https://github.com/wallabydesigns/Larust/blob/main/docs/GOTCHAS.md)
for what's already been found and fixed, and expect to find a few more
things yourself.

## Why does a new app need a full clone of this repository?

Larust isn't published to crates.io (or anywhere else) yet - every
generated app resolves the framework's own crates as local path
dependencies pointing back into a real checkout. See
[Installation](../getting-started/installation#clone-the-repository) for
what that means day to day.

## Where do I report a bug or ask a question?

[Open an issue on GitHub](https://github.com/wallabydesigns/Larust/issues).
Include your `xr --version` output (the commit hash matters more than
you'd think) and, if it's a platform-specific issue, which OS you're on -
this framework has already found a meaningful number of genuinely
Windows-only and Linux-only bugs, so the platform is real, relevant
information.
