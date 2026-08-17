# Architecture

## The core design rule: one dependency surface

Every generated app's `Cargo.toml` depends on `larust-core`, `larust-http`,
`larust-support` (plus `tokio` and `sqlx` — see below), and nothing else
framework-related. `larust-support` is a **facade crate**: it depends on
every other Larust crate and re-exports exactly what app code and
macro-generated code need, under one consistent path prefix
(`larust_support::...`).

This isn't just tidiness. The proc-macros in `larust-macros` generate code
that gets spliced into the *app's* crate, not `larust-macros`'s own crate —
so every fully-qualified path in generated code (`::larust_support::AppError`,
`::larust_support::orm::pool()`, `::larust_support::view::escape(...)`) has
to resolve from the app's dependency graph. If a macro generated a path like
`::larust_orm::pool()` instead, every app would need `larust-orm` as a
*direct* dependency too, and the "just depend on larust-support" promise
would be false. This was caught and fixed during M2's review (see
[GOTCHAS.md](GOTCHAS.md)) — the rule now is: **every path in generated code
is routed through `larust_support`, with no exceptions**, and `larust-support`
re-exports whatever's needed to make that resolve.

```text
                     ┌─────────────────┐
                     │  larust-support  │  ← apps depend on this
                     │   (the facade)   │
                     └────────┬─────────┘
          ┌──────────┬────────┼────────┬───────────┬─────────────┐
          │          │        │        │           │             │
    larust-core  larust-http  │  larust-orm  larust-validation  larust-view
                               │
                        larust-macros  ← proc-macros; generates code
                                          referencing ::larust_support::...
```
Simplified — it shows the branches, not every edge. Both `larust-http` and
`larust-orm` also depend on `larust-core` directly (for `AppError`; see the
table below), even though that's not drawn as a separate arrow here — they
still don't depend on each other, which is the shape this diagram is
actually illustrating.

`larust-macros` has an unusual relationship with `larust-support`: it's a
**dev-dependency**, used only to integration-test the macros' generated code
end-to-end (see `crates/larust-macros/tests/*.rs`), while `larust-support`'s
*normal* dependency on `larust-macros` is what actually re-exports the
derive macros to apps. This is a real dependency cycle, broken by Cargo's
support for cycles through dev-dependency edges — see GOTCHAS.md if this
ever stops building.

### The one exception: `sqlx`

`#[derive(Model, sqlx::FromRow)]` requires `sqlx` to be a *direct*
dependency of the app, because `sqlx::FromRow`'s own derive macro generates
code referencing `::sqlx::...` directly — it doesn't honor a local `use
larust_support::orm::sqlx;` alias the way our own macros do, since we don't
control sqlx's codegen. `xr new`'s generated `Cargo.toml` includes a pinned
`sqlx` dependency for exactly this reason (see
`crates/larust-cli/src/scaffold.rs`'s `cargo_toml()`). Everything else sqlx-
related (`QueryBuilder`, `pool()`, `connect()`, `migrate()`) *is* fully
routed through `larust_support::orm::*`.

## Crate-by-crate

| Crate | Owns | Depends on (within workspace) |
|---|---|---|
| `larust-core` | `Application` (config + logging + serve loop), `AppError`, the `/__larust_dev` live-reload SSE route (`dev_reload.rs`, only merged into the router when `LARUST_DEV_RELOAD` is set), `public/`-directory static-file serving (`tower_http::services::ServeDir`, always on) | — |
| `larust-http` | `Route`/`Router` DSL, middleware, sessions (`tower-sessions` + `tower-sessions-sqlx-store`'s `SqliteStore`), CSRF | `larust-core` (for `AppError`) |
| `larust-validation` | Validation rule functions, `ValidationErrors` (422 response) | — |
| `larust-view` | Blade-like parser (text → `Node` AST), layout resolution, `View`/`escape` runtime, live-reload client script injection (gated the same way, checked via `OnceLock<bool>`) | — |
| `larust-orm` | `QueryBuilder<T>`, connection pool (`OnceLock<SqlitePool>`), migration runner | `larust-core` (for `AppError`) |
| `larust-macros` | `#[derive(FormRequest)]`, `view!`, `#[derive(Model)]` (proc-macros) | `larust-view` (parser reuse) |
| `larust-auth` | Password hashing, `Authenticatable`, session guard functions, `Auth<U>` extractor, `require_auth`/`redirect_authenticated` middleware, `authorize()` | `larust-core` (`AppError`), `larust-http` (`Session`) |
| `larust-mail` | `Mailable` trait, `mail().to(...).send(...)`/`.queue(...)`, `log`/`smtp` drivers (`lettre`), `MailJob` (a framework-owned `Job` for queued mail) | `larust-core` (`AppError`, `Config`), `larust-queue` (`Job`, `dispatch`) |
| `larust-notifications` | `Notification` trait, `notify`/`notifications_for`/`unread_count`/`mark_as_read`/`mark_all_as_read` — durable, per-notifiable, read-tracked database notifications | `larust-core` (`AppError`), `larust-orm` (`pool()`), `larust-auth` (`Authenticatable`, `authorize`) |
| `larust-cache` | `cache::{put, get, forget, remember}` — single SQLite-backed driver, self-bootstrapping `cache_items` table | `larust-core` (`AppError`), `larust-orm` (`pool()`) |
| `larust-events` | `Event`, `event::{listeners, dispatch}` — in-process, synchronous pub/sub, no persistence | — |
| `larust-queue` | `Job`, `queue::{dispatch, work, JobRegistry}` — durable, SQLite-backed job queue, `failed_jobs` on error | `larust-core` (`AppError`), `larust-orm` (`pool()`) |
| `larust-scheduler` | `Schedule`, `schedule::work` — recurring, in-process tasks (`cron`-expression-driven), no persistence | `larust-core` (`AppError`) |
| `larust-storage` | `Disk`, `storage::{local, public}` — two fixed disks, path-traversal-safe file I/O | `larust-core` (`AppError`) |
| `larust-live` | `WireComponent`, `LiveRegistry`, `mount`/`update`/`runtime_js` — server-state-backed reactive components (`@wire(...)`), session-keyed, plus the vendored client runtime | `larust-core` (`AppError`), `larust-http` (`Session`, `random_hex`), `larust-view` (`View`, `escape`) |
| `larust-support` | The facade — re-exports everything above under one path | all of the above |
| `larust-convert` | `xr convert`'s conversion logic (`php` tree-sitter wrapper, `composer`/`routes`/`migrations`/`config`/`requests`/`blade` converters, `discover` — recursive directory discovery, `report`), plus `codegen` — the shared `generate_file`/`append_to_mod_rs`/etc. primitives also used by `xr make:*` | — (a build-time/dev-tooling crate, never wired into `larust-support`'s facade — see "Laravel conversion" below) |
| `larust-cli` | The `xr` binary: `new`, `make:*`, `migrate`, `route:list`, `queue:work`, `schedule:work`, `dev`, `convert`, `audit`, `update` | `larust-core`, `larust-convert` |

`xr dev` (`crates/larust-cli/src/dev.rs`) is a standalone process supervisor,
not a library any other crate depends on: it watches an app's source, runs
`cargo build`, and spawns the resolved binary directly with
`LARUST_DEV_RELOAD=1` set in its environment. That env var is the *only*
coupling between it and `larust-core`/`larust-view` — neither of those
crates depends on `larust-cli`, they just both check for the same
process-wide flag independently, so the live-reload route/script cost
nothing and require no code changes in any generated app when `xr dev`
isn't in use.

Note that `larust-view` (the *parser*) has no `syn`/`proc-macro2` dependency
at all — it's plain text parsing, reusable at macro-expansion time by
`larust-macros` without pulling proc-macro machinery into a crate that's
also a normal runtime dependency (`View`/`escape` are used by generated
code at actual request-handling time). `larust-macros` is the only crate
that turns the `Node` AST into Rust code, via `syn::parse_str` on each
interpolated expression — see [MACROS.md](MACROS.md).

## Static assets (`public/`)

`crates/larust-core/src/application.rs`. `Application::serve()` mounts
`tower_http::services::ServeDir::new("public")` as the router's
`fallback_service` — consulted only when no registered route matches, so
a route wins over a same-path file for any literal request path. (One
caveat, not a security issue: axum matches routes on the raw, undecoded
path while `ServeDir` percent-decodes before resolving a file, so a
percent-encoded request can reach a file a registered route would
otherwise have handled — it can only ever surface content already
sitting in `public/`, see the comment above `fallback_service` in
`application.rs` for detail.) Served at the URL root (`public/logo.png` →
`/logo.png`), matching Laravel's convention where `public/` *is* the
webserver's docroot — not nested under a `/public` prefix. Always on, no
config flag: unlike `xr dev`'s live-reload wiring or `APP_DEBUG`, there's
no reason a real app would ever want this off. `public/` is scaffolded
empty by `xr new`; a missing directory isn't an error, `ServeDir` just
checks the filesystem per-request. Like any static-file server, a symlink
placed inside `public/` is followed wherever it points — not a request-path
traversal risk, but worth knowing if a build step ever vendors assets via
symlinks.

Every response (not just `ServeDir`'s) carries `X-Content-Type-Options:
nosniff` (`SetResponseHeaderLayer`, applied in `Application::serve()`
alongside the panic-catching layer). This matters specifically for
`public/`: `ServeDir` infers a served file's `Content-Type` purely from
its *extension* (`mime_guess`), never its actual bytes — so anything an
app writes into `public/` under a name whose extension doesn't match its
real content (e.g. a file-upload feature that validates a declared MIME
type but not the file's actual signature) would otherwise be subject to
browser content-sniffing on top of that. `nosniff` is what keeps a
browser from second-guessing the declared type — a general OWASP-baseline
header regardless, but load-bearing here in particular.

## Health routing (`/up`)

Laravel 11-style health routing is opt-in at application bootstrap:

```rust
app.with_health_route("/up")
    .router(route.into_axum_router())
    .serve()
    .await
```

`Application::with_health_route` registers one public `GET` endpoint and
returns `200 OK` after Larust has booted successfully. The reference apps use
Laravel's conventional `/up` path. The response is a self-contained HTML
status page—“Application up” with a pulsing status indicator—rather than an
empty status response, so opening it in a browser has the same useful
experience as Laravel's built-in health page. It requires no application
template, database connection, asset, CDN, or external font request.

The page reports “HTTP request received. Response rendered in _n_ms.” using
the browser's `performance.now()` value. That is deliberately client-observed
elapsed time: it includes the request/response experience a browser sees,
whereas server-side handler work in a long-running Rust process is often less
than one millisecond and would misleadingly display as `0ms`. Non-browser
health probes still receive the normal `200 OK` HTML response; the timing
placeholder remains `--` if JavaScript does not run. The endpoint sends
`Cache-Control: no-store` so an intermediary cannot serve a stale healthy
response.

This initial route verifies successful application bootstrap only. Dependency
diagnostics (database, cache, or external services) belong in a future health
check registry, rather than making a basic load-balancer endpoint fail because
an optional application dependency is unavailable.

## Navigation transitions (`view-transition` meta tag)

The scaffolded layout (`crates/larust-cli/src/scaffold.rs`'s
`LAYOUT_APP_BLADE_XR`) includes
`<meta name="view-transition" content="same-origin">` in `<head>`. Larust is
a traditional server-rendered app — every link is a real, full-document
navigation, not client-side routing — and without this, that means a hard
flash to blank between pages (the old document fully unloads before the new
one's first paint, so the tab title and content both visibly disappear for
a moment). This meta tag opts into the browser-native Cross-Document View
Transitions API: on a same-origin navigation, supporting browsers capture
the outgoing page, cross-fade to the incoming one, and the `<title>` never
visibly blanks. No JavaScript, no client-side router, no change to how
pages are served — `view!(...)` still renders one complete HTML document
per request exactly as before; this only changes how the *browser* presents
the transition between two already-complete documents. Progressive
enhancement: browsers without support (older Safari/Firefox as of this
writing) just navigate normally, with no error and no missing
functionality — this is purely a presentational improvement layered on top
of navigation that already worked.

## Descriptive errors (`APP_DEBUG`)

`crates/larust-core/src/error.rs` + `crates/larust-core/src/debug.rs`.
`Config::app_debug` (`APP_DEBUG` env var, default `false`) is read once in
`Application::new()` and stored in a process-wide `OnceLock<bool>` — same
idiom as `larust-orm`'s connection pool, and the same env-var-gated
pattern `xr dev`'s live-reload wiring already uses elsewhere in this
crate. `AppError::into_response` checks it:

- `Internal`/`Config` (both carry a boxed `std::error::Error`): debug mode
  renders an HTML page with the top-level message and the full `source()`
  chain, walked one level at a time. Production mode is byte-for-byte
  today's generic `"internal server error"` text — nothing about the
  default response changed.
- `NotFound`: debug mode gets a small branded HTML 404; production mode
  unchanged plain text.
- `Http { status, message }`: unchanged in both modes — already
  caller-controlled via `abort()`, nothing to gate.
- A panic in a handler is caught by `tower_http::catch_panic::CatchPanicLayer`
  (wired in `Application::serve()`) and routed through the same
  debug/production branching (`error::render_panic`), so a panic gets a
  real response instead of silently failing that one request — with no
  synthetic `source()` chain, since there's no `std::error::Error` value
  for a panic payload. This relies on `catch_unwind`, which only works
  under the default `panic = "unwind"` strategy — no crate in this
  workspace sets `panic = "abort"` in its profile today, but if one ever
  does, `CatchPanicLayer` silently becomes a no-op and a panic goes back
  to aborting the whole process, exactly as it would without this layer.

Both the HTML page and the panic handler live in `larust-core` only —
`AppError`'s debug rendering doesn't reuse `larust-view`'s `escape()` (a
different crate, and pulling in a view-layer dependency for one small
HTML-escaping helper would be an odd layering edge for what it buys); it
has its own tiny local copy instead. Scaffolded apps ship `APP_DEBUG=true`
in their own `.env` for local dev (Laravel's own scaffold convention);
the struct-level default of `false` only matters for a deployment that
never carries that file/var. See `docs/GOTCHAS.md` for why this must
never be `true` outside local dev.

## `Route::resource(...)`

`crates/larust-http/src/route.rs` — Laravel's `Route::resource('posts',
PostController::class)` in one call, registering all 7 RESTful routes
(index/create/store/show/edit/update/destroy) with Laravel's own naming
convention (`posts.index`, `posts.show`, etc.). Pairs naturally with `xr
make:controller --resource`, which already generates the 7 stub methods.

Two arguments, both explicit strings, neither inferred: the resource name
(`"posts"`, no leading slash — matching Laravel's own convention; the
path gets one prepended, the route names must not have one, and the same
string drives both) and the path-parameter name used by the
`show`/`edit`/`update`/`destroy` routes (`"post"`) — kept separate rather
than singularized from the resource name, since turning `"categories"`
into `"category"` needs real singularization logic this codebase
deliberately doesn't have (same "explicit string, never inferred" stance
`#[belongs_to_many(...)]`'s `related_pivot_key` already established), and
because `param` really describes `#[derive(Model)]`'s route-model-binding
convention for the target type, not a property of the resource name
string itself.

Implemented as a straight-line sequence of the existing
`.get`/`.post`/`.put`/`.delete`/`.name` calls — not a separate
registration path — so it composes with `.middleware(...)`/`.group(...)`
exactly like a hand-written sequence of those calls would.

## Request lifecycle

For a request hitting a route built via `Route::get(...).middleware(...).with_sessions()`:

1. **Session layer** (`tower_sessions::SessionManagerLayer`) — always
   applied outermost by `Router::into_axum_router()`, regardless of the
   order `.middleware()`/`.with_sessions()` were called in. This guarantees
   every other layer and every handler can rely on `Session` being
   extractable. See `crates/larust-http/src/route.rs`.
2. **Registered middleware**, in the order `.middleware(...)` was called —
   `Router` reverses axum's own last-registered-wins-outermost ordering
   internally so that call order matches Laravel's middleware-array
   semantics (see GOTCHAS.md). Applied *per route entry* (via
   `MethodRouter::layer`), not once over the whole merged `axum::Router` —
   this is what makes group-scoped middleware possible (see below).
   CSRF verification (`larust_http::csrf::verify`) and, in an app scaffolded
   with `xr new --auth`, `require_auth`/`redirect_authenticated` (see
   "Authentication" below) are the middleware built in so far. CSRF checks
   the `X-CSRF-TOKEN` header *first* (Laravel's own convention, sourced
   from a `<meta name="csrf-token">` tag — see `larust_http::csrf::HEADER_NAME`),
   before touching the body at all; only when that header is absent does it
   fall back to buffering the body, checking the `_csrf_token` form field
   against the session, and reconstructing the request so downstream
   extractors can still read it. The header path matters beyond matching
   Laravel: it's what lets a JS-driven request (a `fetch`/`XMLHttpRequest`
   upload, for instance) skip the form-urlencoded fallback's 2MB body-read
   cap entirely — that cap only ever applies to the field-based path.
3. **Route matching** — axum's own router, using paths translated from
   Laravel's `{param}` syntax to axum's `:param` syntax by
   `larust_http::path::to_axum_path` at `Router::into_axum_router()` time.
4. **Extraction**, left to right in the handler's argument list:
   - `FromRequestParts` extractors first (session, route-model-bound
     `Model` types via `#[derive(Model)]`'s generated
     `impl FromRequestParts`, `Auth<U>` — see below) — these don't consume
     the body, so order among themselves doesn't matter, but they must all
     come before —
   - the one `FromRequest` extractor last (a `#[derive(FormRequest)]`
     struct), which consumes the body: reads it fully (2 MiB cap), parses
     as `application/x-www-form-urlencoded`, runs every field's
     `#[validate(...)]` rules, and either returns `Self` or short-circuits
     with a 422 **before the handler body ever runs**.
