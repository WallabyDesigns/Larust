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

## Logging

Every generated app logs through [`tracing`](https://docs.rs/tracing) -
`larust_support::tracing::info!`/`warn!`/`error!` are the same macros
`AppError`'s own 500/panic handling already calls internally. By default
that output goes straight to the terminal (`LOG_CHANNEL=stdout`, the
implicit default with no `.env` entry at all) - fine for `xr dev` and
anything running under a process manager or container runtime that
already captures stdout for you.

```
# .env
LOG_CHANNEL=stdout   # stdout (default) | file | stack
LOG_LEVEL=debug      # trace | debug | info | warn | error - leave unset for this framework's own default
LOG_MAX_SIZE=10485760
LOG_KEEP_FILES=5
```

`LOG_CHANNEL=file` writes to `storage/logs/larust.log` instead of the
terminal; `LOG_CHANNEL=stack` writes to both at once (Laravel's own
`LOG_CHANNEL=stack` meaning). Either way, the log file rotates once it
passes `LOG_MAX_SIZE` bytes (10 MiB by default): the current file becomes
`larust.log.1` (shifting any existing `.1`/`.2`/... down one generation
first) and a fresh, empty file is started. `LOG_KEEP_FILES` caps how many
rotated backups survive that shift - anything older is deleted outright;
`LOG_KEEP_FILES=0` keeps no backups at all, just the current file.

`LOG_LEVEL` picks a single level for every crate at once
(`trace`/`debug`/`info`/`warn`/`error`) - left unset, verbosity falls back
to this framework's own existing default (`debug` when `APP_ENV=local`,
`info` otherwise). `sqlx`/`tower_sessions`'s own per-query/per-request
debug spans stay capped at `warn` regardless of `LOG_LEVEL` - both are
extremely noisy at `debug`/`trace`, logging full SQL statements per call.
For anything more targeted than a single level - "everything at `info`,
except `sqlx` at `debug`" - set the standard [`RUST_LOG`](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html)
environment variable instead; it's read before `LOG_LEVEL` and wins over
it (and over the built-in default) whenever it's set at all.

{: .warning }
Not a full replacement for Laravel's own `single`/`daily`/`stack`/`slack`
channel menu - `stack` here only ever means "stdout and the rotating file
together," and rotation is size-based only (no `daily` channel). Sending
an alert (email/Slack) when a rotation actually happens isn't built in
either.

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
