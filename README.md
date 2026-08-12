# Larust

Larust is a Laravel-shaped web framework for Rust. The pitch: a Laravel developer should be able to open a generated project and recognize almost everything:
directory layout, routing style, validation, templates, the ORM's vocabulary,
CLI commands.

The code underneath is real, compiled, type-checked
Rust (Axum + sqlx + tower-sessions), not a PHP-flavored DSL bolted on top.

See [`rust-laravel.md`](rust-laravel.md) for the original product vision and
design rationale (why `$var` isn't realistic, why `let` isn't the enemy,
what's deliberately preserved vs. translated vs. rejected from Laravel).

## Status

**v0.1 (M0–M6) and v0.2's auth + relationships + eager-loading +
many-to-many milestones (M7–M15) are complete.** Every milestone below is
implemented, covered by tests, and has been through an independent code
review pass.

| Milestone | What it added |
|---|---|
| M0 | Workspace scaffold, `Application` bootstrap (config + logging), `xr new` |
| M1 | `Route`/`Router` DSL over Axum, named routes, `xr route:list` |
| M2 | `#[derive(FormRequest)]` validation, 422 responses before the handler runs |
| M3 | Blade-inspired `.blade.xr` templates: `@extends`/`@section`/`@yield`/`@if`/`@foreach` |
| M4 | `#[derive(Model)]` + `QueryBuilder` over sqlx (SQLite), migrations |
| M5 | Sessions, CSRF protection (`@csrf`), middleware DSL |
| M6 | Route model binding (`{post}` → `Model`), `xr make:*` generators |
| M7 | Group-scoped middleware (`Route::middleware(...)->group(...)`) |
| M8 | `larust-auth`: password hashing, `Authenticatable`, session guards, `Auth<U>`, `authorize()` |
| M9 | `xr new --auth`: scaffolded register/login/logout, `require_auth`/`redirect_authenticated` |
| M10 | `#[has_many(...)]`/`#[has_one(...)]`/`#[belongs_to(...)]` relationships on `#[derive(Model)]` |
| M11 | `Post`/`User` linked in the `--auth` reference app, dogfooding relationships |
| M12 | `QueryBuilder::where_in` |
| M13 | Batch (eager) loading: `load_*` methods for every relationship kind |
| M14 | `PostController::index` eager-loads authors — verified 2 queries, not N+1 |
| M15 | `#[belongs_to_many(...)]`: many-to-many via a pivot table, `attach`/`detach`/`sync` |
| M16 | `xr dev`: rebuild + restart on save, with browser auto-refresh via an SSE-based reload signal |
| M17 | `Route::resource(...)`: all 7 RESTful routes in one call, Laravel's naming convention |
| M18 | `APP_DEBUG`: descriptive HTML error pages (message + source chain) and panic-catching, gated off by default |
| M19 | `@elseif(...)` chains; `{{ if cond { "a" } else { "b" } }}` for conditional values |
| M20 | `public/` static asset serving — files there are served at the URL root, always on |
| M21 | `@push('name')`/`@stack('name')` — accumulating, per-page contributions to a layout spot |
| M22 | `@global(name, fallback)`/`@globals ... @endglobals` — single-value, page-overridable layout placeholders (title, canonical, etc.) |
| M23 | Persistent (SQLite-backed) sessions — replaces the in-memory store that silently lost every session on restart |
| M24 | CSRF: header-based `X-CSRF-TOKEN` fallback (checked before the body), matching Laravel's own convention and unblocking JS-driven uploads |
| M25 | `route()`/`route_with()`/`url()`/`asset()`/`config()` helpers — Laravel-style, callable directly from any `{{ }}` template interpolation |
| M26 | `Policy<U>` trait — Laravel-style per-model authorization (`view_any`/`view`/`create`/`update`/`delete`), `xr make:policy` generator |
| M27 | `larust-testing`: `TestClient`/`TestResponse`/`test_db` — in-process HTTP test client, `acting_as()` auth simulation, per-test-binary migrated database. Generated apps gain a library target so `tests/*.rs` can reach `controllers`/`models`/etc. |
| M28 | `larust-mail`: `Mailable` trait + `mail().to(...).send(...)` — `log`/`smtp` drivers, `log` is the scaffold default (no SMTP server needed for local dev or `cargo test`) |
| M29 | `larust-cache`: `cache::{put, get, forget, remember}` — single SQLite-backed driver (no in-memory option, no toggle), self-bootstrapping `cache_items` table, no setup required in generated apps |
| M30 | `larust-events`: in-process, synchronous `event::{listeners, dispatch}` pub/sub, no persistence. `larust-queue`: durable, SQLite-backed `Job`/`queue::dispatch`/`xr queue:work` worker — `failed_jobs` on error, no retries/crash recovery in v1 |
| M31 | `larust-storage`: `storage::{local, public}` — two fixed disks (`storage/app/`, `public/`), path-traversal-safe `put`/`get`/`exists`/`delete`/`url`. `UploadController` refactored onto it, fixing a latent bug where a fresh `xr new` app's `/uploads` route 500'd until `public/uploads` was created by hand |
| M32 | `Mail::fake()`/`assertSent()`/`assertNotSent()` — records rendered mail instead of dispatching (log/smtp), type-checked assertions via `std::any::type_name`, reached through `larust-testing` only (not the production `larust_support::mail` facade) |
| M33 | `larust_testing::test_transaction` — a fresh, fully isolated, freshly migrated database per call (Laravel's `RefreshDatabase`, not `DatabaseTransactions` — a real `BEGIN`/`ROLLBACK` design was tried and abandoned when it broke session-backed routes; see `docs/ARCHITECTURE.md`'s Testing section). No "one test per file" constraint, unlike every other process-wide mechanism this crate relies on |
| M34 | `larust-live`: `@live('name', { prop: expr })` reactive components — server-state-backed (session-keyed, not Livewire's client-held signed snapshot), `wire:model`/`wire:model.live`/`wire:click`/`wire:submit` support via a vendored, build-step-free client runtime with its own small DOM-patcher (`wire:ignore` opts an element out, e.g. a rich-text editor; `@loadonce ... @endloadonce` is compile-time sugar over `wire:ignore` for colocating a component's own static assets). `mount`/`call` receive the real session (per-viewer identity, not just per-component state); actions can return a redirect (`Ok(Some(path))`), not just re-render in place. `@larustscripts` (Livewire's `@livewireScripts`) auto-injects the client runtime script on any page that mounts a component, written once in the shared layout, not per-page. `demo`'s Journal (`/posts`, a `PostList` component — the listing itself, live-filtered by `wire:model.live`, with per-author Edit/Delete) and `/posts/create` + `/posts/{id}/edit` (one `PostForm` component shared by both, a `wire:submit` form with reactive validation) are the live, working examples; `/profile` (update name/email, change password) is deliberately a plain server-rendered form pair instead, matching `/login`/`/register`'s own convention |

**Not yet built** (v0.2+):
queued mail (`.queue()` — v1 `send()` is always synchronous), the Laravel
conversion tool. See [`rust-laravel.md`](rust-laravel.md)'s staged-release
section for the original plan; deviations and additions since then are
tracked in the active planning doc for the current work session.

## Quick start

Optionally, install the `xr` CLI globally first (`./install.sh` or
`.\install.ps1` — a local wrapper around `cargo install --path
crates/larust-cli`, since Larust isn't published anywhere yet, so `xr ...`
works instead of `cargo run -p larust-cli -- ...` below):

```bash
./install.sh      # macOS/Linux/git-bash
.\install.ps1      # Windows PowerShell
```

```bash
# Build everything
cargo build --workspace

# Scaffold a new app (must run from inside this workspace checkout —
# Larust isn't published to crates.io yet, so `xr new` resolves framework
# crates as local path dependencies)
cargo run -p larust-cli -- new examples/myapp

# ...or with session-based auth (User model, register/login/logout,
# auth/guest-protected routes) scaffolded in from the start:
cargo run -p larust-cli -- new examples/myapp --auth

# From inside the generated app:
cd examples/myapp
cargo run -- migrate   # create the SQLite database
cargo run               # serve on http://127.0.0.1:8000

# ...or, instead of the last line, rebuild + restart on every save and
# auto-refresh any open browser tab once the new build is back up:
../../target/debug/xr.exe dev
```

In another terminal, from the app directory:

```bash
../../target/debug/xr.exe route:list   # or `xr route:list` if it's on PATH
../../target/debug/xr.exe make:controller CommentController --resource
../../target/debug/xr.exe make:model Category --migration
../../target/debug/xr.exe audit         # cargo-audit over the resolved workspace lockfile
```

`examples/blog` is the reference app — generated with `--auth`, it exercises
every milestone end to end (a `Post` model belonging to its author via
`#[belongs_to(User, ...)]`, a CSRF-protected create form, session flash
messages, route model binding on `/posts/{post}`, and a full
register/login/logout flow with post-creation gated behind `require_auth`)
and is the first place to look for a working example of any feature.

### Running the test suite

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --check
```

## Workspace layout

```text
crates/
├── larust-core        Application bootstrap: config, logging, AppError
├── larust-http         Route/Router DSL, middleware, sessions, CSRF
├── larust-orm           QueryBuilder + connection pool + migrations over sqlx
├── larust-validation   FormRequest validation rules + ValidationErrors
├── larust-view          Blade-inspired template parser (pure text, no macros)
├── larust-macros        All proc-macros: FormRequest, view!, Model
├── larust-auth          Password hashing, Authenticatable, session guards, Auth<U>
├── larust-support        The facade apps actually depend on (see docs/ARCHITECTURE.md)
└── larust-cli            The `xr` binary: new, migrate, make:*, audit, update
examples/
└── blog                Reference app dogfooding every milestone
```

Generated apps depend on exactly **`larust-core`, `larust-http`,
`larust-support`, `tokio`, and `sqlx`** — every other framework crate,
including `larust-auth`, is reached indirectly through `larust-support`'s
re-exports. See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for why, and
why `sqlx` is the one crate that can't be fully hidden behind that facade.

## Documentation

- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — crate graph, the
  single-dependency-surface pattern, request lifecycle
- [`docs/MACROS.md`](docs/MACROS.md) — how each proc-macro parses and
  generates code, and why they're shaped the way they are
- [`docs/GOTCHAS.md`](docs/GOTCHAS.md) — non-obvious constraints discovered
  while building this, and why they exist — read this before debugging
  anything that touches axum extractors, macro codegen, or the CLI
  generators
