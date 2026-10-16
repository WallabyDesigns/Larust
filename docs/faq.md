---
title: FAQ
nav_order: 13
---

# FAQ
{: .no_toc }

1. TOC
{:toc}

## Is Larust affiliated with Laravel?

No. Larust is an independent, community project inspired by Laravel's
application structure and developer experience, reimplemented from
scratch in Rust. It is not affiliated with, endorsed by, or sponsored by
Laravel LLC, Taylor Otwell, or the Laravel project. "Laravel" is a
trademark of its respective owner; Larust uses the name only to describe
the design it's inspired by, the same way many language ports of other
frameworks do.

## Why does `xr --version` show a commit hash, not just a version number?

It shows both: `0.5.0 (<commit hash>)`. The version is `larust-cli`'s own
published crates.io version - what `xr upgrade`'s new crates.io-based
default actually upgrades against. The commit hash exists for `xr
upgrade --legacy` (an `xr` built from a real checkout via `install.sh`/
`install.ps1`/`cargo install --path`, not `cargo install larust-cli`):
it tells you precisely which source revision your binary contains, and
`--legacy` compares it against the checkout's current `HEAD` to decide
whether there's anything new to pull. An `xr` installed via plain
`cargo install larust-cli` has no checkout at all, so its own commit
hash just reads `unknown` - harmless, since `--legacy` isn't the mode
that install is meant to use anyway.

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

If `APP_PORT` isn't set at all, `APP_URL`'s own port is used before
falling back to `34187` - `APP_URL=http://127.0.0.1:8000` resolves to
port `8000`, not `34187`. This matters most for `xr convert`: a real
Laravel `.env` commonly sets `APP_URL` but never `APP_PORT` (Artisan's own
`serve` command never reads `APP_URL` for its `--port` default, so a
Laravel project never needed the two kept in sync), and `xr convert` only
ever carries over the source app's real `.env` values - it never invents
an `APP_PORT` line the original didn't have. Without this leniency, a
converted app would silently land on `34187` instead of the port its own
`APP_URL` already, if only incidentally, documented.

## I've got several `xr dev` sessions running and can't tell them apart - how do I stop just one?

`xr list` shows every `xr dev` session currently running on the machine -
PID, app name, port, uptime, and directory. `xr kill --id <pid>` (the id
`xr list` just printed) stops that one specifically, wherever you run it
from. Run `xr kill` with no arguments from inside a project's own
directory to stop whatever's tied to *that* app without needing its id at
all. See [CLI Reference](../cli-reference#xr-list) for both.

This is also the fix for a stale generation left over from a closed
terminal or IDE window (nothing ties a spawned child's lifetime to the
console that started it on Windows) - `xr kill --id <pid>` ends it
directly instead of hunting for it in Task Manager.

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
said, this is a young, single-maintainer framework: there's no
i18n/localization story, and some areas (SQL
Server support, multi-instance scaling for sessions specifically) are
documented as partial rather than complete - see [Coming from
Laravel](../coming-from-laravel#what-isnt-here-yet-or-at-all) for the honest
list. Evaluate it the way you'd evaluate any pre-1.0 framework: read
[`GOTCHAS.md`](https://github.com/wallabydesigns/Larust/blob/main/docs/GOTCHAS.md)
for what's already been found and fixed, and expect to find a few more
things yourself.

## Why does a new app need a full clone of this repository?

Every Larust crate is published to crates.io now, but `xr new`/`xr
convert` haven't finished migrating to it yet - a generated app still
resolves the framework's own crates as local path dependencies pointing
back into a real checkout, not `version = "..."` dependencies against the
published crates. See
[Installation](../getting-started/installation#clone-the-repository) for
what that means day to day.

## Where do I report a bug or ask a question?

[Open an issue on GitHub](https://github.com/wallabydesigns/Larust/issues).
Include your `xr --version` output (the commit hash matters more than
you'd think) and, if it's a platform-specific issue, which OS you're on -
this framework has already found a meaningful number of genuinely
platform-specific bugs (Windows, Linux, and macOS alike), so the platform
is real, relevant information.
