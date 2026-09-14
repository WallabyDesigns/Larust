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
stored in the same database `DB_CONNECTION` points at (a hand-written
`SessionStore` over `sqlx::AnyPool`, since third-party per-backend store
crates need a concretely-typed pool Larust's runtime-generic pool can't
give them). This is a deliberate choice, not a limitation waiting to be
lifted: session data needs to survive a process restart - a deploy, a
crash, `xr dev`'s own rebuild-and-restart cycle - so there's no in-memory
store in the public API at all. An in-memory session store that quietly
logs every user out on every deploy is a well-known Laravel footgun
(`SESSION_DRIVER=array` reaching production); Larust just doesn't offer
the option.

A handler reads/writes the session through the `Session` extractor -
`tower_sessions::Session`, re-exported directly:

```rust
pub async fn store(session: Session, ...) -> Result<impl IntoResponse, AppError> {
    session.insert("user_id", user.id).await?;
    let flash: Option<String> = session.remove("success").await?;
}
```

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
