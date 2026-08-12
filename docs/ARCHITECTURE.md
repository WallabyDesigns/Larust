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
| `larust-mail` | `Mailable` trait, `mail().to(...).send(...)`, `log`/`smtp` drivers (`lettre`) | `larust-core` (`AppError`, `Config`) |
| `larust-cache` | `cache::{put, get, forget, remember}` — single SQLite-backed driver, self-bootstrapping `cache_items` table | `larust-core` (`AppError`), `larust-orm` (`pool()`) |
| `larust-events` | `Event`, `event::{listeners, dispatch}` — in-process, synchronous pub/sub, no persistence | — |
| `larust-queue` | `Job`, `queue::{dispatch, work, JobRegistry}` — durable, SQLite-backed job queue, `failed_jobs` on error | `larust-core` (`AppError`), `larust-orm` (`pool()`) |
| `larust-storage` | `Disk`, `storage::{local, public}` — two fixed disks, path-traversal-safe file I/O | `larust-core` (`AppError`) |
| `larust-live` | `LiveComponent`, `LiveRegistry`, `mount`/`update`/`runtime_js` — server-state-backed reactive components (`@live(...)`), session-keyed, plus the vendored client runtime | `larust-core` (`AppError`), `larust-http` (`Session`, `random_hex`), `larust-view` (`View`, `escape`) |
| `larust-support` | The facade — re-exports everything above under one path | all of the above |
| `larust-cli` | The `xr` binary: `new`, `make:*`, `migrate`, `route:list`, `queue:work`, `dev`, `audit`, `update` | (none — templates are plain strings, no codegen dependency) |

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

Also out of scope for v1: `.queue()` (deferred sending via a background
worker) — that's Jobs/Queues, a separate roadmap item; `send()` is always
synchronous/immediate.

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

`assertSentCount`/`assertNothingSent`/`assertQueued` are out of scope for
v1 — `assert_sent`/`assert_not_sent` cover Laravel's own most common real
usage; the rest are a documented future extension, the same shape as
Mail's own deferred `.queue()` and Queue's deferred retry/backoff.

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
well after its own initial queue design) — a documented future extension,
same shape as Mail's deferred `.queue()`/`Mail::fake()`.

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

