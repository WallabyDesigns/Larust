---
title: Project Structure
parent: Getting Started
nav_order: 3
---

# Project Structure
{: .no_toc }

Every generated Larust app follows the same layout. If you know Laravel's
directory conventions, almost every folder name below will already make
sense - the table under [Coming from Laravel](../../coming-from-laravel)
covers the mapping.

1. TOC
{:toc}

## Top level

```
myapp/
├── app/                  Business logic - controllers, models, jobs, ...
├── config/               Rust functions returning a JSON config value
├── database/             Migrations, factories, seeders
├── resources/            Views (and frontend assets, if you add them)
├── routes/               Route declarations
├── src/                  main.rs / lib.rs - the app's own small "glue" crate
├── tests/                Integration tests (this app gets a library target
│                         so tests/*.rs can reach app::controllers, etc.)
├── public/               Static files served at the URL root (if present)
├── storage/              Release binaries, uploaded files - gitignored
├── Cargo.toml            This app's own crate manifest
└── .env                  Local configuration - gitignored (.env.example isn't)
```

## `src/main.rs` and `src/lib.rs`

This is the one place Larust's shape genuinely diverges from Laravel's -
there's no framework binary that boots your app for you the way `php
artisan serve` boots Laravel's own `public/index.php`. Your app *is* a
real Rust binary, and these two files are its entry point.

`lib.rs` re-exports every `app/`/`routes/`/`config/` module under a
stable path (`#[path = "../app/Http/Controllers/mod.rs"] pub mod
controllers;` and so on) and defines the shared boot sequence:

- `application()` - loads config, registers error pages. No database
  connection yet - `route:list` needs only this much.
- `router(&app)` - merges `routes::web` and `routes::api` and registers
  any reactive (`@wire`) components. Still no database.
- `connect_database()` - connects `larust_orm`'s pool from
  `config/database.rs`.
- `serve()` - the above three, plus attaching sessions and actually
  binding the port. What a plain `cargo run` (no subcommand) calls.

`main.rs` is a thin CLI dispatcher in front of that: it checks
`std::env::args()` for `migrate`/`migrate:fresh`/`queue:work`/
`schedule:work`/`route:list`, and falls through to `serve()` if none
match. This is also exactly why every one of those has to be run as
`cargo run -- migrate` (note the `--`) rather than a separate binary -
they're all the same executable, branching on `argv[1]`.

{: .note }
Splitting boot logic into `lib.rs` isn't just tidiness - it's what makes a
[Tauri desktop build](../../deployment-and-desktop-apps#tauri-desktop-apps)
possible at all: `src-tauri/`'s own `main.rs` calls this crate's
`serve()` directly from a background thread instead of spawning your
compiled binary as a subprocess.

## `app/` - your business logic

Mirrors Laravel's `app/` almost exactly:

| Directory | What goes here |
|---|---|
| `app/Http/Controllers/` | Controllers - plain `struct`s with `async fn` methods, one per action |
| `app/Http/Requests/` | `#[derive(FormRequest)]` structs - validated input |
| `app/Http/Middleware/` | Your own middleware functions |
| `app/Models/` | `#[derive(Model)]` structs |
| `app/Policies/` | `Policy<U>` implementations |
| `app/Permissions/` | Role/permission definitions (only with the `permissions` feature) |
| `app/Events/` | Structs dispatched via `larust_support::event::dispatch` |
| `app/Jobs/` | `Job` implementations for the queue |
| `app/Mail/` | `Mailable` implementations |
| `app/Notifications/` | `Notification` implementations |
| `app/Wire/` | `@wire(...)` reactive components |
| `app/Providers/` | Present for Laravel-shaped familiarity; Larust has no service-container/provider-registration concept, so this is normally empty - see [FAQ](../../faq) |
| `app/Services/` | Plain, ordinary modules for whatever doesn't belong in a controller - no framework contract attaches here, it's just a conventional home |

Every one of these is real Rust: a `mod.rs` per directory declares its
children, and `lib.rs` mounts each directory under a stable module path so
the rest of the app (and `tests/`) can reach it as `app::controllers::
PostController`, `app::models::Post`, and so on.

## `routes/`

