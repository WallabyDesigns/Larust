---
title: Your First App
parent: Getting Started
nav_order: 2
---

# Your First App
{: .no_toc }

1. TOC
{:toc}

## Scaffold a project

From inside your Larust checkout:

```bash
xr new examples/blog-tutorial --auth
```

A few things about that command:

- **The path must resolve inside a Larust workspace checkout.** `xr new`
  walks up from the target directory looking for the workspace's own
  `Cargo.toml` so it can wire up the new app's path dependencies
  automatically. `examples/` is the natural place to put it - the
  workspace's root `Cargo.toml` already lists `examples/*` as a member
  glob, so a new app there is picked up with zero extra configuration. A
  directory *outside* `examples/*` (or outside the checkout entirely, via
  `--workspace <path>`) still works, but needs one manual step - adding
  it to the root `Cargo.toml`'s `workspace.members` (or `.exclude`) - and
  is generally worth reserving for a real, separate project once you've
  learned the framework.
- **Omit the path** (`xr new` with nothing else) to get an interactive
  wizard instead - it asks for the project directory, whether to include
  auth, and which optional `larust-support` features you want (`db`,
  `permissions`, `reverb`, `sanctum`, `sitemap`, `socialite`), one at a
  time, rather than requiring you to already know every flag.
- **`--auth`** scaffolds a `User` model plus register/login/logout, so you
  get a real authenticated flow to look at rather than an empty shell.
  Leave it off for a minimal app with no auth at all.

You'll see:

```
Created new Larust application at examples/blog-tutorial
```

## Look around

```bash
cd examples/blog-tutorial
```

A `--auth` scaffold ships a small working blog: `Post`/`Comment`/`User`
models, a `PostController` with the full create/show/edit flow, real
`register`/`login`/`logout` routes, and Blade-flavored templates for all
of it. See [Project Structure](../../getting-started/project-structure) for what every file is.

## Create the database

```bash
cargo run -- migrate
```

```
Migrated: 0001_create_posts_table.sql
Migrated: 0002_create_users_table.sql
Migrated: 0003_create_comments_table.sql
```

This runs every `.sql` file under `database/migrations/` in order against
a fresh SQLite database (the default `DB_CONNECTION`) at the path your
`.env` names - no separate database server to install or configure for
local dev. See [Migrations & the Query Builder](../../database/migrations-and-query-builder)
for MySQL/Postgres/SQL Server instead.

## Serve it

```bash
cargo run
```

Or, if you have `xr` installed (see [Iterate with xr dev](../../getting-started/your-first-app/#iterate-with-xr-dev)), you can run it with:

```bash
xr dev
```

Visit **http://127.0.0.1:34187** (Larust's default `APP_PORT` - not 8000,
deliberately; see the [FAQ](../../faq)). Register an account, write a post,
and you're looking at a real, working Larust app.

## See every route

From another terminal, in the same directory:

```bash
xr route:list
```

```
GET     /                        
GET     /posts                   posts.index
GET     /posts/{post}            posts.show
GET     /__larust_wire/runtime.js 
POST    /__larust_wire/{component_id} 
GET     /__larust_spa/runtime.js 
GET     /__larust_reverb/runtime.js 
GET     /__larust_reverb/{channel} 
GET     /posts/create            posts.create
POST    /posts                   posts.store
POST    /posts/{post}/comments   posts.comments.store
GET     /register                register
POST    /register                register.store
GET     /login                   login
POST    /login                   login.store
POST    /logout                  logout
```

The unnamed `/__larust_*` routes are framework-internal - the wire
component runtime script and its action endpoint, the SPA-mode runtime
script, and the live-broadcast WebSocket runtime/endpoint. They're always
registered; you'll never call them directly.

## Iterate with `xr dev`

Stop `cargo run` and use the dev server instead:

```bash
../../target/debug/xr.exe dev     # or just `xr dev` if it's on PATH
```

`xr dev` binds the port itself immediately (serving a "building..." page
if the very first build hasn't finished yet), then rebuilds and hot-swaps
the running process on every save - with **zero dropped requests**, not
just a fast restart - and pushes a live-reload signal to any open browser
tab over SSE so it refreshes automatically once the new build is ready. A
build that fails leaves the last known-good version running and shows you
the real compiler error on the page, instead of taking the site down.

## Generate something

```bash
xr make:controller CommentController --resource
xr make:model Category --migration
xr make:policy Category
```

See the [CLI reference](../../cli-reference) for every `make:*` generator and
every other subcommand.

## Next

[Project Structure](../../getting-started/project-structure) walks through what every generated
file does, or jump straight into [The Basics](../../the-basics/) to start
building.
