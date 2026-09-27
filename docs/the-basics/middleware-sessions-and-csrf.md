---
title: Middleware, Sessions & CSRF
parent: The Basics
nav_order: 4
---

# Middleware, Sessions & CSRF
{: .no_toc }

1. TOC
{:toc}

## Middleware

Middleware is a plain Axum middleware function - `.middleware(...)` on a
`Router` attaches it, and it applies to whatever it's attached to
(`.group(...)` scopes it to a subset, the whole chain applies it to
everything). There's no named-middleware registry to register into and
nothing to look up by string:

```rust
route.group("", |r: Router| {
    r.middleware(axum::middleware::from_fn(require_auth))
        .get("/posts/create", PostController::create)
})
```

Write your own with `xr make:middleware EnsureSubscribed`, which
generates the standard Axum middleware-function shape
(`async fn(request, next) -> Response`) for you to fill in.

## Sessions

`.with_sessions(pool, secure_cookie)` attaches session support to a
router - called once, on the final, fully-merged router (not inside a
`.group(...)` closure, since it's `async` and every route-building
closure is not). A generated app's `serve()` does this for you already:

```rust
route.with_sessions(larust_support::orm::pool()?, app.config().session_secure_cookie).await?
```

Sessions are backed by [tower-sessions](https://github.com/maxcountryman/tower-sessions),
via one of two real `SESSION_DRIVER` values:

```
# .env
SESSION_DRIVER=database   # default
# SESSION_DRIVER=file
```

- **`database`** (default) - stored in the same database `DB_CONNECTION`
  points at (a hand-written `SessionStore` over `sqlx::AnyPool`, since
  third-party per-backend store crates need a concretely-typed pool
  Larust's runtime-generic pool can't give them).
- **`file`** - one file per session under `storage/sessions/`, Laravel's
  own `SESSION_DRIVER=file` equivalent. No database round trip for session
  reads/writes; a real choice worth reaching for if session churn is
  putting real load on your database (see `docs/GOTCHAS.md` for a real
  incident that caused). Only works for a single server: every process
  reading/writing sessions needs to see the same directory, which a real
  multi-server deployment behind a load balancer can't guarantee the way a
  shared database can - stick with `database` if that's where you're
  headed.

Neither is an in-memory store: session data needs to survive a process
restart - a deploy, a crash, `xr dev`'s own rebuild-and-restart cycle -
which files on disk do exactly as well as database rows do. There's
deliberately no in-memory option in the public API at all, and never will
be: an in-memory session store that quietly logs every user out on every
deploy is a well-known Laravel footgun (`SESSION_DRIVER=array` reaching
production) that persisting to *something* - a database or a file, either
one - avoids entirely.

{: .warning }
Any other `SESSION_DRIVER` value (a typo, or a real Laravel value this
framework doesn't implement - `array`, `redis`, `cookie`, ...) falls back
to `database`, with a startup warning explaining why - see
`Config::session_driver`'s own doc comment.

A handler reads/writes the session through the `Session` extractor -
`tower_sessions::Session`, re-exported directly:

```rust
pub async fn store(session: Session, ...) -> Result<impl IntoResponse, AppError> {
    session.insert("user_id", user.id).await?;
    let flash: Option<String> = larust_http::session::take(&session, "success").await?;
}
```

{: .warning }
**Prefer `larust_http::session::take` over a bare `session.remove(key)`
for a flash-message read that runs on every page view** (checking "is
there a success/error message to show" on every response, not just the
ones that redirected in with one) - `tower_sessions::Session::remove`
marks the session modified even when the key was never present, so a bare
`.remove()` writes to the session store on *every* request, not just the
ones with something to flash. `take` checks presence first and only calls
`remove` when there's actually something to remove. See `docs/GOTCHAS.md`
for the real production incident (cascading SQLite lock contention) this
caused before `take` existed.

{: .warning }
**The session cookie's `Secure` attribute is silently dropped on any
hostname a browser doesn't recognize as a secure context** - only
`127.0.0.1`, `::1`, and the literal `localhost` qualify over plain HTTP. A
custom local-dev hostname (a `.test` domain in `/etc/hosts`, even one that
resolves to loopback) will look fine in the browser but silently receive
no session cookie at all - every state-changing request then fails CSRF
verification with no error pointing at the real cause. Set
`SESSION_SECURE_COOKIE=false` in `.env` if you develop against a custom
hostname rather than `localhost`.

## CSRF

`larust_http::csrf::verify` is the middleware; a generated app's
`routes/web.rs` applies it once, to the whole router, at the very end of
the chain - and deliberately **not** to `routes/api.rs`, since CSRF
protects cookie-authenticated browser form submissions, and an API route
merged in via `.merge()` is immune to whatever middleware the router it's
merged into carries (see [Routing](../../the-basics/routing#merge-vs-group)).

In a template, `@csrf` expands to a hidden input carrying the current
session's token:

```
<form method="post" action="/posts">
    @csrf
    <input name="title">
</form>
```

Equivalently, for a JS-driven request (a `fetch()` call, a file upload)
where a hidden form field isn't natural, send the token as a header
instead - checked before the body, matching Laravel's own convention:

```js
fetch("/posts", { method: "POST", headers: { "X-CSRF-TOKEN": token } })
```

Get the current token explicitly (e.g. to embed in a `<meta>` tag for
JS to read, as the reference layout does) with:

```rust
let csrf_token = larust_http::csrf::token(&session).await;
```

A request that fails verification gets a real `419` page with a link
home, not a bare "CSRF token mismatch" string - `xr dev`'s own auto-reload
used to be able to trigger this spuriously by resubmitting a stale POST
on reconnect; that's fixed, but the page itself stayed friendlier since a
genuine mismatch (an expired tab, a forged request) can still reach it.

## Next

[Error Handling](../../the-basics/error-handling) covers what happens when any of this
fails - `AppError`, `APP_DEBUG`, and the default error pages.