**The `@live(...)` directive.** `@live('name')` or `@live('name', { prop:
expr, ... })` — an `@word(...)` directive like every other (`@extends`,
`@global`, ...), not a custom HTML-tag syntax (which would need a genuinely
new parser marker-kind/attribute grammar). `larust-view`'s `Node::Live {
name, props }` stores `props` as raw `(String, String)` pairs, same
convention as `Node::Globals` — no `syn` dependency in that crate. Needs
**no `resolve.rs` changes at all**: unlike `@push`/`@globals` (whole-chain
compile-time collection passes, explicitly rejected inside `@foreach`),
mounting a component is an ordinary runtime statement, so it composes
naturally with a real Rust `for` loop and needs no special-casing — `@live`
is deliberately allowed inside `@foreach`/`@if`.

`larust-macros/src/view.rs`'s `Node::Live` codegen arm is the first arm
that needs `.await`/`?` and an in-scope `session: &Session` binding — an
implicit contract on the `view!` call site, exactly like `@csrf`'s existing
`csrf_token` contract, just one binding richer. `expand()` checks for this
eagerly (`contains_live` + a context-name scan) so a template misusing
`@live` fails at the macro call site with a clear message, not as a
confusing "cannot find value `session`" pointing at generated code.

**Component definition.** A `LiveComponent` trait (`mount`/`render`/`call`,
all `async`, spelled `-> impl Future<..> + Send` rather than `async fn` so
the `Send` bound is explicit — no `#[trait_variant]`/`async-trait`
dependency needed) — async because a real component routinely needs real
async work (`demo`'s post listing queries the database in `render`; `demo`'s
post-creation form writes to it from `call`). `mount` and `call` both
receive `session: &Session` (the same session the page's own `@live(...)`
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

**Session storage.** One session key, `__live_components`, holding an
insertion-ordered `Vec<(String, StoredComponent)>` — not one key per
component. `tower-sessions-sqlx-store` already round-trips the *entire*
session blob (MessagePack, single BLOB column) on every write regardless of
how many top-level keys are touched, so per-component keys would buy
nothing on that axis while making capping/sweeping stale entries harder.
Every full-page GET through an `@live(...)` mount point creates a
**brand-new** component instance (fresh id, freshly `mount()`-ed state) —
no cross-navigation persistence, matching Livewire's own per-page-load
semantics — so stale/orphaned entries are expected on every page view, not
a bug. Capped at `MAX_COMPONENTS_PER_SESSION = 50` (hardcoded, evicted
oldest-first), matching this codebase's "no toggle until real pressure
justifies one" stance elsewhere.

A process-wide, per-session-id `tokio::sync::Mutex`
(`larust_live::lock::with_session_lock`) guards the full read-modify-write
cycle of any `__live_components` mutation, since the session store itself
has no per-key locking or optimistic-concurrency check — without it, two
concurrent live-component writes under one session (two components on a
page, an overlapping double-click) could silently clobber each other.
Documented, deliberate gap: this only covers `__live_components` writes —
it does *not* protect against racing an unrelated session write elsewhere
(a CSRF-token regen, a login), which stays last-writer-wins at the
whole-blob level, same as every other session write today. `Auth::
logout()`'s `session.flush()` wiping `__live_components` along with
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

**Wire protocol.** One route, `POST /__larust_live/{component_id}`, handles
a `wire:model`-style prop sync and a `wire:click`/`wire:submit`-style
action call together — every sync carries the component's *entire* current
`wire:model` field set (not a delta), which is what correctly threads a
deferred field's just-typed value through when a different element's
click/submit/live-sync is what actually triggers the request. Response is
`200 text/html`, the same `<div data-live-id="...">` wrapper shape the
initial mount produces (one uniform patch target for both first paint and
every later fragment) — even when the action also requested a redirect
(below), so the component's own state is still saved and reflected
correctly if the redirect target ever reads it back. The whole sync is
atomic — if the prop merge or the action call fails, nothing is written
back. An action's `Ok(Some(path))` return is carried out-of-band as an
`X-Live-Redirect: {path}` response header (checked by the client before it
ever reads the body) rather than folded into the body — that keeps the
response's shape (a `text/html` fragment) identical in both the redirect
and non-redirect case, instead of inventing a second, JSON-shaped response
convention just for this one case. `GET /__larust_live/runtime.js` serves
the vendored client script (`include_str!`'d from
`crates/larust-live/assets/live-runtime.js`, not copied into `public/js/`,
so it stays version-locked to the installed `larust-live` crate with no
upgrade-drift risk). Both routes are registered **explicitly** by the app
(or pre-populated by the `xr new` scaffold) — nothing here is auto-mounted
by `Application::serve()` the way `/__larust_dev` is.

**`@larustscripts`** — Livewire's `@livewireScripts` equivalent, written
once in a shared layout (conventionally right before `</body>`, same
placement `demo`'s and the `xr new` scaffold's own `layouts/app.blade.xr`
use) rather than requiring every individual page that mounts a `@live(...)`
component to remember its own `<script src="/__larust_live/runtime.js">`
tag. Unlike the route registration above, this genuinely is automatic —
but it's still a compile-time decision, not a runtime branch: `larust-view`'s
`Node::LarustScripts` codegen arm in `larust-macros/src/view.rs` expands to
the script tag only when that exact template's resolved tree (itself, or —
via `@extends` — whatever page is rendering through it) contains a
`Node::Live` anywhere, reusing the very same `contains_live` scan that
decides whether `session` needs to be in the `view!` context at all. A page
with no `@live(...)` gets nothing from `@larustscripts`, even though it
shares the exact same layout as a page that does — proven directly in
`crates/larust-macros/tests/view_larustscripts.rs` (two sibling pages
extending one layout, asserting the script tag appears on one and not the
other) and again against the real app in
`demo/tests/live_post_list_test.rs`.

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

`demo` has two real, working examples. `/posts` (`demo/app/Live/
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
`demo/tests/live_post_list_test.rs`.

`/posts/create` **and** `/posts/{id}/edit` (`demo/app/Live/post_form.rs`,
`demo/resources/views/components/post-form.blade.xr`) share a single
`PostForm` component rather than two near-duplicate templates — Livewire's
own usual pattern for a create/edit pair. `create.blade.xr` mounts
`@live('post-form')` with no props; `edit.blade.xr` mounts
`@live('post-form', { post_id: post.id })`, and `mount` populates `title`/
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
`demo/tests/live_post_form_test.rs`.

`/profile` (`demo/app/Http/Controllers/profile_controller.rs`,
`demo/resources/views/profile/show.blade.xr`) is the one addition in this
area that's deliberately *not* a `@live(...)` component — a plain
server-rendered form pair (update name/email, change password), matching
`/login`/`/register`'s own plain-form convention rather than `PostForm`'s
reactive one. `ProfileController::update_password` re-verifies the
submitted `current_password` against the session user's real hash before
allowing a change, the same "always the real authorization boundary, not
just a page-level gate" posture `PostForm::update_existing` above takes.
Exercised end-to-end in `demo/tests/profile_test.rs`.

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