5. **Handler runs**, returning anything implementing `IntoResponse` — a
   `View` (from `view!`), a `Redirect` (from `larust_support::redirect()`),
   a plain type, or `Result<T, AppError>`.
6. **Response flows back out** through the middleware stack in reverse,
   the session layer serializes any session mutations into a `Set-Cookie`
   header, and it's sent.

## Group-scoped middleware

`Router::group(prefix, build)` doesn't just prefix paths — any
`.middleware(...)` calls made inside `build`'s closure are baked into
*just that group's* route entries (via `MethodRouter::layer`) before
they're merged into the parent `Router`, and never leak into the parent's
own `middlewares` list. A top-level `.middleware(...)` call still covers
every route on that `Router`, including ones added via `.group(...)` —
the two mechanisms compose, with a group's own middleware always ending up
innermost (wrapped by whatever the parent's global middleware applies).
This is what lets `require_auth`/`redirect_authenticated` protect only
some routes (`Route::middleware('auth')->group(...)` in Laravel terms)
instead of applying to the whole app the way CSRF does. See
`crates/larust-http/src/route.rs`'s `group`/`middleware`/
`into_axum_router` and `crates/larust-http/tests/middleware_dsl.rs` for
the composition rules, tested directly (nesting, sibling groups, call
order independence).

## Sessions (`Router::with_sessions`)

`crates/larust-http/src/session.rs`. Backed by `tower-sessions-sqlx-store`'s
`SqliteStore` over the app's own connection pool — not
`tower_sessions::MemoryStore`. Session data has to survive a process
restart (a deploy, a crash, `xr dev`'s rebuild-and-restart cycle on every
file save), not just live for one process's lifetime; an in-memory store
"works" in every manual test and then silently logs every user out on the
next restart, with no error anywhere — the same trap as Laravel shipping
the `array` session driver to production. There's deliberately no
in-memory option left in this crate's public API to fall into.

`SqliteStore::new(pool)` + `.migrate()` (an idempotent
`CREATE TABLE IF NOT EXISTS`) is self-contained: no migration file is
needed in any app's own `database/migrations/`, and nothing is added to
the app's `_migrations` bookkeeping table — the sessions table manages
itself.

**`larust-http` depends on `sqlx` directly** for this — the same
exception this doc's "one dependency surface" section already carves out
for `sqlx::FromRow`, reused here for the same reason (`SqlitePool` has to
be nameable in this crate's own function signatures). It does **not**
depend on `larust-orm`: the pool is constructed by the caller
(`larust_support::orm::pool()`) and passed in, keeping `larust-http` and
`larust-orm` as independent siblings under `larust-support`, same as
today's dependency shape.

`Router::with_sessions(pool, secure)` is `async` (building the store runs
`.migrate().await`) and returns `Result<Self, AppError>`. This is why
every generated app's `main.rs` builds its `route` *without*
`.with_sessions(...)` in the fluent chain, checks for `route:list` on that
undecorated route, and only calls `.with_sessions(...)` afterward, right
before `connect_database().await?`'s pool becomes the argument —
`xr route:list` is pure static introspection (path/method/name only, see
`Router::routes()`) and has no reason to need a working database
connection just to print a table.

## Authentication

`larust-auth` (re-exported as `larust_support::auth::*`) is session-backed,
not token-based — it stores the authenticated user's id (from
`Authenticatable::auth_id`) in the same `tower_sessions::Session` CSRF and
flash messages already use, rather than introducing a second state
mechanism. An app's `User` model implements `Authenticatable` (typically a
two-line delegation to `#[derive(Model)]`'s own generated `find`, since
`find_for_auth`'s signature mirrors it exactly).

- `larust_support::auth::login(&session, &user)` rotates the session id
  (`Session::cycle_id()`) *before* storing the user id — session-fixation
  protection, so a pre-login session token can't be reused post-login. See
  GOTCHAS.md.
- `larust_support::auth::logout(&session)` flushes the *entire* session
  (not just the auth key), so CSRF tokens and flash data are invalidated
  too.
- `Auth<U>` (a `FromRequestParts` extractor) resolves the current user or
  401s — for handlers that need the `User` itself. `require_auth`/
  `redirect_authenticated` (route middleware, group-scoped per above) are
  for redirecting browsers rather than returning a bare 401.
- `authorize(bool) -> Result<(), AppError>` is a one-line 403 helper, not a
  `Gate`-style runtime registry — the convention is a plain typed method on
  your own model (`post.can_update(&user)`), converted at the call site
  (`authorize(post.can_update(&user))?`), so a typo in an ability name is a
  compile error rather than a silently-always-false runtime lookup. For the
  common CRUD case, `Policy<U>` (below) builds on this same function.

`xr new --auth` scaffolds a `User` model + migration, register/login/logout
routes, and demonstrates group-scoped middleware for real: post-creation
routes wrapped in `require_auth`, `/register`+`/login` wrapped in
`redirect_authenticated`. See `crates/larust-cli/src/scaffold.rs`'s
`MAIN_RS_WITH_AUTH`.

## Authorization policies (`Policy<U>`)

`larust_support::auth::Policy<U: Authenticatable>` is a hand-implemented
trait per model (`crates/larust-auth/src/policy.rs`) — the same shape as
`Authenticatable` on `User`: no derive macro, no auto-discovery, so a typo
in an ability name is a compile error, not a silently-false runtime
lookup. It gives every model the same 5 ability names Laravel's own
default resource policy uses — `view_any`/`view`/`create`/`update`/
`delete` — deliberately excluding `restore`/`forceDelete`, since
`larust-orm` has no soft-delete concept anywhere for those to gate.

`view_any`/`create` are class-level abilities (no specific row exists yet)
and are associated functions, not methods; `view`/`update`/`delete` take
`&self`. All 5 are **required, with no default body** — a trait-level
default of `false` would reintroduce exactly the "silent gap instead of
compile error" failure mode `authorize()`'s own doc comment already
treats as this framework's core selling point over Laravel's
`Gate::define`. Each ability has a matching `authorize_*` default method
built on `authorize()` itself, so a call site reads `post.authorize_update(&user)?`
(instance) or `Post::authorize_create(&user)?` (class-level) rather than
`authorize(post.update(&user))?`.

`xr make:policy Post [--user User]` writes `app/Policies/post_policy.rs`
with all 5 methods stubbed `false` (deny-by-default — matches Laravel's
own generated-policy convention, and forces a developer to decide each
ability rather than accidentally ship an open `true`). Unlike Laravel's
`make:policy PostPolicy --model=Post`, there's no separate policy class
name to invent — the `impl Policy<User> for Post` lives directly on the
model, so the generator just takes the model name. `app/Policies/` (empty
in every scaffolded app until the first `xr make:policy` call) is wired
into `main.rs` as a real module from the start, the same way
`app/Http/Middleware/` already is — see `crates/larust-cli/src/generate.rs`'s
`make_policy` and `scaffold.rs`'s `POLICIES_MOD_RS`.

`Policy` is intentionally *not* consulted by `Auth<U>` or `require_auth` —
those only answer "is someone logged in"; `Policy` answers a later,
narrower question ("is *this* user allowed to do *this* to *this row*")
and is called explicitly inside the handler body once a concrete user is
already in hand. Folding policy checks into an extractor or middleware
would require knowing which model/ability applies per-route, reintroducing
a string-or-closure-keyed registry — exactly what this framework's
`authorize()` doc comment already rejects.

## Helpers: `route()`/`route_with()`/`url()`/`asset()`/`config()`

Laravel-shaped free functions in `larust_support`, callable from anywhere —
a controller, or directly inside a `{{ }}` template interpolation, since
those already accept arbitrary Rust expressions. No template-layer changes
were needed to add these.

- `route(name)` resolves a named route to its declared path. Fails (rather
  than returning a broken literal path) if the route needs a `{param}` that
  wasn't given — use `route_with` for those.
- `route_with(name, &[("param", "value"), ...])` substitutes each `{param}`
  placeholder and fails if any remain unfilled afterward. `params` is
  explicit `(name, value)` pairs, matching this codebase's existing
  "explicit, never inferred" stance elsewhere (`#[belongs_to_many(...)]`'s
  `related_pivot_key`, `Route::resource`'s `param` argument) rather than
  Laravel's looser positional-array form. Substitution is a single
  left-to-right pass over the route's *declared* path — an inserted
  param value is never rescanned for further `{...}` matches, so one
  param's value can't be misread as another param's placeholder even if it
  happens to contain literal brace characters.
- `url(path)`/`asset(path)` build an absolute URL from `Config::app_url`
  (set via the `APP_URL` env var, defaulting to `http://localhost` —
  Laravel's own scaffold default). `asset()` is currently a pure
  delegation to `url()`; a distinct `ASSET_URL`/CDN concept is a natural
  future addition once something actually needs it.
- `config(key)` is a deliberately stringly-typed, Laravel-shaped
  `config('app.name')`-style lookup over a small set of known keys
  (`app.name`, `app.env`, `app.url`, `app.port`, `app.debug`,
  `session.secure_cookie`) — the one intentional exception to this
  framework's usual compile-checked config access. `app.config().field`
  (via the `Application` returned by `Application::new()`) remains
  available and is still the preferred way to reach config values that are
  statically known at the call site; `config(key)` exists for Laravel
  API-shape parity, not as a replacement.

All four read through `larust_core::config()`, a `OnceLock`-backed
process-wide accessor in the same style as `larust_orm::pool()`, populated
once by `Application::new()`. It shares its bare name with
`larust_support::config(key)` — call it by its full path
(`larust_core::config()`, as every call site in this codebase already
does) if a file needs both.

## Testing (`larust-testing`)

`larust_testing::{TestClient, TestResponse, test_db, test_transaction}` — added to a
generated app's `[dev-dependencies]`, never shipped. Drives the app's real
`axum::Router` in-process via `tower::ServiceExt::oneshot` (no TCP
binding); `Application::serve()` consumes itself and has no test-friendly
seam, so a test builds its own router independently, the same way
`crates/larust-auth/tests/guard.rs`/`crates/larust-http/tests/csrf.rs`
already did by hand before this crate existed. Also re-exports
`{fake, assert_sent, assert_not_sent, SentMail}` straight from
`larust-mail` (see the Mail section's own "`Mail::fake()`/`assertSent()`"
subsection below) — not wrapped or reimplemented, since the interception
point (`MailBuilder::send`) has to live in `larust-mail` itself.

- **`TestClient`** wraps a router plus a tracked session cookie, adopted
  automatically from every response's `Set-Cookie` header — eliminating
  the "manually thread the cookie string through every call" boilerplate
  those hand-rolled tests repeated. `acting_as(&user)` (Laravel's
  `actingAs($user)`) builds its own `Session` against the *same*
  `SqliteStore`/pool the router's session layer uses (two independently
  constructed `SqliteStore` handles over the same pool/table are
  behaviorally interchangeable — verified directly against the vendored
  `tower-sessions-core` source: `Session::save()` and `Id`'s `Display`
  impl are both public, and produce exactly the cookie value the
  middleware expects), logs the user in, and persists the session — no
  need for a working `/login` route to exist in the router under test at
  all. `post_multipart(path, csrf_token, filename, content_type, bytes)`
  (added for M31's `demo/tests/upload_test.rs`, the first test needing a
  real file upload) hand-builds a single-field `multipart/form-data` body
  and sends the CSRF token via the `X-CSRF-TOKEN` *header*, not a form
  field — `larust_http::csrf::verify` checks that header before touching
  the body specifically so a multipart body is never misread as
  `application/x-www-form-urlencoded` (see that function's own doc
  comment), so a multipart test has to authenticate the same way a real
  upload client would.
- **`TestResponse`** eagerly buffers the body into an owned `String` at
  construction (eliminating the repeated `to_bytes`+`from_utf8`+`unwrap`
  dance at every assertion site), and offers `csrf_token()` (scrapes the
  `@csrf` directive's hidden field via `larust_http::csrf::FIELD_NAME`)
  plus panic-on-failure `assert_status`/`assert_redirect_to`/
  `assert_body_contains` sugar over this codebase's plain `assert!`/
  `assert_eq!` style.
- **`test_db(migrations_dir)`** connects and migrates a fresh, on-disk
  (`tempfile`-backed, not `:memory:`) SQLite database, idempotent within a
  process via a `tokio::sync::OnceCell` — the first call in a test binary
  connects and migrates; every later call in the *same* binary (i.e.
  every other `#[tokio::test]` fn in that file) just returns a clone of
  the already-connected pool. Deliberately **additive-only**: write test
  assertions scoped to the specific rows a test creates (see
  `demo/tests/posts_policy_test.rs`), not broad table-wide counts. The
  temp directory backing each database is deliberately leaked (kept alive
  for the test binary's whole process, not cleaned up on exit) —
  `cargo test` runs accumulate one temp directory per test binary run
  indefinitely; nothing in this repo sweeps them, so a CI environment
  should clean its own OS temp directory between runs rather than relying
  on this crate to do it.
- **`test_transaction(migrations_dir, body)`** (M33) is the isolation
  story `test_db()` always deferred — Laravel's `RefreshDatabase`, though
  the name suggests `DatabaseTransactions`; the difference is deliberate
  and worth recording. A real `BEGIN`-before/`ROLLBACK`-after design was
  built first: every generated `#[derive(Model)]` method and
  `QueryBuilder` call resolves its connection through exactly one
  function, `larust_orm::pool()` (not a parameter threaded through every
  generated method), so a `tokio::task_local!` override on `pool()`
  itself gave real per-call isolation with zero changes to
  `larust-macros`'s generated code or `QueryBuilder`'s public API — far
  smaller than this feature's own prior "well beyond a testing crate"
  estimate suggested. It broke on contact with a realistic test, though:
  `tower-sessions-sqlx-store` (already a real dependency, used by every
  session-backed route) opens its own real `sqlx::Transaction` internally
  on every session save. A raw `BEGIN` bypasses sqlx's own transaction
  bookkeeping entirely (deliberately, to avoid `pool()`'s return type
  ever having to change), so sqlx's `pool.begin()` call had no idea a
  transaction was already open and issued a second, literal `BEGIN` on
  the same connection — SQLite rejects nested `BEGIN`s outright, so *any*
  test using `TestClient` against a session-backed route (the single most
  common, most valuable kind of test in this codebase) broke immediately.
  `test_transaction()` ships instead as a **fresh, dedicated, freshly
  migrated SQLite database per call** — no shared transaction state for
  anything else to collide with, so it works unconditionally, at the cost
  of a real migration run per call instead of reusing one schema and
  undoing only the data. The `tokio::task_local!` override on `pool()`
  survives from the abandoned design and is still exactly what makes this
  work — it's *what* gets scoped per call (a whole dedicated pool, not a
  transaction handle) that changed. Because the task-local is per-*task*,
  not process-wide, `test_transaction()` uniquely among this crate's
  mechanisms doesn't need the "one test per file" workaround `test_db()`
  and everything else here relies on — see `crates/larust-testing/src/
  transaction.rs`'s own doc comment for the full design story, including
  a real, known gap: `larust-cache`/`larust-queue` each lazily
  self-bootstrap their own table behind a *process-wide* `OnceCell`, not
  a per-pool one, so a second `test_transaction()` call touching either
  in the same process can hit "no such table."

## Mail (`larust-mail`)

`larust_support::mail::{Mailable, mail}` — Laravel's `Mail::to($user)
->send(new WelcomeMail($user))`. Two halves, deliberately shaped
differently:

- **`Mailable`** (one required method, `subject(&self) -> String` and
  `html_body(&self) -> String`, no defaults) is a trait implemented once
  per email type — the same "app implements this once per thing" shape as
  `Policy<U>`/`Authenticatable`, not a bare builder struct, since a
  Mailable genuinely varies per app (unlike `redirect()`, where there's no
  per-app variation, just a fluent entry point). Both methods are
  required with no default body for the same reason `Policy<U>`'s 5
  abilities are: a trait-level default would reintroduce a silent gap
  (a blank subject or body) instead of a compile error. A typical impl
  renders its body through the same `view!` macro used for HTTP
  responses, via `View::into_html()` (`crates/larust-view/src/runtime.rs`)
  — a small addition alongside the existing `IntoResponse` impl,
  deliberately bypassing its dev-reload script injection (irrelevant for
  an email body). `Mailable::build()` doesn't exist as a separate step
  the way Laravel's does — `subject()`/`html_body()` are plain synchronous
  methods, since arranging already-resolved data into a subject/body does
  no I/O, sidestepping the async-trait-`Send` GOTCHAS.md landmine
  entirely (no `-> impl Future<...> + Send` spelling needed anywhere in
  this design).
- **`mail()` → `MailBuilder`** (the *sending* side) mirrors `redirect()` →
  `RedirectBuilder`: `mail().to(email).send(mailable).await?`, `.to(...)`
  callable more than once for multiple recipients.

`send()` first checks whether `Mail::fake()` (see below) is active; if
not, it dispatches on `Config::mail_driver`:
- `"log"` (the scaffold default, matching Laravel's own `MAIL_MAILER=log`
  local-dev convention) writes the rendered subject/body to
  `tracing::info!` and returns — no network touched, no SMTP server
  needed for local dev or `cargo test`. This is what keeps
  `demo`/`examples/blog`'s own registration tests
  (`demo/tests/posts_policy_test.rs`) from needing a real SMTP server.
- `"smtp"` sends for real via `lettre`, building a fresh
  `AsyncSmtpTransport` on every call rather than pooling one behind a
  process-wide `OnceLock` — mail-sending isn't a hot path, and this
  avoids adding an SMTP-connectivity failure mode to `Application::new()`'s
  own startup sequence. `MAIL_ENCRYPTION` selects implicit TLS
  (`"tls"`, the default — also the fallback for any unrecognized value),
  `"starttls"`, or `"none"`.

**`MailBuilder::queue<M>()`** — `mail().to(email).queue(mailable).await?`,
Laravel's `Mail::to($user)->queue(new WelcomeMail($user))`. `Mailable`
deliberately has no `Serialize`/`'static` bound (see above — the real
`WelcomeMail<'a>` borrows), so `.queue()` can't serialize the typed
`mailable` the way an app-defined `larust_queue::Job` would. Instead it
renders `subject()`/`html_body()` eagerly and synchronously — the exact
same rendering `.send()` already does — and enqueues only the
already-rendered `{to, subject, html_body}` via a framework-owned
`larust_mail::MailJob` (`JOB_TYPE = "__larust_queued_mail"`), whose
`handle()` reuses the same driver-dispatch logic (`deliver()`) `.send()`'s
real path already calls. **A deliberate, documented deviation from
Laravel**: Laravel's `Mail::queue(...)` stores a serialized *reference* to
the mailable's own data and re-renders fresh on the worker at send time —
DB changes between queue-time and send-time are reflected, and rendering
work moves off the request thread. This design only defers *delivery*
(the SMTP/network I/O); rendering still happens synchronously at
`.queue()`'s own call site, and the HTML is frozen from that moment on.
Replicating Laravel's re-resolve-on-worker behavior would need a
`SerializesModels`-style generic model-lookup mechanism this framework
doesn't have — a materially bigger feature than this one.

There's no runtime auto-registration mechanism for `MailJob` any more than
for an app's own job types — `JobRegistry` never discovers handlers on its
own. What differs is *who writes the registration line*: `xr new`'s
scaffold generates every app's `queue:work` branch with
`registry.register::<larust_support::mail::MailJob>()` already present by
default (see "Events + Jobs/Queues" below), rather than leaving it as a
hint the app author has to remember to add — an idle registration costs
nothing if `.queue()` is never called, so there's no reason to make it
opt-in the way an app-specific job type is. An app that deletes the line
(or was scaffolded before this default existed) sees an unregistered
`MailJob` land in `failed_jobs`, the same failure mode as any other
unregistered job type — not a special case.

`Config` (`crates/larust-core/src/config.rs`) gained `mail_driver`/
`mail_host`/`mail_port`/`mail_username`/`mail_password`/
`mail_encryption`/`mail_from_address`/`mail_from_name`, each with its own
`MAIL_*` env override, matching every existing field's one-at-a-time
pattern. `mail_username`/`mail_password` are deliberately **not**
reachable through `larust_support::config(key)` (the stringly-typed,
template-reachable helper) — a credential is one accidental
`{{ config("mail.password") }}` away from being rendered into a page;
`app.config().mail_password` (the compile-checked path) stays available
for anything that legitimately needs it.

### `Mail::fake()`/`assertSent()` (`larust_testing::{fake, assert_sent, assert_not_sent}`)

Reached through `larust-testing`, never through `larust_support::mail` —
calling `fake()` from real app code would silently and permanently stop
that process from ever sending real mail again (an `OnceLock`, first call
wins, same idiom as every other process-wide registry in this codebase).
`larust-testing` reaches `larust-mail`'s internals directly rather than
through the production facade, the sanctioned "reach deeper into
framework internals, for testing only" exception this crate already
relies on for `TestClient`.

**Records rendered output, not the typed `Mailable` instance.** The real
`WelcomeMail` (`demo/app/Mail/welcome_mail.rs`) is `WelcomeMail<'a> { user:
&'a User }` — a borrow, not owned data. Storing the instance itself
(`Box<dyn Any + Send>`, closer to Laravel's own reference-counted-object
approach) would require `Mailable: 'static`, forcing every Mailable
— including this one, already shipped — to own its data instead of
borrowing. Recording `to`/`subject`/`html_body` (all owned strings by the
time `subject()`/`html_body()` return) plus the sender's
`std::any::type_name::<M>()` sidesteps that entirely, at the cost of
assertions being content-based rather than able to inspect the Mailable's
own fields directly. `type_name` needs no `'static` bound on `M`, and
**empirically verified** (a standalone probe, not just reasoned about)
that it renders every lifetime parameter uniformly as `<'_>` — so
`type_name::<WelcomeMail<'_>>()` at the assertion site always matches
`type_name::<M>()` captured during a real `send::<M>()` call with a
concrete borrowed lifetime.

`MailBuilder::send`'s signature changed from `impl Mailable` sugar to an
explicit `send<M: Mailable>(self, mailable: M)` — behaviorally identical,
just needed so `std::any::type_name::<M>()` has a concrete `M` to name.
`fake()` short-circuits `send()` *before* `Config::mail_driver` is even
read, so once active it overrides log/smtp regardless of configuration
(matching Laravel's own `Mail::fake()`) — a test using it doesn't need
`Application::new()` to have run just for mail's sake, though other code
paths in a real test may still need it for unrelated reasons.
`demo/tests/posts_policy_test.rs` deliberately stays on the plain `log`
driver, untouched by this feature.

`assert_sent<M>`/`assert_not_sent<M>` compute their result and drop the
recorder's `Mutex` guard *before* calling `assert!` — panicking with the
lock still held would poison it (`std::sync::Mutex` poisons on an unwind
through a held lock), breaking every later assertion or `send()` call in
the same process with a confusing `PoisonError` instead of the real
failure. This was a real bug caught by this feature's own test suite
(a test intentionally triggering an assertion failure via
`std::panic::catch_unwind`, to prove `assert_sent` panics on a
non-matching predicate, poisoned the lock for every subsequent scenario
in the same test function), not merely a theoretical concern.

`.queue()` folds into this exact same recorded list under `Mail::fake()` —
a faked `.queue()` call records a `SentMail` exactly like `.send()` does,
so `assert_sent::<M>(...)` doesn't care which one an app used. Laravel
itself tracks these separately (`assertSent` vs. `assertQueued`, since
`assertQueued` fires before delivery), but this framework has no such
timing distinction worth preserving yet — `assertSentCount`/
`assertNothingSent`/`assertQueued` remain out of scope for v1, a
documented future extension, the same shape as Queue's own deferred
retry/backoff.

## Cache (`larust-cache`)

`larust_support::cache::{put, get, forget, remember}` — Laravel's
`Cache::put($key, $value, $ttl)`/`Cache::remember(...)`. Plain functions,
not a builder: unlike `mail()` (which earns `MailBuilder` from genuine
multi-value chaining — `.to(a).to(b)`), cache's operations don't accumulate
state across calls, so a builder here would be ceremony with no payoff.

**A single SQLite-backed driver, no toggle, no in-memory option** — the
same stance `larust_http::session` already takes for sessions, whose own
doc comment calls an in-memory store "a real, common trap (an app that
'works' in every manual test, then silently [fails] on every deploy)."
Laravel itself points the same direction: as of Laravel 11, the default
`CACHE_STORE` is `database`, not `file`/`array`. This is a narrower design
than Mail's `log`/`smtp` split — that split exists specifically to dodge
real network I/O in tests/local dev, a concern that doesn't apply to a
local SQLite table, so there's nothing here to toggle and no new `Config`
fields, `.env` entries, or scaffold wiring at all.

The `cache_items` table (`key`, `value` as JSON text, `expires_at` as a
Unix-seconds integer) self-bootstraps via a plain
`CREATE TABLE IF NOT EXISTS`, the same statement shape
`larust_orm::migrate::run` already uses unconditionally for its own
`_migrations` bookkeeping table — no app-level migration file needed. It
goes a step further than either existing self-bootstrapping table in this
codebase, though: `migrate::run`'s `_migrations` table and
`larust_http::session`'s `tower_sessions` table (via `SqliteStore::
migrate()`) are each unconditional *once invoked*, but both still need one
explicit call at startup/wiring time (`main.rs`'s `migrate` subcommand;
`Router::with_sessions()`). `cache_items` has no such call anywhere —
bootstrap runs lazily, inside every public `cache::*` function, memoized
process-wide via `tokio::sync::OnceCell<()>` (the same
`OnceCell::const_new()` + `get_or_try_init` idiom `larust-testing`'s
`TEST_DB` uses), so `cache::put(...)` works immediately after
`larust_support::orm::connect(...)` has run — no `.with_cache(...)`-style
wiring call in `main.rs` at all.

`get::<T>(key)` returns `Ok(None)` only for a genuine miss — key absent, or
present but expired (evicted lazily on that same read). A key that exists
but fails to deserialize as the requested `T` (e.g. `get::<String>(key)`
against a value `put` as an `i64`) is a caller bug, not a freshness
question, so it surfaces as `Err(AppError::Internal)` rather than silently
degrading to `None` the way Laravel's own cache would — matching this
codebase's stated "never silently swallow errors" discipline over strict
Laravel parity here.

`remember(key, ttl, f)` is the only place `put`/`get` compose: a cache hit
returns without calling `f`; a miss calls `f`, stores the result, and
returns it. `f: FnOnce() -> Fut` is a plain generic parameter, not a trait
method, so there's no async-fn-in-traits `Send` pitfall to design around —
the same reasoning `larust-mail`'s `Mailable` methods already rely on.

`demo`/`examples/blog` wire a real example: `PostController::index` caches
the total post count under `"posts.count"` for 60 seconds via `remember`,
and `store`/`destroy` (the handlers that change the total) call
`cache::forget("posts.count")` after a successful mutation to invalidate
it. A plain `i64` was chosen over caching the assembled, per-viewer-shaped
post list itself (whose `can_manage` flag depends on who's asking, exactly
the kind of data that must not be cached keyed only by `"posts.index"`) —
and it needs no `serde` derive in app code, since `i64` already implements
`Serialize`/`Deserialize` via serde's blanket impls, preserving "one
dependency surface" without any new escape hatch. Like `WelcomeMail`, this
usage lives only in `demo`/`examples/blog`, not in
`crates/larust-cli/src/scaffold.rs`'s templates — there's no required setup
for a freshly generated app to get `cache()` working, so nothing is
scaffolded by default.

## Events + Jobs/Queues (`larust-events`, `larust-queue`)

Two deliberately separate crates, matching Laravel's own real distinction
(an event's listeners run in-line, synchronously, by default; only a
`ShouldQueue` listener defers) rather than building one system that tries
to do both:

- **`larust_support::event::{listeners, dispatch, Event}`** — in-process,
  synchronous, no persistence. `Event` is a blanket impl over any
  `Clone + Send + Sync + 'static` value — no derive macro, no required
  methods. `event::listeners().on::<E>(closure).publish()` registers
  listeners into a process-wide registry (same `OnceLock`, "first writer
  wins, a second `.publish()` warns" shape as
  `larust_http::route`'s named-route registry); `event::dispatch(e).await`
  runs every listener registered for `E`'s type, sequentially, in
  registration order. Dispatching before any `.publish()` call, or an
  event type nothing is registered for, is a silent no-op — there's no
  `AppError` return here, since a listener that can fail should log its
  own error rather than short-circuit the others.
- **`larust_support::queue::{dispatch, work, Job, JobRegistry}`** —
  durable, SQLite-backed. `Job` is implemented once per job type (the same
  "app implements this once per thing" shape as `Policy<U>`/`Mailable`),
  with a required `const JOB_TYPE: &'static str` (explicit and app-chosen,
  deliberately not `std::any::type_name::<Self>()` — that string isn't
  stable across a rename, and an already-queued row would silently stop
  matching its handler) and `fn handle(&self) -> impl Future<Output =
  Result<(), AppError>> + Send` — the exact `-> impl Future<...> + Send`
  spelling `larust_auth::Authenticatable::find_for_auth` already
  established (see `docs/GOTCHAS.md`) to avoid the async-fn-in-traits
  `Send`-propagation pitfall. Unlike `Mailable`'s methods, `handle()` can't
  sidestep this by staying synchronous — it's inherently real async I/O.
  `queue::dispatch(&job).await` serializes the job to JSON and inserts a
  row into a lazily self-bootstrapped `jobs` table (`CREATE TABLE IF NOT
  EXISTS`, memoized via `tokio::sync::OnceCell`, same idiom
  `larust-cache`'s `cache_items` table already uses) — durable the moment
  it returns `Ok`, independent of whether a worker is currently running.

**`xr queue:work`** claims and runs jobs until stopped. Same shape as `xr
migrate`/`xr route:list`: `larust-cli` just runs `cargo run -- queue:work`
in the app directory (`run_app_subcommand`, `crates/larust-cli/src/
main.rs`); the generated app's own `main.rs` parses the `queue:work`
argument, builds a `JobRegistry` (`.register::<J>()` per job type — panics
on a duplicate `JOB_TYPE`, since a shadowed job type would otherwise
silently stop running forever, a startup-time bug worth failing loudly
on), and calls `queue::work(registry)`. Unlike Cache/Mail's demo-only
usage, this branch ships in *every* generated app's `main.rs` template
(`crates/larust-cli/src/scaffold.rs`'s `main_rs()`) with an empty,
ready-to-extend registry — `xr queue:work` is documented framework
infrastructure the CLI promises to work out of the box, the same tier as
`migrate`/`route:list`, not an opt-in feature example.

`work()`'s claim is a single `DELETE FROM jobs WHERE id = (SELECT id FROM
jobs ORDER BY id LIMIT 1) RETURNING ...` statement — already atomic under
SQLite's own writer serialization, so nothing else can claim the same row
even with more than one `xr queue:work` process running, no separate
"reserved" state needed. A job whose handler returns `Err`, or whose
`job_type` has no registered handler, is recorded in a `failed_jobs` table
(mirroring Laravel's own) rather than retried or silently dropped.
**Documented v1 gap**: this is at-most-once, not crash-safe — a worker
killed mid-`handle()` has already claimed (deleted) the row but never
reached the `failed_jobs` insert, so that job is lost, not requeued. No
reservation/heartbeat/backoff mechanism yet (Laravel itself added this
well after its own initial queue design) — a documented future extension.

`larust-mail` itself ships one framework-owned `Job` implementation,
`MailJob` (`JOB_TYPE = "__larust_queued_mail"`, see the Mail section
above) — `MailBuilder::queue()` enqueues one. Registration is still real,
explicit source code (`registry.register::<larust_support::mail::
MailJob>()`) — nothing in this codebase's `JobRegistry` model discovers
job types on its own at runtime, framework-owned or not — but `xr new`'s
scaffold writes that line into every generated app's `queue:work` branch
by default (unlike an app's own job types, which the app author still
adds by hand), since an idle registration costs nothing if `.queue()` is
never called. Removing the line is a one-line opt-out, not a missing
opt-in.

`demo`/`examples/blog` wire a real, additive example that deliberately
never touches the existing, already-tested Mail-on-register flow:
`PostController::store` dispatches a `PostCreated` event after creating a
post; a listener registered in `main.rs` logs it and enqueues a
`NotifyPostCreatedJob`, whose `handle()` also just logs (no real external
system touched, matching Mail's `log` driver as "the safe, zero-setup
default that still exercises the real end-to-end path"). `NotifyPostCreatedJob`
needs `#[derive(Serialize, Deserialize)]`, which — like `sqlx::FromRow` —
can't be routed through `larust_support`'s facade (the derive macro
generates code referencing `::serde::...` directly), so `serde` joins
`sqlx` as a direct, blessed dependency in generated apps' own `Cargo.toml`
(`crates/larust-cli/src/scaffold.rs`'s `cargo_toml()`) — a real, narrow
exception to "one dependency surface," not a broadening of it (`Event`
payloads need no such exception, since `Event` is Clone-based, never
serialized).

## Notifications (`larust-notifications`)

`larust_support::notification::{Notification, notify, notifications_for,
unread_count, mark_as_read, mark_all_as_read}` — Laravel's
`$user->notify(new InvoiceSent($invoice))`, narrowed to **only** Laravel's
*database* notification channel, not its full multi-channel shape.

**This is a deliberate scope decision, not a gap.** Laravel's
`Notification` class has *optional* per-channel render methods
(`toMail()`, `toDatabase()`, `toBroadcast()`), decided at runtime by
`via($notifiable)` — a class simply doesn't implement the methods for
channels it doesn't use. Rust has no clean way to express "this trait
method is conditionally required based on another method's runtime return
value" without `Option`-returning defaults, and this codebase's three
closest sibling traits — `Mailable` (`subject`/`html_body`), `larust_queue
::Job` (`JOB_TYPE`/`handle`), `larust_auth::Authenticatable`
(`auth_id`/`find_for_auth`) — are all **zero-default-method traits**,
deliberately, specifically to force a compile error on a real gap rather
than a silently-unimplemented one. Building Laravel's `via()` shape here
would be the first trait in this codebase to break that convention.

`larust-mail` (`mail().to(...).send()/.queue()`) and `larust-live::push`
(`push::broadcast(channel, html)`) already fully solve "send an email" and
"push a live update" independently — wrapping them inside a unified
`Notification` dispatch would add indirection without adding capability.
So this crate doesn't try: if a notification-worthy event should also
email or live-push someone, call those APIs directly, at the same call
site, alongside `notify`:

```rust
notify(&user, &InvoiceSent { invoice_id }).await?;                    // database
mail().to(&user.email).send(InvoiceSentMail { invoice_id }).await?;   // mail, if wanted
push::broadcast(&format!("notifications.{}", user.auth_id()), ...);   // broadcast, if wanted
```

Three ordinary, independently-composed calls — no framework-level dispatch
table, no hidden dynamic dispatch deciding which method runs based on a
runtime array. `demo`/`examples/blog` demonstrate exactly this: their
existing `PostCreated` listener already fanned out by hand to two
channels (a queued `Job` and a `push::broadcast` ticker); this feature
added a third ordinary call — `notify(&author, &PostPublished {...})` —
to record a database notification for the post's own author, alongside
the other two, unchanged.

**The trait itself**, mirroring `Job::JOB_TYPE`'s exact convention:

```rust
pub trait Notification: Serialize + Send + Sync {
    const NOTIFICATION_TYPE: &'static str;
}
```

Serializing `Self` *is* the stored `data` payload — no separate render
method. No `DeserializeOwned` bound (unlike `Job`): nothing in this crate
ever reconstructs a concrete notification type from a stored row —
`notifications_for` reads heterogeneous rows across many different
notification types in one query and can only sensibly return the type tag
plus raw JSON (`StoredNotification { notification_type: String, data:
serde_json::Value, .. }`), matching Laravel's own `type`/`data` column
split.

**Storage**: a self-bootstrapping `notifications` table (`CREATE TABLE IF
NOT EXISTS`, memoized via `OnceCell`, no migration file and no explicit
startup call needed anywhere — the same lazy idiom `larust-cache`'s
`cache_items` and `larust-queue`'s `jobs`/`failed_jobs` already establish).
No `notifiable_type` polymorphic column the way Laravel's own schema has
one — this framework only ever has one app-chosen `Authenticatable` type
per app, the same assumption `Policy<U>`/`Auth<U>` already make. Also
creates an index on `(notifiable_id, created_at DESC)` — the first
framework-owned table in this codebase actually filtered and sorted by a
foreign-key-shaped column at read time, unlike `jobs` (claimed FIFO by
`id`) or `cache_items` (looked up by exact `key`).

**`notifications_for` takes a caller-supplied `limit: i64`, not a
framework-picked constant** — directly mirrors `larust_orm::QueryBuilder::
paginate(per_page: i64)`'s own real precedent in this exact crate family,
making an unbounded query structurally impossible rather than merely
discouraged. Ordered `created_at DESC, id DESC` (a tiebreak is needed —
two rows can share a `created_at` second). No cursor/`before_id`
pagination in v1, the same documented gap `paginate` itself carries.

**`mark_as_read`'s ownership check reuses `larust_auth::authorize`** —
not a silent `Ok(())` collapse. That collapse pattern exists in
`larust_auth::guard` specifically to hide an *authentication*-state
ambiguity ("not logged in" vs. "logged in as a since-deleted id" — telling
them apart helps nobody). `mark_as_read` asks a different question — "does
this specific row belong to the acting user?" — structurally identical to
`Policy<U>::update`/`delete`, whose established answer is a loud
`AppError::Http{FORBIDDEN, ..}`, matching how updating someone else's post
already responds today. A nonexistent notification id is `AppError::
NotFound`, kept distinct from the mismatched-owner case. `mark_all_as_read`
needs no such check at all — its own `WHERE notifiable_id = ?` already
makes touching another notifiable's rows structurally impossible.

No `notification::fake()`/`assert_notified()` exists yet, unlike
`Mail::fake()` — these tests hit a real temp SQLite database directly and
are already fast, so there's been no need for one; a documented future
parity item, not an oversight.

## Scheduler (`larust-scheduler`)

`larust_support::schedule::{Schedule, work}` — Laravel's `$schedule->
command(...)->daily()`, driven by `xr schedule:work` the same way `xr
queue:work` drives `larust-queue`. Genuinely greenfield: unlike Mail/Queue,
this codebase had zero prior groundwork — no `chrono`, no cron-expression
parsing, no timezone concept anywhere (`Config` has no timezone field;
every existing timestamp, e.g. `larust_queue::now_unix_secs()`, is a bare
Unix-epoch integer with no timezone semantics attached at all).

**A scheduled task is a plain closure, not a trait implemented once per
task the way `Job` is.** `Job` needs `Serialize + DeserializeOwned`
because it survives a process boundary — dispatched now, run later,
possibly by a different `xr queue:work` process, via a SQLite row. A
scheduled task runs in the exact same process, same memory, that declared
it; there's no boundary to cross, so no serialization need, so no trait.
The right precedent is `larust_events::ListenerRegistry::on<E, F, Fut>` (a
payload-carrying closure registry), not `Job` — `Schedule::cron`'s boxed
task type is the same shape minus the payload parameter. Because tasks are
inline closures, not named types, they're declared directly in the
generated app's own `main.rs`, in its `schedule:work` branch — there's no
new `app/Schedule/` directory the way `app/Jobs`/`app/Mail`/`app/Events`
exist, since those hold named types that need a home to be `use`d from
multiple call sites, and a task closure is used exactly once, at its own
registration call.

```rust
let schedule = larust_support::schedule::Schedule::new()
    .daily(|| async { /* ... */ Ok(()) })
    .hourly(|| async { /* ... */ Ok(()) })
    .cron("0 */5 * * * * *", || async { /* every 5 minutes */ Ok(()) });
return larust_support::schedule::work(schedule).await;
```

`Schedule::cron`'s own public signature never mentions `chrono`/`cron`
types at all — task closures take `()` and return `Result<(), AppError>`,
so app code using `.daily(...)` never needs either crate as a direct
dependency, satisfying "one dependency surface" even more cleanly than
Mail/Queue do.

**Fluent methods** (Laravel's own most common real usage, the same
narrow-cut philosophy `Mail::fake()`'s `assert_sent`/`assert_not_sent`
already established against Laravel's fuller assertion API): `every_minute`,
`hourly`, `daily`, `daily_at("HH:MM")`, `weekly`, `monthly`, plus `cron(expr,
task)` as a raw escape hatch for anything else (e.g. `"0 */5 * * * * *"`
for every 5 minutes — left out of the fluent set for v1 pending
confirmation that the underlying crate's step-syntax works on non-year
fields, but already expressible via the escape hatch today).
**`Schedule::cron` uses the `cron` crate's own 7-field extended dialect**
(seconds, minutes, hours, day-of-month, month, day-of-week, year) — **not**
Laravel's classic 5-field Unix cron format. `.cron(...)`/`.daily_at(...)`
panic on an invalid/malformed expression, the same fail-loud-at-startup
precedent `JobRegistry::register`'s duplicate-`JOB_TYPE` panic already
establishes — a bad schedule declaration is a real bug worth surfacing
immediately, not a silently-never-runs task discovered much later.

**No timezone support in v1** — everything runs against `chrono::Utc::
now()`, matching this codebase's already-100%-naive/UTC-only posture
everywhere else. There's no field to even hang a per-app timezone off of
yet; adding one now would be scope creep into a cross-cutting concern Mail
and session cookies would also want. A documented, deliberate v1 gap.

**Tasks due in the same tick run sequentially, in registration order** —
matching both `larust_events::dispatch`'s "runs every listener
sequentially" and `queue::process_next`'s one-at-a-time claim. A slow task
delays a same-tick sibling *and* the next tick's own check (`work()` awaits
the whole sweep before ticking again) — but this also means a task can
never overlap *with itself* across ticks for free, a safer default than
concurrent-by-default would be without an explicit `withoutOverlapping()`-
equivalent. A task returning `Err` is logged and does not stop the others
due that tick.

**The worker ticks once a second and uses `MissedTickBehavior::Skip`** —
matching the `cron` crate's own native seconds-level precision (even
though every fluent method above only offers minute-or-coarser
granularity). If a task blocks the loop for, say, 90 seconds, anything due
in that window silently does not run — it is **not** queued up and
burst-fired afterward. This matches Laravel's own `schedule:run` behavior,
not just a Rust-idiom default: Laravel's own scheduler is invoked once a
minute by an external cron entry with no catch-up mechanism either, if
that invocation's own process is still busy.

**Not safe to run as more than one process against the same app.** Unlike
`xr queue:work` (whose claim step — `DELETE ... RETURNING` — is atomic
under SQLite's writer serialization, making multiple worker processes a
supported scaling story), the scheduler has no claim/lock step at all —
`work()` just checks an in-memory `Schedule` against the wall clock. **Two
`xr schedule:work` processes watching the same app will both run every due
task, every time**, silently duplicating side effects (e.g. sending the
same email twice) rather than sharing the work. This is a documented v1
gap — Laravel itself only solved this with `onOneServer()` well after its
own initial scheduler design — but a more consequential one than most gaps
in this codebase, since the failure mode is silent duplicate side effects,
not a crash or a missed run: **run at most one `xr schedule:work` process
per app.**

`routes/console.rs` (mirroring Laravel 11's own `routes/console.php`
convention) is schedule declarations' real home: a `pub fn schedule() ->
Schedule` that `main.rs`'s `schedule:work` branch calls and hands to
`larust_support::schedule::work`. `xr new` scaffolds it (alongside
`routes/web.rs`/`routes/api.rs`, both real and `mod`-declared too — see
`docs/GOTCHAS.md`'s "`xr convert`'s demo-scaffold cleanup is a real,
silent coupling to `scaffold.rs`'s current output" entry for the one
subtlety that came out of wiring these in), and `demo`/`examples/blog`
each wire a
real, additive example there: a `.daily(...)` task that logs the current
post count, proving the closure's generic bounds (`Fn() -> Fut where Fut:
Future<Output = Result<(), AppError>> + Send + 'static`) actually compile
against a real, non-trivial closure body — the same reason `examples/blog`
is rebuilt from scratch every milestone specifically to prove the
generated template compiles end-to-end, not just that the scaffold's own
Rust source (the template strings) compiles.

Deliberately out of scope: a Laravel-Artisan-style named command registry
(`Artisan::command('name', closure)`). Nothing in the codebase implements
dispatch-by-name for app-defined CLI commands — `routes/console.rs` is
specifically a home for *schedule* declarations, not a general command
registry. Building one would be a separate, genuinely large feature (a new
crate/module, a trait, a string-keyed registry mirroring
`larust_queue::JobRegistry`'s own shape, `xr` CLI wiring to dispatch by
name) and belongs in its own future milestone.

## Filesystems (`larust-storage`)

`larust_support::storage::{local, public}` — Laravel's
`Storage::disk('local')`/`Storage::disk('public')`, as two plain functions
rather than a stringly-typed `disk(name)` lookup: this framework has no
config-driven, arbitrary disk registry to look up against (matching Cache
and Queue, both of which ship exactly one fixed driver, not a registry),
so there's nothing a runtime string lookup would add except an error path
for a typo. `local()`'s root is `storage/app/` (Laravel's own convention,
private, never served); `public()`'s root is `public/` itself — this
framework's *existing* static-file docroot
(`larust_core::Application::serve()`'s `ServeDir::new("public")`), so a
file written to `public/uploads/x.png` is already reachable at
`/uploads/x.png` with **no symlink machinery**, unlike Laravel's own
`storage/app/public` ↔ `public/storage` symlink convention — a genuine
simplification specific to this framework's layout, not a compromise.

`Disk::put`/`get`/`exists`/`delete` all validate the caller-supplied
relative path by walking `Path::components()` and rejecting anything
except `Component::Normal` — so `..`, a leading `/`, and a Windows drive
prefix are all rejected *before* the path is ever joined onto the disk's
root, and a rejected path never touches the filesystem at all. This is
deliberately not a `canonicalize()`-then-check-the-prefix approach:
`canonicalize()` requires the target to already exist, which would break
`put()` for a brand-new file. `get()` returns `Result<Option<Vec<u8>>,
AppError>`, not `Result<Vec<u8>, AppError>` — a missing file is a normal,
expected outcome (the same shape `larust_cache::get::<T>`'s own
`Result<Option<T>, AppError>` already established for a cache miss), not
routed through `AppError::NotFound` (which stays tied to "no route
matched"). `delete()` on an already-missing path is not an error, matching
`larust_cache::forget`'s identical "not an error to forget an
already-missing key" precedent.

`UploadController::store` (`demo/app/Http/Controllers/upload_controller.rs`)
is the real integration — it already did real file I/O (a bare
`tokio::fs::write` straight to `public/uploads/{filename}`, with no
abstraction at all) before this milestone, so this is a refactor of
*existing*, working code, not new demo scaffolding. All of the handler's
existing upload-specific security work (the image-type allowlist,
magic-byte verification against the declared content type, random
filename generation so a client's own filename — including its extension
— is never trusted) is untouched; only *how bytes reach disk* changed.
This refactor also fixes a real, previously-live latent bug: `APP_DIRS`
(`crates/larust-cli/src/scaffold.rs`) scaffolds `public` and `storage`,
but never `public/uploads` — a brand-new `xr new` app's `/uploads` route
would 500 on its very first request, since `tokio::fs::write` never
creates missing parent directories. This "worked" before only because
`demo`'s own `public/uploads` already existed on disk by hand.
`Disk::put()` lazily creating its parent directory
(`tokio::fs::create_dir_all`) fixes this for every future app, not just
the one that happened to have the directory already — confirmed via
`demo/tests/upload_test.rs`, the first test this upload flow has ever had.

## Reactive components (`larust-live`)

Larust's Livewire equivalent: server-rendered UI that updates in place in
the browser (`wire:model`/`wire:model.live`/`wire:click`/`wire:submit`)
without a full page reload. The single biggest design decision — settled with the user
before any code was written — is that a component's state lives **entirely
server-side, keyed by the user's session**, not round-tripped through the
client as an HMAC-signed snapshot the way Livewire itself does. Livewire
needs that client-held snapshot because PHP/Laravel is stateless between
requests (a fresh PHP-FPM process per request); Larust is one long-running
process with real, persistent (`tower-sessions-sqlx-store`) sessions
already wired up, so only an opaque component id ever needs to cross the
wire. Trade-off accepted deliberately: this ties a mounted component to
this server process/session store — revisit if Larust ever grows a
multi-server story.

**The `@wire(...)` directive.** `@wire('name')` or `@wire('name', { prop:
expr, ... })` — an `@word(...)` directive like every other (`@extends`,
`@global`, ...), not a custom HTML-tag syntax (which would need a genuinely
new parser marker-kind/attribute grammar). `larust-view`'s `Node::Wire {
name, props }` stores `props` as raw `(String, String)` pairs, same
convention as `Node::Globals` — no `syn` dependency in that crate. Needs
**no `resolve.rs` changes at all**: unlike `@push`/`@globals` (whole-chain
compile-time collection passes, explicitly rejected inside `@foreach`),
mounting a component is an ordinary runtime statement, so it composes
naturally with a real Rust `for` loop and needs no special-casing — `@wire`
is deliberately allowed inside `@foreach`/`@if`.

`larust-macros/src/view.rs`'s `Node::Wire` codegen arm is the first arm
that needs `.await`/`?` and an in-scope `session: &Session` binding — an
implicit contract on the `view!` call site, exactly like `@csrf`'s existing
`csrf_token` contract, just one binding richer. `expand()` checks for this
eagerly (`contains_wire` + a context-name scan) so a template misusing
`@wire` fails at the macro call site with a clear message, not as a
confusing "cannot find value `session`" pointing at generated code.

**Component definition.** A `WireComponent` trait (`mount`/`render`/`call`,
all `async`, spelled `-> impl Future<..> + Send` rather than `async fn` so
the `Send` bound is explicit — no `#[trait_variant]`/`async-trait`
dependency needed) — async because a real component routinely needs real
async work (`demo`'s post listing queries the database in `render`; `demo`'s
post-creation form writes to it from `call`). `mount` and `call` both
receive `session: &Session` (the same session the page's own `@wire(...)`
mount point had) — `call` so an action can resolve the logged-in user
(`larust_support::auth::id(session)`) to do real, per-user work, and
`mount` so a component can capture per-viewer identity *once*, at mount
time, rather than needing it again on every subsequent render (`demo`'s
`PostList` caches both the viewer's id, to decide whether to show Edit/
Delete controls on each post, and a CSRF token, for its own per-row delete
forms — `render` itself takes no `session` param at all, since nothing it
does needs one beyond what `mount` already cached). `call` returns
`Result<Option<String>, AppError>` — `Ok(Some(path))` is Livewire's own
`redirect()`: the client navigates the browser to `path` instead of
patching the fragment in place, for an action (typically a
`wire:submit`-triggered one) that finishes by sending the user somewhere
else entirely, e.g. to the record it just created. `Ok(None)` is the
ordinary case: re-render in place, whatever `self` mutation the action
made (including setting a validation-errors field for the next render to
display — see `demo`'s `PostForm` for a real example of both). Dispatch by
string name (session storage only ever has a name, never a compile-time
type) goes through `LiveRegistry`, modeled on `larust_events::
ListenerRegistry`'s "build via fluent chain, `.publish()` once into a
process-wide `OnceLock`" shape — not `JobRegistry`'s shape, since both
page-mount codegen and the update handler need concurrent, process-wide
read access from arbitrary request-handling tasks, `ListenerRegistry`'s
situation, not `JobRegistry`'s dedicated-worker-loop one. Each
`register::<C>()` call monomorphizes four boxed closures (`mount`/`render`/
`set_many`/`call`) that round-trip a type-erased `serde_json::Value`
through `C` via `serde_json::to_value`/`from_value` — `mount`/`render`/
`call` return `Pin<Box<dyn Future<...> + Send + '_>>` (a `Box<dyn Fn>`
can't return `impl Future` directly); `set_many` stays synchronous, since
merging a props object and round-tripping it through `C` purely as a type
check needs no async work. A component whose state has no `wire:model`
fields at all (a `wire:click`/`wire:submit`-only unit struct, which
`serde_json` serializes as `Value::Null`, not an object) is deliberately
tolerated: `set_many` skips the merge entirely when `props` is empty (the
normal case for such a component), and only rejects a genuinely
non-empty, mismatched prop payload, with a 422, not a 500.

**Session storage.** One session key, `__wire_components`, holding an
insertion-ordered `Vec<(String, StoredComponent)>` — not one key per
component. `tower-sessions-sqlx-store` already round-trips the *entire*
session blob (MessagePack, single BLOB column) on every write regardless of
how many top-level keys are touched, so per-component keys would buy
nothing on that axis while making capping/sweeping stale entries harder.
Every full-page GET through an `@wire(...)` mount point creates a
**brand-new** component instance (fresh id, freshly `mount()`-ed state) —
no cross-navigation persistence, matching Livewire's own per-page-load
semantics — so stale/orphaned entries are expected on every page view, not
a bug. Capped at `MAX_COMPONENTS_PER_SESSION = 50` (hardcoded, evicted
oldest-first), matching this codebase's "no toggle until real pressure
justifies one" stance elsewhere.

A process-wide, per-session-id `tokio::sync::Mutex`
(`larust_live::lock::with_session_lock`) guards the full read-modify-write
cycle of any `__wire_components` mutation, since the session store itself
has no per-key locking or optimistic-concurrency check — without it, two
concurrent wire-component writes under one session (two components on a
page, an overlapping double-click) could silently clobber each other.
Documented, deliberate gap: this only covers `__wire_components` writes —
it does *not* protect against racing an unrelated session write elsewhere
(a CSRF-token regen, a login), which stays last-writer-wins at the
whole-blob level, same as every other session write today. `Auth::
logout()`'s `session.flush()` wiping `__wire_components` along with
everything else is accepted as correct, not carved out.

`session.id()` is `None` until `tower-sessions` actually persists this
session's first write (minted at `save()`, not at `insert()`) — a
brand-new, first-time visitor has no id yet when `mount()` runs. Rather
than funneling every such session through one shared fallback lock key
(which would needlessly serialize unrelated anonymous first-time visitors
behind a single global lock — a real bug caught in review, not a
theoretical one), `with_session_lock` skips locking entirely in that case:
with no persisted id yet, nothing else could already hold a reference to
this not-yet-identified session to race against.

**Wire protocol.** One route, `POST /__larust_wire/{component_id}`, handles
a `wire:model`-style prop sync and a `wire:click`/`wire:submit`-style
action call together — every sync carries the component's *entire* current
`wire:model` field set (not a delta), which is what correctly threads a
deferred field's just-typed value through when a different element's
click/submit/live-sync is what actually triggers the request. Response is
`200 text/html`, the same `<div data-wire-id="...">` wrapper shape the
initial mount produces (one uniform patch target for both first paint and
every later fragment) — even when the action also requested a redirect
(below), so the component's own state is still saved and reflected
correctly if the redirect target ever reads it back. The whole sync is
atomic — if the prop merge or the action call fails, nothing is written
back. An action's `Ok(Some(path))` return is carried out-of-band as an
`X-Wire-Redirect: {path}` response header (checked by the client before it
ever reads the body) rather than folded into the body — that keeps the
response's shape (a `text/html` fragment) identical in both the redirect
and non-redirect case, instead of inventing a second, JSON-shaped response
convention just for this one case. `GET /__larust_wire/runtime.js` serves
the vendored client script (`include_str!`'d from
`crates/larust-live/assets/wire-runtime.js`, not copied into `public/js/`,
so it stays version-locked to the installed `larust-live` crate with no
upgrade-drift risk). Both routes are registered **explicitly** by the app
(or pre-populated by the `xr new` scaffold) — nothing here is auto-mounted
by `Application::serve()` the way `/__larust_dev` is.

**A second surface syntax, added later, for mounting:**
`<wire:name attr="literal" :attr2="expr" />` — Livewire's own
`<livewire:counter />` convention, added the same way and for the same
reason `<resource:name>` was added to `@resource(...)` (see "Static
template inclusion" below): not a second AST concept, just a second
spelling parsing to the identical `Node::Wire`, so `resolve.rs` and
codegen stay unaware two syntaxes exist. **Always self-closing** — unlike
`<resource:...>`, `@wire(...)` has never had a body/slot concept at all (a
mounted component renders entirely from its own template), so a
non-self-closed `<wire:name>` is a parse error, not silently treated as an
empty body. Shares its attribute grammar and scanner (`parse_tag_attrs`)
directly with `<resource:...>`. Demo example: `demo/resources/views/
posts/create.blade.xr` (`<wire:post-form />`) and `posts/edit.blade.xr`
(`<wire:post-form :post_id="post.id" />`), converted from the directive
form; `demo/resources/views/posts/index.blade.xr`'s `@wire('post-list')`
was deliberately left as-is, proving both spellings coexist in one app.
Parity proven in `crates/larust-macros/tests/view_wire_tag.rs`.

**`@larustscripts`** — Livewire's `@livewireScripts` equivalent, written
once in a shared layout (conventionally right before `</body>`, same
placement `demo`'s and the `xr new` scaffold's own `layouts/app.blade.xr`
use) rather than requiring every individual page that mounts a `@wire(...)`
component to remember its own `<script src="/__larust_wire/runtime.js">`
tag. Unlike the route registration above, this genuinely is automatic —
but it's still a compile-time decision, not a runtime branch: `larust-view`'s
`Node::LarustScripts` codegen arm in `larust-macros/src/view.rs` expands to
the script tag only when that exact template's resolved tree (itself, or —
via `@extends` — whatever page is rendering through it) contains a
`Node::Wire` anywhere, reusing the very same `contains_wire` scan that
decides whether `session` needs to be in the `view!` context at all. A page
with no `@wire(...)` gets nothing from `@larustscripts`, even though it
shares the exact same layout as a page that does — proven directly in
`crates/larust-macros/tests/view_larustscripts.rs` (two sibling pages
extending one layout, asserting the script tag appears on one and not the
other) and again against the real app in
`demo/tests/wire_post_list_test.rs`.

**Client runtime.** v1 scope: `wire:model` (deferred, sent only when
another trigger fires), `wire:model.live` (immediate, 150ms debounce),
`wire:click="action"`/`wire:submit="action"` (no arguments; `wire:submit`
intercepts the form's native `submit` event the same way `wire:click`
intercepts a click). Explicitly deferred: `.lazy`/`.throttle`/custom
debounce values, `.number`/`.boolean` coercion, action arguments. The
DOM-patch function is a small, hand-vendored function (no morphdom
dependency) — required, not optional: naively replacing `innerHTML` would
destroy focus/cursor position on the input the user is actively typing
into, breaking the exact `wire:model.live="search"` UX this feature exists
for. Attribute/property writes only happen when the value actually
differs, which is what avoids the cursor-jump bug with no separate
focus-tracking logic needed; the *currently focused* element's value is
never overwritten at all, regardless of what the response echoes back
(guards against a slower, now-stale response clobbering keystrokes typed
after that request was sent). Children are matched by position + tag (+
`id` when present) — no keyed-list reordering, since a component's own
re-render is a structurally-stable subtree, not a general list-diffing
target. `wire:ignore` (same attribute name/meaning as real Livewire) opts
an element's entire subtree out of patching — needed for any element a
*different* piece of JS manages after mount (`demo`'s post-creation form
marks its Trix rich-text editor `wire:ignore`, since Trix builds its own
real DOM children that the server-rendered HTML — which only ever contains
the empty `<trix-editor>` tag — knows nothing about; without it, every
re-render's child-diff would delete Trix's own editable surface).
`@loadonce ... @endloadonce` (see `docs/MACROS.md`) is sugar built directly
on `wire:ignore` — it wraps a block in `<div wire:ignore>...</div>` at
compile time, for markup a component wants colocated with the element it
belongs to (`demo`'s `PostForm` uses it for the `<link>`/`<script>` tags
Trix itself needs, right next to the `<trix-editor>` element, instead of
requiring the *page* that mounts the component to carry them). One
in-flight request per component id (a "resync pending" flag instead of
concurrent requests) both for correctness (an older response can't clobber
a newer edit) and as a partial client-side mitigation for the session-write
race above.

`demo` has two real, working examples. `/posts` (`demo/app/Wire/
post_list.rs`, `demo/resources/views/posts/index.blade.xr` +
`components/post-list.blade.xr`) is the Journal's own listing *and* its
live search, as one `PostList` component — `wire:model.live="query"`
filters the exact same grid the page loaded with, in place, rather than
sending the visitor to a separate search page (an earlier, standalone
`/search` page + `SearchBox` component was folded into this one directly,
on request — searching a list you're already looking at should filter it,
not navigate away from it). `PostList::mount` also demonstrates the
session-aware side of `mount`: it captures the viewer's own user id once,
at mount time, so `render` can decide per-post whether to show Edit/Delete
controls without needing `session` itself — `PostController::index` no
longer does any of this work (no author/tag lookups, no per-viewer
`can_manage`, no `posts.count` cache, all of which used to live there);
it just renders the page shell and mounts the component. `Post::title`
filtering is in-memory (no `LIKE`-style `QueryBuilder` filter exists yet,
so this is a plain demo helper, not a query-performance example). Exercised
end-to-end (unfiltered listing, live filtering, the empty state, and —
critically — that Edit/Delete only appear for a post's own author, proving
`mount`'s per-viewer caching actually works) in
`demo/tests/wire_post_list_test.rs`.

`/posts/create` **and** `/posts/{id}/edit` (`demo/app/Wire/post_form.rs`,
`demo/resources/views/components/post-form.blade.xr`) share a single
`PostForm` component rather than two near-duplicate templates — Livewire's
own usual pattern for a create/edit pair. `create.blade.xr` mounts
`@wire('post-form')` with no props; `edit.blade.xr` mounts
`@wire('post-form', { post_id: post.id })`, and `mount` populates `title`/
`tags`/`content` from the existing post whenever `post_id` is present
(falling back to an empty create-mode form if the post doesn't exist or
isn't owned by the current session's user — `mount` has no way to signal
failure, so this is defense-in-depth, not the real authorization boundary;
the page-level GET already requires `PostController::edit`'s own
`post.authorize_update(&user)` before a component ever mounts in edit
mode). `wire:model` on `title`/`tags`/`content`, `wire:submit="post"` on
the form itself, hand-rolled validation (mirroring `StorePostRequest`'s own
rules) that populates an `errors` field and re-renders in place on failure,
and on success either a real `Post::create` + `PostCreated` event dispatch
(create mode) or a direct `UPDATE posts` after re-checking the post's
`user_id` against the session's current user (edit mode — the real
authorization boundary, mirroring what `PostController::update`'s
still-existing plain-form endpoint checks via `Policy`), followed in both
cases by `Post::sync_tags_from_csv` and an `Ok(Some(path))` redirect to the
post. Exercised end-to-end (a successful publish creating a real, tagged
post and redirecting; a blank title showing the inline error and creating
nothing; edit mode prefilling from the existing row and updating it in
place rather than creating a second post) in
`demo/tests/wire_post_form_test.rs`.

`/profile` (`demo/app/Http/Controllers/profile_controller.rs`,
`demo/resources/views/profile/show.blade.xr`) is the one addition in this
area that's deliberately *not* a `@wire(...)` component — a plain
server-rendered form pair (update name/email, change password), matching
`/login`/`/register`'s own plain-form convention rather than `PostForm`'s
reactive one. `ProfileController::update_password` re-verifies the
submitted `current_password` against the session user's real hash before
allowing a change, the same "always the real authorization boundary, not
just a page-level gate" posture `PostForm::update_existing` above takes.
Exercised end-to-end in `demo/tests/profile_test.rs`.

## Static template inclusion (`@resource(...)`)

The non-reactive counterpart to `@wire(...)` — Laravel's real split, kept
deliberately: Blade components (`<x-alert>`, static, props + slots) versus
Livewire components (`<livewire:counter>`, reactive, session-backed).
`@wire(...)` is this framework's Livewire equivalent; `@resource('name', {
prop: expr, ... }) ... @endresource` is the Blade-component equivalent —
props plus a slot, resolved once at render time, no session storage, no
client JS, no round-trip at all. Full mechanics in `docs/MACROS.md`'s
`@resource` entry; this is the design summary.

Unlike `@wire(...)`'s props (which round-trip through `serde_json::Value`
because they have to survive a session-storage/JSON boundary between
requests), `@resource(...)`'s props become real `let` bindings at codegen
time — no serialization at all, since inclusion happens once, inline, in
the same codegen pass as everything around it. The slot (the captured body
between the tags) is the interesting piece: it renders in the *caller's*
own scope (so it can reference the caller's own variables, not just
whatever the included template itself received as props), into an
isolated `String`, then gets handed to the included template as a plain
`slot` variable — placed via the *already-existing* `{!! slot !!}` raw-
interpolation mechanism, not a new AST concept. The included template's
own resolved node list is then codegen'd directly into the *caller's* own
output buffer, exactly like `@if`/`@foreach` bodies already are — no
separate runtime dispatch, no registry, nothing resembling `@wire(...)`'s
`WireComponent`/`LiveRegistry` machinery at all.

Two accepted v1 limits: an included template gets no `@extends`/`@push`/
`@globals` resolution of its own (it's meant to be a small, self-contained
partial, not a full page), and `@wire(...)` used directly inside an
included template's own file — not its slot, which is part of the
caller's own tree and scanned normally — won't be picked up by
`@larustscripts`'s detection, since that scan never loads included files.

Demo example: `demo/resources/views/components/panel.blade.xr` (`title`/
`subtitle`/`extra_class` props, a slot) wraps both `<section
class="form-card">` sections on `/profile`, replacing what was previously
duplicated markup between the two — see
`demo/resources/views/profile/show.blade.xr`.

**A second surface syntax, added later, for the same feature:**
`<resource:name attr="literal" :attr2="expr">...</resource:name>` —
requested specifically because a component wrapping a substantial slot
(a whole `<form>`, say) reads more like ordinary markup this way than as a
`@resource(...) ... @endresource` pair. Deliberately *not* a second AST
concept: both spellings parse to the identical `Node::Resource`, so every
downstream stage (`resolve.rs`, codegen) is unaware two syntaxes even
exist, and a template can freely mix both — the tag form was added by
touching `larust-view/src/parser.rs` alone. Plain attributes are literal
string props (the raw text re-escaped into a Rust string literal at parse
time); a leading `:` marks an attribute's value as a raw expression instead
— Blade's own `<x-alert :message="$message">` convention. Unlike a bare
`@endresource` (which, like every other `@endXxx` closer, just closes
whichever block opened last, with nothing to check it against), a closing
`</resource:name>` tag's name *is* validated against its opening tag's —
a renamed-one-side-not-the-other mistake is a parse error, not a silent
misparse. Full mechanics, including how the closer name-matching threads
through the existing `Closer` machinery, in `docs/MACROS.md`'s tag-syntax
subsection; parity with the directive syntax is proven directly in
`crates/larust-macros/tests/view_resource_tag.rs` (asserts byte-identical
output). Demo example: `demo/resources/views/profile/show.blade.xr`'s two
`<resource:components.panel>` blocks (converted from the directive form).

## Server-pushed updates (`@live(...)`)

The third and final piece of the naming split above:
`@wire(...)` is client-initiated (something *this* browser tab does
triggers a round trip that patches *this* tab); `@resource(...)` is static
(resolved once, at render time, nothing after that); `@live(channel_expr)
... @endlive` is **server-initiated** — something that happened anywhere
at all (another user's request, a background job, an event listener) pushes
a fresh fragment to *every* tab currently subscribed to that channel,
including ones where nobody did anything. This is the one thing neither
`@wire(...)` nor a plain page reload can express, and it's the reason the
name `@live`/`@endlive` was deliberately freed up by renaming the old
reactive-component directive to `@wire` — `@live` was always meant for
exactly this ("anything that could receive web-socket like updates, like
live chat").

Deliberately the simplest of the three: **no component trait, no session
state, no server-side struct at all.** `@live`'s channel argument is a
plain string key, not a typed, registered component — the registry it
needs (`larust_live::push`'s `OnceLock<Mutex<HashMap<String,
broadcast::Sender<String>>>>`) creates a channel's `broadcast::Sender`
lazily on first use, not via any upfront `register::<C>()` call the way
`@wire`'s `LiveRegistry` requires. Unlike `@wire`/`@resource`'s `name`
(always a quoted string, since both resolve against a compile-time
registry or file path), `@live`'s `channel` is parsed as an **arbitrary
Rust expression** (`parse_paren_expr`, not `parse_quoted_string`) —
nothing keys a lookup on it at compile time, so there's no reason to
restrict it, and a dynamic per-resource channel
(`format!("post.{}.comments", post.id)`) is exactly the kind of thing this
is for.

`@live`'s body renders once, inline, **in the caller's own scope** —
same "codegens directly into the caller's output buffer, no separate
runtime dispatch" shape `@resource`'s included-template body already
uses — wrapped in `<div data-live-channel="{escaped channel}">...</div>`.
No session, no `.await`/`?` requirement on its own: mounting doesn't touch
session storage at all, so a template using only `@live` needs nothing
extra in its `view!(...)` context. `@larustscripts` gained a second,
independent scan (`contains_live`, alongside the existing `contains_wire`)
so a page gets exactly the runtime scripts the directives it actually uses
require — `@wire`-only pages get only `wire-runtime.js`, `@live`-only
pages get only `push-runtime.js`, pages using both get both.

**Why this isn't built on `@wire`'s machinery.** The two directives solve
opposite-direction problems, and `@wire`'s entire design — session-keyed
component identity, per-session storage, a `LiveRegistry` resolving a
string name to one typed component — exists to answer "which browser tab
is this, and what does its component currently look like." A push target
isn't a browser tab at all; it's just a name multiple tabs (and multiple
users) can subscribe to. Reusing `@wire`'s identity scheme would have meant
either iterating every session's stored components to find subscribers
(session storage was never designed to be iterated) or inventing a second
identity scheme anyway — a plain named channel, decoupled from any session,
is what the feature actually needs, so that's what it is.

**Server side.** `larust_live::push::broadcast(channel, html)` publishes a
fragment to every current subscriber of `channel` — a harmless no-op, not
an error, if nobody's currently listening (fire-and-forget; there is no
buffering or replay for a subscriber that connects later). `push::wrap
(channel, inner_html)` produces the *exact* `<div
data-live-channel="...">...</div>` shape `@live` itself renders — used to
build a broadcast payload that structurally matches what the client's DOM
patcher expects to find. `push::socket` is the `GET
/__larust_push/{channel}` WebSocket upgrade handler (subscribes to the
channel's `broadcast::Receiver`, forwards every message as a `Message::Text`
frame, tolerates `RecvError::Lagged` by continuing rather than dropping the
connection, and also polls the socket's own `recv()` to detect client
disconnects). `push::runtime_js` serves the vendored client script at `GET
/__larust_push/runtime.js`. Both routes are registered **explicitly** by
the app, same as `@wire`'s routes — nothing here is auto-mounted.

**Client runtime (`push-runtime.js`).** Connects one WebSocket per
`[data-live-channel]` element on the page, reconnecting after a fixed
2000ms delay on close. Its DOM patcher (`larustPushPatch`) is a
**deliberate, near-verbatim duplicate** of `wire-runtime.js`'s own
patcher, not a shared module — the two scripts are independently vendored
and served with no bundler between them, so sharing code would mean
introducing build tooling neither currently needs, for a function small
enough that duplicating it is cheaper than the alternative.

**The one thing the framework doesn't enforce:** that a channel's initial
render (via `@live` + whatever's inside it) and its broadcast payload
(built wherever `push::broadcast` is called, potentially far away in the
codebase — an event listener, a job) stay in the same shape. Nothing
prevents them from drifting apart; the app is responsible for keeping them
in sync. The mitigation, demonstrated in the demo: use the *same*
`@resource`-included template for both, so the shape is defined in exactly
one place.

Demo example: `demo/resources/views/welcome.blade.xr`'s home-page post
counter — `@live("posts.count")` wraps a `@resource('components.
post-count-ticker', { count: count })`, composing all three directives at
once (the reactive/static/push split isn't exclusive — a push channel's
contents can themselves be a static, prop-driven include). `demo/src/
main.rs`'s existing `PostCreated` event listener (already dispatching
`NotifyPostCreatedJob`) now also re-queries the post count and broadcasts
a fresh fragment rendered from the *exact same*
`components.post-count-ticker.blade.xr` template via
`larust_support::view!(...).into_html()` + `push::wrap(...)` — the initial
render and every subsequent broadcast can never drift apart, since they're
both just this one template. End-to-end proof (a real WebSocket client
subscribes, a real post gets created through the existing form flow, the
socket receives a broadcast reflecting the incremented count) in
`demo/tests/live_ticker_test.rs`.

## Zero-downtime deploys (`GracefulShutdown`, `xr restart`)

Self-orchestrated: the app manages its own restart entirely on its own —
no external supervisor, reverse proxy, or process manager required (though
nothing here conflicts with one either). The currently-running process
spawns its own replacement, hands it the exact listening socket it was
already using, waits for confirmation the replacement is genuinely
serving, and only then gracefully drains its own in-flight requests and
exits. Built and verified on both Linux and Windows — the two platforms
need genuinely different low-level mechanisms (fd inheritance across
`fork`+`exec` vs. `WSADuplicateSocket`), covered below, but present one
unified interface (`lifecycle::listener`) to everything above them.

**Everything here is opt-in**, layered in two independent steps, since
this changes real process-lifecycle behavior every existing app currently
depends on implicitly (a bare `axum::serve` that exits the instant Ctrl+C
is pressed):

```rust
Application::new()?
    .router(route.into_axum_router())
    .with_graceful_shutdown(GracefulShutdown {
        drain_timeout: Duration::from_secs(30),
        restart_channel: true,   // a second, independent opt-in
    })
    .serve().await
```

No `.with_graceful_shutdown(...)` call at all → today's exact original
behavior, byte-for-byte unchanged. `restart_channel: false` (the default)
→ graceful shutdown on Ctrl+C/SIGTERM only, no local IPC surface of any
kind — a legitimate, smaller feature entirely on its own. Only
`restart_channel: true` turns on the full dual-process handoff. The `xr
new` scaffold does **not** enable either by default — this is documented
as a deliberate step an app takes once its own drain-timeout tradeoffs are
understood, not baked into every generated app silently.

**Graceful shutdown** (`lifecycle::signal`, wired into `Application::
serve()`): `shutdown_tx` fires once, on Ctrl+C, (Unix) SIGTERM, or
(Windows) `Ctrl+Break` — see below for why Windows needs that third
trigger specifically. `axum::serve(...).with_graceful_shutdown(...)` then
stops accepting new connections and drains in-flight ones. A separate
spawned task sleeps for `drain_timeout` as a hard backstop: if the drain
hasn't finished naturally by then (a stuck connection, a hung upstream
call), the process is forced to exit anyway — never "wait forever," since
a stuck connection must not block a deploy indefinitely.

**Listener handoff** (`lifecycle::listener`, `#[cfg(unix)]`/
`#[cfg(windows)]` split behind one shared interface —
`prepare_for_handoff(listener, child_pid) -> String` /
`inherit(encoded: &str) -> TcpListener`): not SO_REUSEPORT (Linux-only;
Windows' `SO_REUSEADDR` has different, weaker semantics with no clean
concurrent-accept load-balancing). Unix: the parent duplicates its
listener's fd, clears `FD_CLOEXEC` on the duplicate via a raw
`libc::fcntl` call (std sets it by default on every socket specifically to
*prevent* fd inheritance — has to be explicitly undone), and passes the
plain fd number to the child. Windows: `WSADuplicateSocketW` — the
Winsock-sanctioned mechanism for handing a live socket to another process
by PID (used by real production software, e.g. IIS), producing a
`WSAPROTOCOL_INFOW` struct the child reconstructs a working socket from
via `WSASocketW`. Both encoded values travel over the child's own **stdin**
(`Stdio::piped()`), not an env var — `WSADuplicateSocketW` needs the
child's real PID, which only exists *after* `Command::spawn()` returns, by
which point env vars can no longer be added, but stdin can still be
written to. In the happy path there's no meaningful window where both
processes are simultaneously calling `accept()` on the shared socket — the
old process simply stops calling `accept()` once its own shutdown starts,
and by elimination every new connection goes to the replacement.

**Readiness protocol** (`lifecycle::readiness`/`lifecycle::handoff`): the
replacement writes a single marker line to its own stdout right before it
starts serving; the parent reads its child's piped stdout in the
background, bounded by a 15s timeout (`HANDOFF_READY_TIMEOUT`). If the
replacement crashes or never reports ready, the parent kills it, discards
the attempt, and keeps serving completely normally — a bad build can never
take down an already-healthy running process. `handoff::
spawn_replacement_and_wait_for_ready` is the whole orchestration in one
function: spawn, hand off the listener, wait bounded, return `Some(child)`
on success or `None` (having already killed and reaped whatever was
spawned) otherwise.

**The admin restart channel** (`lifecycle::admin`, `xr restart`): a local,
OS-native IPC listener (`tokio::net::UnixListener` on Unix, a named pipe
on Windows) — preferred over a loopback TCP port, since OS-level file/pipe
permissions give real access control for free, with no risk of colliding
with `app_port` or another local service, and nothing shows up as an open
network port to scan. Path/name is derived deterministically from
`Config::app_name` (`admin::channel_address`) — both `Application::
serve()` and `xr restart` compute it identically and independently, no
runtime negotiation needed to agree on where to find it. Protocol: connect,
send `RESTART`, read back `OK` or `FAILED`. On `RESTART`, the running
process attempts the full handoff (listener passing + readiness wait); on
success it triggers its own graceful shutdown via the exact same
`shutdown_tx` Ctrl+C/SIGTERM already use — one shutdown path, three
possible triggers.

**Release-pointer convention** (`lifecycle::handoff::resolve_binary_path`):
re-execing `std::env::current_exe()` (the process's own file) only works
if the file at that exact path has already been replaced by a new build —
which Windows won't allow while the current process still holds it open
(same constraint `xr dev` already works around — see `docs/GOTCHAS.md`).
Fixed the same way on both platforms, not just Windows: `storage/releases/
current`, a plain text file (not a symlink — Windows symlinks need
elevated privilege/Developer Mode, which can't be assumed) containing the
path of the release that should be spawned next. A real deploy lands new
builds at a fresh, versioned path (`storage/releases/<version-or-hash>/
<name>`) and updates this pointer atomically as the last deploy step —
auditable, trivially rollback-able (just point it back). Falls back to
`current_exe()` only when no pointer file exists at all — meaningful for
local dev/testing, not the real production story.

**Two real, non-obvious bugs surfaced building this, both worth knowing
about if touching this code again** (full detail in `docs/GOTCHAS.md`):
`tokio::signal::ctrl_c()` on Windows only ever resolves on a genuine
`CTRL_C_EVENT` — but an external controlling process can't reliably target
`CTRL_C_EVENT` at one specific process at all (only `CTRL_BREAK_EVENT`
can), so `wait_for_termination` also listens on `tokio::signal::windows::
ctrl_break()` specifically to make that a viable trigger; and a
handoff replacement's own admin-channel boot races its still-shutting-down
predecessor for the same exclusive Windows named pipe name, needing a
short bounded retry rather than failing outright on the first attempt.

**Verification.** Every stage of this feature has its own real subprocess-
based integration test under `crates/larust-core/tests/` — this is
process-lifecycle behavior that a plain `#[tokio::test]` genuinely cannot
exercise (no real process spawning, no real signal delivery):
`graceful_shutdown.rs` (a real in-flight request survives a real
termination signal), `listener_handoff.rs` (two real processes share one
kernel socket), `handoff.rs` (happy path, immediate-crash, and
never-reports-ready-so-it-times-out, all as real spawned processes), and —
the one that actually substantiates "zero-downtime" as a claim, not just
as an architecture — `zero_downtime_restart.rs`: a real app process serves
continuous real HTTP traffic from a background thread while the exact
`RESTART` command `xr restart` sends is issued against it, asserting
**zero** failed requests across the entire live handoff, exactly two
distinct process pids having served traffic (a genuine handoff, not the
same process surviving), and the original process exiting cleanly on its
own once its drain completes.

### `xr dev`'s zero-downtime reload

`xr dev` (`crates/larust-cli/src/dev.rs`) is a consumer of the exact same
machinery above, not a separate mechanism — the original design killed the
previous server *before* every rebuild (a Windows file-lock workaround,
see `docs/GOTCHAS.md`), which meant every save made the site briefly
unreachable. That's fixed now: the running server is never killed before a
rebuild, and a broken build no longer takes the site down at all — for
*every* rebuild, including the very first one (see "the first-build
placeholder" below; this used to only hold once some build had already
succeeded).

**Release slots, not `target/debug/<name>.exe` directly**
(`release_slots.rs`): every successful build is copied to a fresh,
monotonically-increasing path — `storage/releases/dev-1.exe`, `dev-2.exe`,
… — and `storage/releases/current` is updated to point at it, reusing
`lifecycle::handoff::resolve_binary_path`'s own pointer convention
unchanged. The server is always spawned from its own copy, never from the
exact file the linker just wrote to, so the *next* build's linker is
always free to overwrite `target/debug/<name>.exe` regardless of whether a
server is still running from an earlier copy. Slots are never reused
across generations (see `docs/GOTCHAS.md` for why a 2-slot rotation isn't
safe); old slots are pruned best-effort, keeping the last few generations.

**`ServerState`** (`dev.rs`): `NotStarted` → `Direct(Child)` (generation
1, spawned directly, `xr dev` holds its handle) → `HandedOff` (generation
≥2, handed off to over the admin channel — `xr dev` no longer holds any
handle at all, since the replacement was spawned by its *predecessor's*
own admin loop, entirely outside `xr dev`'s own process tree). Every
generation after the first: build (old process keeps serving, completely
unaffected, for the whole build), publish the new release slot, then send
`RESTART` to whichever process currently owns the admin-channel address —
reusing the exact protocol `xr restart` speaks (`admin_client.rs`, shared
between both).

**The first-build placeholder** (`dev_placeholder.rs`): the guarantee
above ("the previous build keeps serving through a failed rebuild") only
ever covered generation ≥2 — before any build had ever succeeded,
`ServerState` was `NotStarted`, meaning nothing had bound the port at all;
a failing first build meant a bare connection-refused, indistinguishable
from the whole app being broken. `xr dev` now binds the app's own port
itself, before ever invoking `cargo build`, and serves a small built-in
"building…"/"build failed: `<error>`" page (`503`, hand-rolled HTTP/1.1
over a raw `TcpStream` — no `axum`/`larust-http` dependency added to
`larust-cli` for a single fixed response) from that socket until the
first successful build takes over. The handoff is the **same** mechanism
described above, not a second one: `bind_placeholder` clones the bound
listener before setting either handle non-blocking — mirroring
`Application::serve()`'s own `std_listener`/`admin_listener` split
exactly — keeping one blocking handle in `DevState.placeholder_listener`
purely so `advance()`'s `NotStarted` arm can later call
`handoff::spawn_replacement_and_wait_for_ready(&listener, slot,
READY_TIMEOUT)` directly, the exact same call `lifecycle::admin::
run_until_command`'s `RESTART` branch already makes on generation N's
behalf. One real wrinkle this surfaced: that call's spawned `Command`
only ever explicitly sets `LARUST_INHERIT_LISTENER`, relying on normal
env inheritance for `LARUST_DEV_RELOAD` — gen 2+ already gets it for free
because each parent generation had it set on itself, but `xr dev`'s own
process never did (it only ever set that var *on the children it spawned*
before this). Fixed by having `run()` set `LARUST_DEV_RELOAD=1` on its
*own* process environment as its first statement, before any other
thread/task exists, so generation 1 inherits it exactly the way every
later generation already does. A side effect worth noting: this also
means generation 1 now gets the same readiness confirmation generation
2+ already had — the previous plain `Command::spawn()` path never waited
for any signal that the binary actually came up before reporting success.

**`STOP` command** (`lifecycle::admin::STOP_COMMAND`): once `xr dev` has
handed off past generation 1, it has no `Child` handle to kill on Ctrl+C,
and OS signals can't reliably target "whoever is currently listening"
either (same reasoning as the two Windows signal bugs above). `STOP` asks
the running process to drain and exit with no replacement spawned at all
— address-based, so it reaches the right process regardless of how many
handoffs have happened since `xr dev` last held a real handle.

**Auto-enabled under `LARUST_DEV_RELOAD`**: `Application::serve()`
synthesizes `GracefulShutdown { drain_timeout: DEV_DRAIN_TIMEOUT,
restart_channel: true }` purely from that env var being set — no
app-level `.with_graceful_shutdown(...)` call required — but never
overrides an app author's own explicit call. `DEV_DRAIN_TIMEOUT` (2s) is
deliberately much shorter than the 30s production default: `/__larust_dev`
(`dev_reload.rs`) is an infinite SSE stream by design, so its connection
can only ever be closed by the drain timeout's hard backstop, and the
browser's reload detection depends on that connection actually dropping
promptly.

**Verification**: `crates/larust-cli/tests/dev_e2e.rs` is the test that
substantiates "zero-downtime `xr dev`" as a real claim the same way
`zero_downtime_restart.rs` does for production restarts — spawns `xr dev`
itself against a small, hand-authored fixture app
(`tests/fixtures/dev_app/`, its own standalone `[workspace]` so it's never
swept into this repo's own), drives continuous real HTTP traffic through a
real rebuild triggered by a real file edit, and asserts zero failed
requests plus a genuine pid change. A second test,
`xr_dev_serves_a_placeholder_page_when_the_first_build_fails`, covers the
gap the first test can't (it always starts from an already-working first
build): it deliberately breaks the fixture's `src/main.rs` before ever
starting `xr dev`, confirms a `503` placeholder page answers the port
almost immediately (well before the doomed first `cargo build` even
finishes), then fixes the file and confirms the real app takes over
normally. Both marked `#[ignore]` (real `cargo build`s, the first from an
empty target dir) — run explicitly with `cargo test -p larust-cli --test
dev_e2e -- --ignored --nocapture`.

## Laravel conversion (`larust-convert`, `xr convert`)

`xr convert <laravel-app-path> --out <path>` — the last of the four
v0.2/v0.3 roadmap items from `rust-laravel.md`, which itself names this as
the project's central risk: "Trying to launch with a Rust equivalent of
all of Laravel/Livewire/Horizon/Telescope/Filament would probably prevent
the project from ever launching." Two scope decisions, made before any
code was written, keep this from happening:

1. **Third-party (composer) packages are never auto-ported.** A small,
   hand-curated mapping table (`larust-convert::composer`'s `TIER_1`
   const, a package name → a note pointing at its Larust equivalent) is
   populated deliberately over time, the same one-at-a-time way
   `larust-mail`/`larust-queue`/`larust-scheduler`/`larust-notifications`
   were each built as individual crates — never auto-generated PHP-to-Rust
   translation of a package's own internals. It ships **empty at launch**:
   its value is the detection mechanism, not a pre-built library of ports.
   Everything in `composer.json`'s `require` not in that table is named,
   with its version constraint, in the generated report — never silently
   dropped, never guessed at. `laravel/framework` itself is excluded from
   this report: it isn't a third-party dependency to port, Larust *is* its
   wholesale replacement.
2. **PHP business logic is never auto-translated — only mechanically
   regular *structure* is.** `rust-laravel.md`'s own assessment rates
   "Automatic source conversion: moderate" and "Literal PHP source
   compatibility: very low." A converter that *looks* like it converted a
   method body but got it subtly wrong is worse than an honest gap —
   the same reasoning behind every zero-default-method trait in this
   codebase (`Mailable`/`Job`/`Authenticatable`/`Notification`): force a
   compile error (or here, a loud report entry) over a silent one.

### Four phases — fully mechanical only, now all shipped

Split into phases up front, given the size (a real parser dependency, and
the one feature where a bug produces *plausible-looking wrong code* rather
than a compile error — the opposite of every other design decision in this
codebase):

- **Phase 1 (shipped)**: composer package report, routes, migrations,
  config — described below.
- **Phase 2a (shipped)**: form-request validation rules — described below.
- **Phase 2b (shipped)**: Blade templates — described below. Split out
  from 2a after a Plan-agent review found the two pieces have very
  different risk profiles: form-requests reuse grammar Phase 1 already
  empirically verified and fail safely **per-field** — a bad rule on one
  field doesn't affect the rest of the struct. Blade needed genuinely
  new, unverified tree-sitter-php grammar discovery (binary/unary/
  ternary/property-access node kinds) for a from-scratch PHP-expression-
  to-`syn::Expr` translator, and can only fail safely **whole-file** — a
  bad translation there breaks the *converted app's own compile*, not
  just a report entry.
- **Phase 3 (shipped)**: models (fields + relationships), controllers +
  policies (original method bodies preserved as comments), events + jobs
  (constructor-property extraction) — described below. The last of the
  four v0.2/v0.3 roadmap items; sequenced last since model-field
  resolution needs Phase 1's migration output already solid. A design
  review split this into four further sub-pieces (model fields, model
  relationships, controllers+policies, events+jobs) by the same
  per-field-vs-whole-item safety axis that split 2a/2b, all four built in
  one pass.

### Parsing foundation: `tree-sitter-php`

Chosen over two real alternatives, not just the obvious first hit:
`php-parser` (crates.io) is a single-maintainer crate first released
December 2025 with no independent evidence backing its own
"production-grade" claim; `php-parser-rs` ships explicitly alpha with an
unstable API. `tree-sitter-php` (628k downloads/month, 423 dependent
crates, published under the `tree-sitter` GitHub org by tree-sitter's own
creator) is also the better conceptual fit regardless of adoption: its
error-tolerant CST still produces a walkable tree with a detectable
`ERROR` node for a syntax-error-adjacent chunk of real-world PHP, rather
than aborting the whole file — the property "never silently mistranslate"
depends on. `larust_convert::php` wraps it — every converter matches
structure via tree-sitter's own query language (`.scm` patterns) for
simple cases, and via direct `Node` traversal (`php::walk_call_chain`,
which unwraps a `$table->id()->nullable()`-shaped or
`Route::get(...)->name(...)`-shaped chain of arbitrary depth) for anything
tree-sitter's query language can't express in one fixed pattern.

### Crate structure: `larust-convert`, depended on only by `larust-cli`

Never wired into `larust-support`'s facade — this is build-time/dev
tooling, never a generated app's own runtime dependency, so it sits
outside the "one dependency surface" rule entirely (that rule governs
what *apps* depend on; it says nothing about `larust-cli`'s own
dependencies, despite the crate table's "no codegen dependency" note for
`larust-cli` — that note describes the scaffold *templates*, which really
are plain strings with no templating engine, not a ban on `larust-cli`
acquiring a parsing dependency).

`larust-convert::codegen` — `generate_file`/`append_to_mod_rs`/
`validate_identifier`/`to_snake_case`/`pluralize` — used to be private
functions in `larust-cli::generate`. They moved here, `pub`, because
`larust-cli` depends on `larust-convert` (not the reverse), so `xr
convert`'s controller-stub generation couldn't reach them as private
functions in a crate that depends on it. `xr make:*` (`larust-cli::
generate`) now calls `larust_convert::codegen::*` too — one source of
truth for "write a generated file and wire it into the module tree"
(real edge-case handling: rollback-on-failure, placeholder-collision
guards) instead of a second copy nothing would keep in sync.

### What Phase 1 actually converts

- **Composer packages** — `composer.json`'s `require` (plain JSON,
  `serde_json`, no PHP parsing needed) classified against the tier-1/
  tier-2 table above.
- **Routes** (`routes/web.php`, `routes/api.php`) — `Route::get/post/put/
  patch/delete('path', [Controller::class, 'method'])->name(...)` and
  `Route::resource(...)` (expanded into the same 7 entries Laravel's own
  resource routing produces, using a small singularize heuristic — the
  inverse of `codegen::pluralize` — for the path parameter, since that
  mirrors Laravel's own actual inference rather than guessing beyond it).
  **`Route::middleware(...)->group(...)`/`Route::group(...)` are never
  converted** — mapping a middleware name to a real Larust middleware
  function requires knowing whether the app's own aliases match Laravel's
  stock ones, exactly the semantic judgment call this phase avoids.
  Silently dropping the group wrapper and registering its routes
  unprotected would be worse than not converting them, so every route
  inside a group is flagged for manual review and never emitted into the
  compiling route chain. A route whose action is a closure (Laravel's own
  default `routes/web.php` starts with exactly one) is flagged the same
  way — the closure body is business logic.
- **Migrations** (`database/migrations/*.php`) — `Schema::create`/
  `Schema::table` + `Blueprint` calls, mapped to Larust's actual migration
  format: raw SQL files (`NNNN_snake_case.sql`, filename-sort order — see
  `larust_orm::migrate` — not a DSL). Column-type mapping verified against
  the real files under `demo/database/migrations/`: `id()` →
  `INTEGER PRIMARY KEY AUTOINCREMENT`, `string`/`text` → `TEXT NOT NULL`,
  `integer`/`bigInteger`/`boolean` → `INTEGER`, `foreignId(...)
  ->constrained()` → `INTEGER NOT NULL REFERENCES {table}(id)` (inferring
  the referenced table from the column name when no explicit table
  argument is given, the same way Laravel itself does), `->nullable()`/
  `->default(...)`/`->unique()` as modifiers, `->primary([...])` as a
  trailing `PRIMARY KEY (...)` line (a pivot table's composite key).
  **`$table->timestamps()` is emitted but never counted as fully
  converted** — grepped, zero matches for `timestamps`/`created_at`/
  `updated_at` in `larust-macros`: this framework has no automatic
  `created_at`/`updated_at` population anywhere, so counting it as a
  silent success would misleadingly imply Eloquent's auto-touch behavior
  carried over. Every migration using it gets an explicit manual-review
  note instead. A Blueprint method this phase doesn't recognize (anything
  beyond the list above — `softDeletes()`, `json()`, `dropColumn()`, ...)
  is skipped and named in the report, never silently omitted from the
  generated table.
- **Config** (`config/*.php`) — Laravel's config system takes an
  arbitrary set of dotted keys across many files; `larust_core::Config` is
  a **small, fixed, known struct**, not an arbitrary-key system. Only
  flat, top-level `'key' => value` pairs matching a hand-curated
  `laravel.key` → `Config` field table (`app.name`, `app.env`, `app.debug`,
  `app.url`, `mail.default` → `mail_driver`, `session.secure` →
  `session_secure_cookie`) get written into `config/app.toml`; a value
  that's itself a nested array (Laravel's real `config/mail.php` nests
  SMTP settings under `mailers.smtp.*`) is reported as unsupported nesting
  rather than chased — a documented Phase 1 limitation, not a silent gap.
  `env('VAR', default)` and `(bool) env('VAR', default)` are unwrapped to
  their fallback value, since `config/app.toml` has no environment-layer
  equivalent to `env()` itself.
- **Minimal controller stubs** — a converted route needs *something* real
  to reference to compile at all. `xr convert` generates a bare
  `struct Foo; impl Foo { pub async fn bar() -> &'static str { todo!() }
  ... }` shell for every controller/method pair a converted route
  references (only the methods actually referenced, not always all 7 REST
  actions the way `xr make:controller --resource` does) — this is *not*
  Phase 3's real work (preserving each method's original PHP body as a
  reference comment); it's the minimum structural byproduct needed for
  Phase 1's own output to compile, and it's unconditionally flagged in the
  report alongside every other controller-shaped gap, never counted as
  converted.

`xr convert` calls `scaffold::new_app` first for a real, already-tested
skeleton (`Cargo.toml` with correct path deps, every directory's `mod.rs`
pre-created, `src/lib.rs` module wiring) rather than reimplementing any of
that — then deletes `new_app`'s demo-specific content (a `PostController`,
a `Post` model, one migration, one form request, one integration test,
and 4 demo Blade templates — `layouts/app.blade.xr`, `welcome.blade.xr`,
`posts/index.blade.xr`, `posts/create.blade.xr`; see `convert.rs`'s
`remove_demo_scaffold`) before layering the real converted content on top.
**This is a real, deliberate coupling to `scaffold.rs`'s current output**
— if that module's demo content ever changes, `remove_demo_scaffold`'s
file list needs a matching update, or a stale demo file (or a broken
`mod.rs` reference to a deleted one) leaks into every converted app. The
4 Blade paths were a real, shipped gap until a Phase 2a review caught
them — without them, every app converted with Phase 1 alone ended up
with Larust's own branded marketing templates sitting in
`resources/views/`, indistinguishable from real converted output, exactly
the "plausible-looking wrong" failure this tool exists to prevent.
`src/main.rs` itself is built from a template
`convert.rs` owns independently (not spliced into `scaffold.rs`'s own
generated text, which is demo-content-specific and whose consts are
private) — this deliberately duplicates the small, genuinely universal
runtime-bootstrap boilerplate every Larust app needs (`connect_database`/
`print_routes`/the `migrate`/`queue:work`/`schedule:work` branches), since
that's Larust's own runtime wiring, not anything derived from the source
Laravel app.

### What Phase 2a converts: form-request validation rules

`larust_convert::requests` — `app/Http/Requests/*.php` (a `FormRequest`
subclass's `rules(): array` method) → `#[derive(FormRequest)]` +
`#[validate(...)]` (`crates/larust-macros/src/form_request.rs`). Found
via the same `array_creation_expression` shape Phase 1 already verified
for migrations' `->primary([...])`, plus ancestor-walking
(`php::find_ancestor`, new in this phase) from a candidate `return
[...]` up to its enclosing `method_declaration` (checked by name —
`rules`) and that method's enclosing `class_declaration` (the struct
name). Both Laravel rule forms are parsed — pipe-string
(`'required|email'`) and array (`['required', 'max:255']`), which real
Laravel code mixes interchangeably even within one `rules()` array.

**Rule-token granularity is per-field, not whole-file** — the opposite of
Blade's planned whole-file safety (see above), and deliberately so: each
`#[validate(...)]` attribute is independent Rust syntax, so a field with
one unsupported rule (`unique:*`, or anything else this phase doesn't
recognize — `numeric`, `in:...`, `nullable`, `date`, custom rule classes,
...) simply emits without that rule, flagged by name (file, field, exact
dropped rule token) — every other field, and every other rule on the
*same* field, is unaffected. A field whose every rule was unsupported
still gets emitted, bare (`pub category_id: String,` with no
`#[validate(...)]` at all) rather than dropped — the flag carries the
"needs attention" signal, not the field's absence.

**Field names are a real correctness risk, not a naming preference —
this is the one place this phase deliberately does *less* than it could,
on purpose.** `#[derive(FormRequest)]`'s generated code uses a field's
own Rust identifier, verbatim, as the literal HTTP form key it looks up
(`raw.get(field_name)`) — there's no separate "wire name" concept.
Snake-casing a Laravel key like `firstName` to `first_name` would
silently change which submitted form field the generated code actually
reads — a correctness bug hiding behind what looks like a cosmetic
rename. So this converter **never transforms** a rules() key to make it
a valid Rust identifier: a key that isn't already valid verbatim is
flagged and the field is skipped, never emitted under a guessed name. A
dotted or wildcard key (`address.city`, `items.*.name`) is a different,
structural gap — Laravel's nested-array form validation has no
representation at all in the flat-`String`-field model — always flagged,
never emitted under any name, distinct category from a dropped rule. The
**class name** (`StorePostRequest` → struct name) is the one place this
converter's failure mode is whole-file, not per-field — there's nothing
to emit a field list into if the class name itself isn't a valid Rust
identifier.

### What Phase 2b converts: Blade templates

`larust_convert::blade` — `resources/views/**/*.blade.php` →
`resources/views/**/*.blade.xr`, the first converter needing **recursive**
directory discovery (`larust_convert::discover::find_files_recursive` —
Phase 1/2a's migrations/config/requests directories are all flat).

**Whole-file safety, the deliberate opposite of Phase 2a's per-field
granularity.** `crates/larust-macros/src/view.rs`'s `view!` macro parses
every captured `{{ }}`/`@if(...)`/`@foreach(...)` expression directly via
`syn::parse_str::<syn::Expr>`, with **zero** PHP-to-Rust translation at
that layer — this converter is 100% responsible for producing valid Rust
syntax, and a wrong translation would break the *converted app's own
compile*, not just show up as a report entry. There's no safe way to omit
one bad directive from the middle of a template the way a bad
`#[validate(...)]` rule can be dropped from one field — doing so would
either silently change rendered output (worse than a compile error, could
ship unnoticed) or risk a syntax error. So: `larust_convert::blade::scan`
scans a template into a flat sequence of literal text / directive /
interpolation segments (no nested AST — Larust's directive grammar
mirrors Laravel's closely enough for the supported subset that
translating each segment in place and re-emitting linearly is sufficient).
**If any segment fails, the entire file is rejected** — copied
byte-for-byte, original `.blade.php` extension kept, into
`resources/views_needs_manual_conversion/` at the same relative nesting
(so nothing downstream could mistake it for real converted output),
flagged with the specific triggering construct. Only a template where
every segment translates cleanly gets written to the mirrored
`.blade.xr` path.

**Directive grammar**: `@extends`/`@section`/`@endsection`/`@yield`/
`@if`/`@elseif`/`@else`/`@endif`/`@foreach`/`@endforeach`/`@push`/
`@endpush`/`@stack`/`@csrf` translate. `@foreach($posts as $post)` needs
real restructuring, not just a token swap — Larust's own grammar is
`@foreach(post in posts)`, both the connector word (`as` → `in`) *and*
the operand order (iterable-then-binding → binding-then-iterable) differ.
A recognized-but-unsupported Laravel directive (`@include`, `@php`,
`@switch`, `@auth`/`@guest`, `@can`, `@isset`/`@empty`, `@method`,
`@error`, `@each`, `@component`, `@while`/`@for`, Blade `<x-...>`
components, ...) is matched against an explicit keyword list specifically
so it's named in the rejection reason rather than silently mis-scanned —
and specifically *not* treated the same as an unrecognized `@` (e.g. one
character of an email address like `user@example.com`), which is
correctly left as literal text.

**The safe PHP-expression-to-`syn::Expr` subset** (`larust_convert::
blade::expr`) — every node kind was verified empirically first (a
throwaway `examples/inspect.rs` dumping `to_sexp()` against literal
samples, the same technique Phase 1/2a used), not guessed, and two real
findings corrected the original design sketch: `empty(...)`/`isset(...)`
turned out to be plain `function_call_expression`s, not dedicated
intrinsic node kinds — there's no "excluded for free," `empty` needs an
explicit function-name check (and `isset` is correctly excluded by that
same check simply never matching it). And **every** binary operator
(`&&`, `==`, `.` string concatenation, `and`, `??`, all of them) shares
the exact same `binary_expression` node kind — the operator itself has to
be recovered from raw source text between the `left` and `right` fields,
never from the node kind alone.

Translated: `$var` → `var`; property chains (`->` → `.`); bool/int/float/
plain-string literals (a PHP interpolated string is a *distinct*
`encapsed_string` node kind, never matching the plain-`string` arm, so
it's excluded structurally rather than by a content heuristic); unary
`!`; parenthesized grouping; `&&`/`||`/comparison operators (PHP
`===`/`!==`/the rare `<>` all collapse to Rust's single `==`/`!=`)/
arithmetic operators; `empty($x)`/`!empty($x)` → `.is_empty()`/
`!(...is_empty())`; ternary → `if cond { a } else { b }` (confirmed
against real usage: `demo/resources/views/layouts/app.blade.xr` already
uses this exact inline-if-expression idiom in Larust's own *output*).
Every recursively-translated sub-expression is defensively parenthesized
when spliced into its parent — cheap insurance against a PHP/Rust
operator-precedence mismatch producing a syntactically valid but
semantically wrong translation, the one residual risk node-kind checking
alone doesn't catch.

Never translated (the whole file is rejected): any other function/method
call, string concatenation, `??`, `isset(...)`, array/index access
(`$x['y']`), the bare `null` literal, `and`/`or`/`xor` keyword-form
operators, interpolated strings.

**Self-checks its own output before ever trusting it**: `translate_
expression` calls `syn::parse_str::<syn::Expr>` on its own translated
text and returns `None` (the ordinary "unsupported, reject the whole
file" path) if that fails — turning a translator bug into a normal report
entry instead of a syntax error surfacing three layers away, inside the
converted app's own `cargo build`. This is why `larust-convert` now
depends on `syn` as a real (non-dev) dependency, not just `larust-macros`.

**A known, accepted testing blind spot**: even with this phase shipped,
the integration test's `cargo build` doesn't exercise `view!`'s own macro
expansion against converted Blade output, since Phase 1/2a's controller
stubs have zero `view!(...)` call sites (Phase 3's job — wiring a real
controller to actually call `view!("posts.index", {...})`). The
`syn::parse_str` self-check is the primary gate against this phase's one
real risk (a syntax error breaking the consuming app's build); it doesn't
need macro-expansion coverage to make that guarantee.

### What Phase 3 converts: models, controllers, policies, events, jobs

Four sub-pieces, split by the same per-attribute-vs-whole-item safety axis
that split Phase 2a/2b, all built and wired together in one pass.

**Model fields (whole-struct safety)** — `larust_convert::models::{schema,
fields}`. Deliberately reads Phase 1's **own already-converted `.sql`
migration output**, not raw PHP and not `migrations.rs`'s private `Column`
struct: Phase 1 already decided which Blueprint columns survive
conversion, and re-deriving fields from raw PHP independently could
disagree with what Phase 1's migrations actually create — exactly the
`sqlx::FromRow`/`SELECT *` mismatch this whole-struct gate exists to
prevent. `schema::accumulate_schema` replays every `.sql` file touching a
table, in filename-sort order (matching `larust_orm::migrate`'s own apply
order), since a table's real column set can span multiple migration files
(`ALTER TABLE ... ADD COLUMN`, verified against `demo/database/
migrations/`). `fields::map_columns` is a small, closed SQL→Rust table
(`INTEGER PRIMARY KEY AUTOINCREMENT` → `i64` + `#[primary_key]`,
`INTEGER`/`TEXT` → `i64`/`String` or `Option<...>` by nullability); any
other SQL type rejects the **entire model** — a converted app's model
struct is load-bearing for every query it participates in, so a
partially-wrong struct is worse than no struct. **A permanent, accepted
gap**: Phase 1's own `classify_chain` already maps both `boolean()` and
`integer()`/`bigInteger()` Blueprint calls to the identical SQL type
`INTEGER` — the distinction is unrecoverably lost by the time this phase
reads the output, so every `INTEGER` column becomes `i64`, never `bool`.
Table name resolution: an explicit `protected $table = '...'` property
always wins; otherwise `codegen::pluralize`/`to_snake_case` of the class
name, reusing Phase 1's existing helpers.

**Model relationships (per-attribute safety)** — `larust_convert::
models::relations`. A relationship method must be **exactly** one
`return $this-><verb>(...);` statement (anything else — multiple
statements, a condition, a chained call — is skipped/flagged, not
guessed at) to become a `#[belongs_to(...)]`/`#[has_many(...)]`/
`#[has_one(...)]`/`#[belongs_to_many(...)]` attribute. Explicit Laravel
arguments are used verbatim; an omitted argument is filled via Laravel's
own default-argument conventions — **verified directly against
`laravel/framework`'s real 11.x source** (`Concerns/HasRelationships.php`),
not worked from memory, since a Plan-agent design review's own
recollection was initially wrong on one point and flagged its own
uncertainty:
- `belongsTo()`'s default FK is `snake_case(the relationship *method's*
  own name) + "_id"` — **not** the related class's name
  (`guessBelongsToRelation()`'s debug-backtrace of the calling method).
  Matters for disambiguation: `Post::author()`/`Post::editor()`, both
  `belongsTo(User::class)`, default to `author_id`/`editor_id`, not
  `user_id` for both.
- `hasMany()`/`hasOne()`'s default FK is `snake_case(the *declaring*
  model's own class name) + "_id"` (`getForeignKey()`).
- `belongsToMany()`'s default pivot table is
  `sort([snake_case(related), snake_case(declaring)]).join("_")`
  (`joiningTable()`, no singularize/pluralize step — Eloquent class names
  are already singular); default pivot keys are each side's own
  `{model}_id`.

A relationship whose shape isn't recognized at all (`morphTo`,
`hasManyThrough`, a multi-statement body, ...) is flagged in the report
without rejecting the rest of the model — unlike field typing, a wrong or
missing relationship attribute doesn't corrupt the struct's other fields,
and `belongs_to` specifically gets a real compile-time backstop from
`larust-macros` itself (it rejects a foreign key that doesn't name a real
`i64` field). `hasMany`/`hasOne`'s FK and `belongsToMany`'s table/pivot
keys have **no** compile-time backstop (pure runtime SQL strings), so
every *inferred* (not explicit-in-source) value gets an
`// inferred from Laravel's default naming convention — verify` comment
directly above the attribute.

Every relationship attribute references its related type bare
(`#[belongs_to(User, ...)]`) — `models::render` collects every relation's
related-type name (`relations::related_type_name`), dedupes, and emits a
`use crate::models::{...};` import line (excluding a self-reference); a
real integration-test failure caught this missing import before it
shipped.

**The `User` model's `Authenticatable` impl** — `larust_support::
auth::Policy<U: Authenticatable>`'s trait bound means a converted `User`
model needs to satisfy `Authenticatable` before *any* generated policy
compiles against it. `models::render` special-cases a class literally
named `User` (matching Laravel's own default `config('auth.providers.
users.model')` convention) and appends the same two-line delegation
`scaffold.rs`'s own `USER_MODEL_RS` template already uses (`fn auth_id
(&self) -> i64 { self.{primary_key} } async fn find_for_auth(id: i64) ->
Result<Option<Self>, AppError> { Self::find(id).await }`) — using the
model's own resolved primary-key field name, not a hardcoded `id`. Also
caught by the integration test's `cargo build` check, not anticipated in
the original design.

**Controllers + policies (method bodies preserved as comments)** —
`larust_convert::{controllers, policies}`, both built on a new shared
`php::find_method(tree, source, class, method)` primitive. `controllers::
convert` enriches Phase 1's already-generated `todo!()` stubs (still
driven by `routes::referenced_controllers` — not re-derived) with each
stubbed method's real PHP body preserved as a comment directly above the
stub, when the real source file/method exists; falls back to Phase 1's
bare stub otherwise, so a missing or malformed controller file never
blocks the rest of the app from compiling. `policies::convert` maps
Laravel's camelCase ability methods (`viewAny`/`view`/`create`/`update`/
`delete`) to Larust's fixed 5, mirroring `xr make:policy`'s own
`POLICY_TEMPLATE` exactly (deny-by-default `false`, same `{model}_policy`
filename convention, same `export: None` — a policy's `impl` block has
nothing nameable to re-export). Model type name inferred by stripping a
`Policy` suffix from the class name; user type fixed to `User`, matching
`xr make:policy`'s own `--user` default. Neither module translates a
single line of logic — the original body is a comment, the stub
underneath is always `false`/`todo!()`.

**Events + jobs (constructor-property extraction)** — new
`larust_convert::constructor_props`, genuinely new parsing territory
(nothing in `php.rs` read `formal_parameters`, promoted-property
visibility modifiers, or `$this->x = $x;` assignment shapes before this).
Detects **both** real Laravel constructor styles — modern promoted
properties (`public function __construct(public int $postId) {}`) and
the older explicit-property-plus-assignment style (`public $postId;
public function __construct(int $postId) { $this->postId = $postId; }`)
— producing the same flat field list either way. **Only the 4 PHP scalar
primitives map** (`int`→`i64`, `string`→`String`, `bool`→`bool`,
`float`→`f64`); a class type hint (e.g. `public Post $post`, a common,
valid Laravel pattern for "the model this event is about") is
**rejected**, not mapped through as a bare type name — an earlier design
draft did map it through, and a real integration-test failure caught why
that's unsound: nothing this phase converts guarantees the referenced
type satisfies whatever the containing struct's own derive needs
(`Event`s need `Clone`, `Job`s need `Serialize`/`Deserialize`;
`#[derive(Model)]` provides neither). The real, hand-authored `demo/app/
Events/post_created.rs` confirms the actual convention is `post_id: i64`,
not the model itself — the fixture and this module's behavior now match
it. `optional_type`/`union_type` hints are rejected the same way they are
in `blade::expr` — no safe single-type mapping for "may be absent."
Whole-item safety throughout (not per-field, unlike relationships): a
constructor's field list is the struct's *entire* shape, so any
unsupported parameter rejects the whole class. Field names are
**snake_cased** (`codegen::to_snake_case`) at emission, unlike Phase 2a's
form-request fields — deliberately, since a Job/Event field name has no
external wire-key contract the way a `#[derive(FormRequest)]` field name
does (it's read back as a literal HTTP form key); `#[derive(Serialize,
Deserialize)]` round-trips against whatever name this converter itself
picks. `events::convert` emits `#[derive(Clone)] pub struct Name { pub
field: Type, ... }` (`larust_events::Event` is a pure blanket impl over
`Clone + Send + Sync + 'static` — no derive macro, no required methods,
so a field-only struct is already enough). `jobs::convert` emits
`#[derive(Serialize, Deserialize)] pub struct Name { ... } impl Job for
Name { const JOB_TYPE: &'static str = "..."; async fn handle(&self) ->
Result<(), AppError> { todo!() } }`, with `handle()`'s original body
preserved as a comment via the same `php::find_method`/`php::
body_as_comment` primitives as controllers/policies. `JOB_TYPE` is
**always** mechanically derived as `to_snake_case(struct_name)` (e.g.
`notify_post_created_job`) — never a hand-picked shorter slug; the real
shipped `demo/app/Jobs/notify_post_created_job.rs` uses the shorter
`"notify_post_created"`, but that's hand-authored demo content predating
this phase, not a target to reproduce — mechanical consistency beats
guessing at a "nicer" name.

**`generate_controller_stubs`, `convert_models`, `convert_policies`,
`convert_events`, `convert_jobs`** in `crates/larust-cli/src/convert.rs`
follow the same flat-`read_dir`-plus-`codegen::generate_file` shape as
every prior `convert_*` step, relying on PSR-4 (a Laravel class's
filename always matches its class name) to derive `class_name` from each
file's stem rather than parsing it out separately. `convert_models` must
run after `convert_migrations` — it reads that step's own `.sql` output
from `out_root`, not `laravel_root`.

### `CONVERSION_REPORT.md`

Written to the converted project's root — expands `rust-laravel.md`'s own
two-bucket sketch ("Converted automatically" / "Requires manual review")
with a third bucket for the two-tier package design above. Per-item
file-path detail for every "requires manual review"/flagged entry, not
just a count — a bare "8 dynamic Eloquent scopes" would be useless for a
design whose whole point is "never silently drop, always name it."

### Testing

Per-converter unit tests (`composer.rs`/`routes.rs`/`migrations.rs`/
`config.rs`/`requests.rs`/`blade/expr.rs`/`blade/scan.rs`/`models::{schema,
fields, relations, mod}`/`controllers.rs`/`policies.rs`/
`constructor_props.rs`/`events.rs`/`jobs.rs`) feed small literal PHP/JSON
strings through `php.rs` and assert exact generated output — the dominant
test style everywhere else in this codebase, including the negative cases
(`requests.rs`: `unique` dropped without affecting sibling fields/rules, a
dotted key skipped without being emitted under a guessed name, an invalid
class name rejecting the whole file; `blade/expr.rs`: every excluded
construct — `and`/`or`, string concatenation, `??`, `isset`, array
indexing, bare `null`, interpolated strings — correctly rejected, plus a
dedicated test asserting every *accepted* translation round-trips through
`syn::parse_str` cleanly; `blade/scan.rs`: an unsupported directive
rejecting the whole file, an out-of-subset expression inside an
otherwise-fine `@if` also rejecting the whole file, an email address
correctly *not* mistaken for a directive; `models::mod`: a model rejected
when no migration creates its table or a column type is unrecognized, an
unsupported relationship flagged without rejecting the model;
`relations.rs`: `belongs_to_infers_foreign_key_from_the_method_name_not_
the_related_class` — the test directly encoding the Laravel-source-
verified convention described above; `constructor_props.rs`: a class
type hint rejected in both promoted and classic constructor styles).

One integration test (`larust-cli/src/convert.rs`'s own `#[cfg(test)]`
module — `larust-cli` has no library target, so a `tests/*.rs` file can't
reach `convert::run` at all) runs the full pipeline against a hand-written
fixture Laravel app (`larust-convert/tests/fixtures/sample-laravel-app/`,
grown every phase: an `app/Http/Requests/StorePostRequest.php` in Phase
2a, a small `resources/views/` tree in Phase 2b, and in Phase 3 an
`app/Models/{User,Post,Tag}.php` mixing explicit and omitted-argument
`belongsTo`/`hasMany`/`belongsToMany` calls plus one deliberately
unsupported `hasManyThrough`, an `app/Http/Controllers/PostController.php`
matching the routes fixture, an `app/Policies/PostPolicy.php`, an
`app/Events/PostCreated.php`, and an `app/Jobs/NotifyPostCreatedJob.php`
covering the classic constructor style), asserting both the generated
report's exact contents and that the output actually **compiles** — the
same "scratch-scaffold verification" technique used elsewhere in this
codebase for a fresh `xr new` scaffold: a temporary `[workspace]` table
isolates the generated crate from the outer workspace (it isn't matched
by `crates/*`), `cargo build` runs against it standalone, then the whole
output directory is discarded. This `cargo build` check is what actually
caught Phase 3's three real integration bugs before they shipped (missing
relationship-type imports, the `User`/`Authenticatable` trait bound, and
the unsound class-typed-constructor-field mapping) — none of the
per-converter unit tests above could have, since each one only checks a
single generated file's text in isolation, never a full multi-file crate
compiling together.

## The generated app's file layout

`xr new` scaffolds Laravel's directory tree (`app/Http/Controllers`,
`app/Models`, `resources/views`, `database/migrations`, etc.) under a
project root with **two** Rust crate roots — `src/lib.rs` and
`src/main.rs` — not just one. Since Rust's module system is based on
`mod` declarations, not directory conventions, `lib.rs` pulls each `app/`
subdirectory in explicitly:

```rust
#[path = "../app/Http/Controllers/mod.rs"]
pub mod controllers;
#[path = "../app/Http/Middleware/mod.rs"]
pub mod middleware;
#[path = "../app/Mail/mod.rs"]
pub mod mail;
#[path = "../app/Jobs/mod.rs"]
pub mod jobs;
#[path = "../app/Events/mod.rs"]
pub mod events;
#[path = "../app/Models/mod.rs"]
pub mod models;
#[path = "../app/Policies/mod.rs"]
pub mod policies;
#[path = "../app/Http/Requests/mod.rs"]
pub mod requests;
```

`main.rs` reaches these through the library crate by name (`use
{crate_ident}::controllers::PostController;`) rather than declaring its
own `mod` blocks — this split exists specifically so `tests/*.rs` (compiled
by Cargo as its own separate crate, with nothing to `use crate::...` from)
can reach `{crate_ident}::controllers`/`{crate_ident}::models`/etc. too;
before this, no generated app had any library target at all, and writing a
real integration test for one was impossible. `crate_ident` is the app's
package name with hyphens replaced by underscores (Cargo's own rule for
deriving a `use`-able identifier — `xr new my-app` produces a package
named `my-app` but a crate reachable as `my_app`).

Every one of these `mod.rs` files is created by `xr new` itself — even
`app/Http/Middleware/mod.rs`, which starts **empty** (no middleware is
scaffolded by default) but still needs to exist and be declared from the
start. Without it, `xr make:middleware` would write a `.rs` file into a
directory nothing `mod`-includes, and it would sit on disk uncompiled and
completely unverified by `cargo build`. This was a real bug caught during
M6's review — see GOTCHAS.md. `app/Policies/mod.rs` follows the identical
pattern, for the identical reason, since M26.

`xr make:controller`/`make:model`/`make:request`/`make:middleware` all
follow the same pattern (`crates/larust-cli/src/generate.rs`): write the new
`.rs` file, then append `pub mod {name}; pub use {name}::{Export};` to the
directory's `mod.rs`. If the second step fails, the first step's file is
deleted — there's no "generated file exists but isn't wired up" state left
behind on any successful *or* failed run. `xr make:policy` follows the
same `generate_file` machinery but with no `pub use` line at all (a policy
file exports nothing nameable — it's just a trait `impl` block).

## The `xr` CLI's two command shapes

Commands split into two categories:

- **Pure file generation** (`new`, `make:*`) — `xr` does the work itself,
  no app process involved. Safe to run without a working `cargo build` in
  the target directory.
- **Commands needing the app's own runtime state** (`route:list`,
  `migrate`, `queue:work`) — `xr` shells out to `cargo run --quiet --
  <subcommand>` *inside the app's own directory*, because routes, database
  connections, and job types are only known inside the compiled app binary
  itself. This mirrors Laravel's `artisan`, which is the app (bootstraps
  the full framework), not an external tool — `xr` can't introspect a
  compiled Rust binary's routes (or its app-defined `Job` types) from the
  outside, so it asks the binary to report/run them instead. See
  `run_app_subcommand` in `crates/larust-cli/src/main.rs`, and the `if
  command.as_deref() == Some("route:list") { ... }` / `Some("migrate")` /
  `Some("queue:work")` branches in every generated `main.rs`.
