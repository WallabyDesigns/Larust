---
title: Error Handling
parent: The Basics
nav_order: 5
---

# Error Handling
{: .no_toc }

1. TOC
{:toc}

## `AppError`

Every handler in this framework returns `Result<impl IntoResponse,
AppError>` (see [Controllers & Requests](../../the-basics/controllers-and-requests#apperror-what-a-handler-can-fail-with)
for the full enum). `?` propagates naturally from any `sqlx`/`AppError`-
returning call, the same as any other Rust `Result`-returning function:

```rust
pub async fn show(post: Post) -> Result<impl IntoResponse, AppError> {
    let author = post.user().await?;   // AppError::Internal on a DB failure
    larust_support::auth::authorize(post.can_manage(&user).await?)?; // 403 if false
    Ok(view!("posts.show", { post }))
}
```

## Default error pages

A generated app registers its error pages once, at boot:

```rust
app.with_error_pages(larust_core::ErrorPages {
    not_found: larust_support::error_view!("404"),
    internal: larust_support::error_view!("500"),
})
```

`error_view!("404")` looks for `resources/views/errors/404.blade.xr` in
your app and compiles it the same way `view!` compiles any other
template; drop a file there to override either page. With no file
present, it compiles to Larust's own built-in default page instead - a
plain, real 404/500 you never have to build from scratch just to have
*something* reasonable in production.

## Panics don't take the server down

Every request runs behind `tower_http`'s `CatchPanicLayer` - if a handler
panics (an unwrapped `None`, an index out of bounds, whatever), that one
request gets a 500 response and the rest of the server keeps running
completely unaffected. This is standard `tower-http` behavior, not
something Larust built itself, but it's on unconditionally in every
generated app.

## `APP_DEBUG`

```
# .env
APP_DEBUG=true
```

With `APP_DEBUG=true`, an `AppError::Internal`/`Config` (or a caught
panic) renders a real, descriptive HTML page instead of a generic
message: the error's own message, its full `source()` chain, and (for a
panic) the panic message itself. This is the single most useful thing to
have on while actually building a feature - a broken query tells you
*why* it broke, right there in the browser, instead of a bare "internal
server error."

{: .warning }
**Never enable `APP_DEBUG` in production.** It's exactly as dangerous as
Laravel's own `APP_DEBUG=true` in production: full error detail -
potentially including query text, file paths, and internal state - goes
straight to whoever's request triggered it. Every `.env.example` this
framework generates ships it `true` for local dev with this warning
attached; double-check it's `false` (the default when the var is unset)
before a real deploy.

With `APP_DEBUG=false` (or unset), the same failures render the plain
default (or your own custom) 404/500 page instead - no detail leaked,
just logged server-side via `tracing::error!`.

## `xr dev`'s build-status banner

Worth knowing about separately from request-time errors: while running
`xr dev`, a small fixed banner appears in any open browser tab the moment
a rebuild starts, and again if that build fails - because otherwise a
request landing on the *old*, still-running process mid-rebuild gets a
confident, correct-looking response for code that's already been changed
underneath it, which is a confusing thing to debug if you don't know it's
happening. A failed build never takes the site down; the last known-good
version keeps serving until a new build succeeds. See [Your First
App](../../getting-started/your-first-app#iterate-with-xr-dev).

## Next

You've now seen the whole request lifecycle - routing, controllers,
validation, templates, sessions, and errors. [Database](../../database/)
covers models, migrations, and relationships next.