```
routes/
├── mod.rs        pub mod api; pub mod console; pub mod web;
├── web.rs        Browser-facing routes - sessions, CSRF, cookie auth
├── api.rs        Stateless API routes - merged in under `api_prefix` (`/api` by default), no CSRF
└── console.rs    Schedule::new() task declarations, read by `schedule:work`
```

`web.rs` returns a `Router` built from the `Route`/`Router` DSL - see
[Routing](../../the-basics/routing) for the full picture. The real, generated
`--auth` scaffold's `web.rs` is a good first thing to read end to end:

```rust
pub fn routes() -> Router {
    let route = Route::get("/", index)
        .get("/posts", PostController::index).name("posts.index")
        .get("/posts/{post}", PostController::show).name("posts.show")
        .plugin(larust_support::wire::WirePlugin)
        .plugin(larust_support::spa::SpaPlugin)
        .plugin(larust_support::reverb::ReverbPlugin)
        // Laravel's Route::middleware('auth')->group(...) - only wraps
        // the routes registered *inside* this closure.
        .group("", |r: Router| {
            r.middleware(axum::middleware::from_fn(require_auth))
                .get("/posts/create", PostController::create).name("posts.create")
                .post("/posts", PostController::store).name("posts.store")
        })
        .group("", |r: Router| {
            r.middleware(axum::middleware::from_fn(redirect_authenticated))
                .get("/register", AuthController::show_register).name("register")
                .post("/register", AuthController::register).name("register.store")
        })
        .post("/logout", AuthController::logout).name("logout");

    // Applied last, to the whole router at once - Laravel's own top-level
    // web middleware group convention.
    route.middleware(axum::middleware::from_fn(csrf::verify))
}
```

`api.rs` is combined into the final router separately, via `.merge(...)`
rather than nesting - deliberately, so CSRF (a cookie-auth, browser-form
concern) never reaches API routes. See [Middleware, Sessions & CSRF](../../the-basics/middleware-sessions-and-csrf)
for why `.merge` and `.group` behave differently here.

## `config/`

```
config/
├── mod.rs        pub mod app; pub mod database;
├── app.rs        pub fn config() -> serde_json::Value
└── database.rs   Connection strings per DB_CONNECTION value
```

There's no fixed, framework-wide config schema the way Laravel's
`config/app.php` implies one - `config/app.rs` is a plain function
returning a `serde_json::Value`, built field-by-field from your `.env`
via `larust_support::config_env::env_or("APP_NAME", "Larust Demo")`-style
calls with an explicit fallback for each one. Add your own config file
(`config/blog.rs`, say) the same way for app-specific settings that don't
belong in the fixed `larust_core::Config` struct - see [The
Basics](../../the-basics/) for how `config()`/`env()` helpers read this back
inside a template or controller.

## `database/`

```
database/
├── migrations/     Forward-only, hand-written SQL - see below
├── factories/       Test data builders
└── seeders/          Scripts that populate a database for local dev
```

Migrations are plain `.sql` files, numbered (`0001_create_posts_table.sql`),
applied in order by `xr migrate`. There's no `down()`/rollback concept -
see [Migrations & the Query Builder](../../database/migrations-and-query-builder)
for why, and what `xr migrate:fresh` offers instead.

## `resources/`

```
resources/
├── views/          .blade.xr templates
└── assets/          Frontend source (only if you opted into the asset pipeline)
```

See [Views & Templates](../../the-basics/views-and-templates) for the
template language itself.

## `tests/`

A generated app gets a **library target** (that `src/lib.rs`) specifically
so `tests/*.rs` can reach `app::controllers`, `app::models`, and so on as
a real dependency, the same way any external integration test reaches a
published crate. See [Testing](../../testing) for `TestClient`, `test_db`,
and `acting_as()`.

## `.env` and `.env.example`

Same idea as Laravel: `.env` holds your actual local configuration and is
gitignored; `.env.example` is the checked-in template documenting every
key a fresh clone needs to fill in. Loaded once, early, via
`dotenvy::from_path` - see [Configuration](../../database/#configuration) for
the full key reference.

## Next

[The Basics](../../the-basics/) walks through routing, controllers,
validation, and templates in depth.
