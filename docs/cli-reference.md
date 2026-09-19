---
title: CLI Reference
nav_order: 8
---

# The `xr` CLI
{: .no_toc }

`xr` is Larust's `artisan`. Every command below also works as `cargo run
-p larust-cli -- <command>` from inside the workspace checkout, if you'd
rather not install it globally (see [Installation](../getting-started/installation)).

1. TOC
{:toc}

## Project scaffolding

### `xr new [path] [--auth] [--tauri] [--features <list>] [--workspace <path>]`

Create a new app. Omit `path` for an interactive wizard that asks for
everything below one question at a time instead.

| Flag | Meaning |
|---|---|
| `--auth` | Scaffold session-based auth (User model, register/login/logout, guarded routes) |
| `--tauri` | Also scaffold `src-tauri/` (a native desktop build) - see [Deployment & Desktop Apps](../deployment-and-desktop-apps#tauri-desktop-apps) |
| `--features <a,b,c>` | Enable optional `larust-support` features: `db`, `permissions`, `reverb`, `sanctum`, `shield`, `sitemap`, `socialite` |
| `--workspace <path>` | Point at a Larust checkout explicitly, if `path` isn't inside one already |

### `xr add tauri`

Retrofit Tauri desktop support onto an already-scaffolded app, run from
inside it - the only `xr add` target today (the other optional features
splice fixed text into `main.rs`/`routes/*.rs` only at generation time,
so retrofitting them safely onto an already-hand-edited file isn't
attempted).

## Generators (`make:*`)

| Command | Generates |
|---|---|
| `xr make:controller <Name> [--resource]` | A controller; `--resource` adds all 7 RESTful methods |
| `xr make:model <Name> [--migration]` | A model; `--migration` also writes a matching `CREATE TABLE` migration |
| `xr make:request <Name>` | A `#[derive(FormRequest)]` struct |
| `xr make:middleware <Name>` | A middleware function skeleton |
| `xr make:policy <Name> [--user <Type>]` | A `Policy<U>` impl skeleton (`--user` if your `Authenticatable` isn't `User`) |
| `xr make:migration <name>` | An empty, correctly-numbered migration file |
| `xr make:command <Name>` | A named CLI command (`larust_support::console::Command`) - see [Named CLI Commands](../digging-deeper/events-queues-and-scheduling#named-cli-commands) |

## Running your app

### `xr dev [--port <port>]`

Watches your app, rebuilds and hot-swaps it on save with zero dropped
requests, and pushes a live-reload signal to any open browser tab. Binds
the port immediately (serving a build-status page until the first build
finishes), and leaves the last known-good build running if a later one
fails. See [Your First App](../getting-started/your-first-app#iterate-with-xr-dev).

### `xr build [--fresh]`

Builds (or rebuilds) an app's frontend assets standalone (`npm run
build`, the same step `xr deploy` runs before publishing) - a no-op if
the app has no `node_modules/`. `--fresh` clears Vite's dependency cache
and `public/build/` first, for when a stuck build needs a clean slate.

### `xr route:list`

Prints every registered route (method, path, name).

### `xr command:list`

Prints every registered [named command](../digging-deeper/events-queues-and-scheduling#named-cli-commands)'s
name and description.

### `xr <your-command-name>`

Any name that doesn't match one of the fixed subcommands on this page is
looked up against your own `routes/console.rs::commands()` registry - see
[Named CLI Commands](../digging-deeper/events-queues-and-scheduling#named-cli-commands).
A name matching nothing at all is a loud error, not a silent fall-through
into starting the web server.

### `xr restart`

Asks an already-running app to perform a zero-downtime restart handoff -
a new process takes over the listening socket before the old one starts
draining, so in-flight requests finish and no new connection is ever
refused.

### `xr list`

Lists every `xr dev` session currently running on this machine - PID, app
name, port, uptime, and directory. Useful once you've got more than one
app's `xr dev` going at the same time and need to tell them apart.

### `xr kill [--id <pid>]`

Stops the `xr dev` session tied to the current directory: sends the
currently-running server the same graceful `STOP` signal `xr dev`'s own
Ctrl+C handler uses, then ends the `xr dev` watcher process itself. With
`--id <pid>` (the id `xr list` prints), stops a specific session instead,
regardless of which directory you run it from.

## Database

### `xr migrate`

Runs every pending migration in `database/migrations/`, in order.

### `xr migrate:fresh`

Drops every table (except the framework's own `sessions` table) and
reapplies every migration from scratch - the honest alternative to a
rollback this framework can't actually offer (migrations have no
`down()`). See [Migrations & the Query Builder](../database/migrations-and-query-builder#no-rollback---xr-migratefresh-instead).

### `xr db:list` / `xr db:get <key>` / `xr db:put <key> <value>` / `xr db:forget <key>`

Manage the embedded key-value store from the command line (requires the
`db` optional feature). `db:put`'s value is parsed as JSON when possible
(numbers, booleans, quoted strings), otherwise stored as a plain string.

## Background work

### `xr queue:work`

Starts a worker that claims and processes queued jobs until stopped. See
[Events, Queues, Scheduling & Commands](../digging-deeper/events-queues-and-scheduling#queues).

### `xr schedule:work`

Runs due scheduled tasks once a second until stopped.

{: .warning }
Not safe to run as more than one process against the same app, unless a
task explicitly opts into `.on_one_server()`. See [Task
Scheduling](../digging-deeper/events-queues-and-scheduling#task-scheduling).

## Deployment & lifecycle

### `xr deploy [--run]`

Builds and publishes a release according to `DEPLOY_TYPE` (`.env`, `"web"`
by default): `cargo build --release`, publish to `storage/releases/`,
then the same zero-downtime restart handoff `xr restart` uses against
whatever's already running. `DEPLOY_TYPE=app` instead runs `cargo tauri
build` from `src-tauri/`. `--run` cold-starts the freshly published
release in the background if nothing is currently running to hand off to
(the very first deploy) - no effect otherwise. See [Deployment & Desktop
Apps](../deployment-and-desktop-apps).

### `xr upgrade [--force]`

Upgrades the `xr` binary itself (not your app - see `xr update` for
that) by pulling and reinstalling from the checkout it was built from:
fetches, fast-forwards the current branch only if there are new upstream
commits, then reinstalls. Does nothing if already up to date. `--force`
skips the freshness check and just reinstalls from the checkout's
current state - the "repair" equivalent of re-running `install.sh`/
`install.ps1`.

### `xr update`

Updates the current *app's* Cargo dependencies within their declared
version constraints (`composer update`'s equivalent) - not `xr` itself.

### `xr audit`

Runs `cargo-audit` over the resolved workspace lockfile, checking for
known security advisories.

## Converting an existing Laravel app

### `xr convert <path> --out <dir>`

Converts a whole Laravel app into a fresh, empty `<dir>` - refuses to run
if `<dir>` already exists and isn't empty (no incremental/merge support;
re-running on an already-converted, hand-edited project needs a new
directory). See [Converting a Laravel App](../converting-a-laravel-app).

### `xr convert --file <blade-path> --destination <xr-path>`

Re-converts a single `.blade.php` template in isolation (overwriting
`<xr-path>` if it exists) - for pulling one template through a converter
fix without redoing the whole project.

## Next

[Testing](../testing) covers `TestClient` and the rest of `larust-testing`.
