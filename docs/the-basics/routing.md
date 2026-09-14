---
title: Routing
parent: The Basics
nav_order: 1
---

# Routing
{: .no_toc }

1. TOC
{:toc}

## The basics

Routes live in `routes/web.rs` (browser-facing) and `routes/api.rs`
(stateless API), each returning a `larust_http::Router` built with the
`Route`/`Router` DSL:

```rust
use larust_http::Route;

pub fn routes() -> Router {
    Route::get("/", home)
        .get("/posts", PostController::index).name("posts.index")
        .post("/posts", PostController::store).name("posts.store")
        .put("/posts/{post}", PostController::update).name("posts.update")
        .patch("/posts/{post}", PostController::patch).name("posts.patch")
        .delete("/posts/{post}", PostController::destroy).name("posts.destroy")
}
```

`Route::get(path, handler)` starts a chain and returns a `Router`; every
verb method after the first (`.get`/`.post`/`.put`/`.patch`/`.delete`)
appends to it. Every method consumes `self` and returns a new `Router` -
the type is `#[must_use]` specifically so a call left unchained (`route
.get(...);` with the result discarded) is a compiler warning, not a
silently-dropped route.

A `handler` is any ordinary Axum handler function - `async fn(...) ->
impl IntoResponse` (or a `Result` of one), taking any combination of Axum
extractors as arguments. There's no special "Larust handler" trait to
implement.

## Named routes

`.name("posts.show")` attaches a name you can resolve later with
[`route()`/`route_with()`](#helpers) instead of hardcoding the path
string a second time. Route names follow Laravel's own dotted convention
(`posts.index`, `posts.show`) but that's just a naming habit - any string
works.

## Route parameters and model binding

A `{param}` segment (`/posts/{post}`) is available to any handler that
declares a matching-typed parameter:

```rust
// Raw string param - works for any handler.
async fn show(Path(post_id): Path<String>) -> ... { ... }
```

```rust
// Route model binding - Laravel's implicit model binding. Takes the
// `Post` argument directly; Larust looks the row up for you by primary
// key and returns a 404 automatically if it isn't found.
async fn show(post: Post) -> ... { ... }
```

The second form works because `#[derive(Model)]` generates a real
`FromRequestParts` impl for you: it reads the path parameter named after
the model's own snake_case name (`post` for `struct Post`), looks the row
up by primary key, and rejects with `AppError::NotFound` (a real 404) if
nothing matches - all before your handler body ever runs. See [Models &
Relationships](../../database/models-and-relationships) for `#[derive(Model)]`
itself.

## Groups and group-scoped middleware

```rust
route.group("", |r: Router| {
    r.middleware(axum::middleware::from_fn(require_auth))
        .get("/posts/create", PostController::create).name("posts.create")
        .post("/posts", PostController::store).name("posts.store")
})
```

`.group(prefix, |r| ...)` opens a fresh `Router`, hands it to your
closure, and merges whatever comes back in under `prefix` (an empty
string is a group with no path prefix at all - used purely to scope
middleware to a subset of routes, as above). Middleware attached *inside*
the closure only applies to routes registered inside it - a sibling
`.group(...)` or a top-level route elsewhere is unaffected. This is
Laravel's `Route::middleware('auth')->group(...)` in different clothes:
the middleware itself is a real function reference
(`axum::middleware::from_fn(require_auth)`), not a string alias looked up
in a registry, so there's nothing to misspell and nothing that can
silently fail to resolve.

{: .warning }
`Router::group` applies **the group's own middleware to everything merged
into it**, including a nested `.plugin(...)` call's already-absolute
paths - a documented landmine if you nest a plugin inside a group rather
than registering it at the top level of a chain. See
[Plugins](../../digging-deeper/social-login-and-plugins#plugins) for the
regression test that pins this down.

## `Route::resource` - all 7 RESTful routes at once

```rust
Route::resource("posts", "post", // param name used for both {post} and route-model-binding
    PostController::index,
    PostController::create,
    PostController::store,
    PostController::show,
    PostController::edit,
    PostController::update,
    PostController::destroy,
)
```

Registers exactly what Laravel's `Route::resource('posts',
PostController::class)` does, with the same naming convention:

| Method | Path | Name |
|---|---|---|
| GET | `/posts` | `posts.index` |
| GET | `/posts/create` | `posts.create` |
| POST | `/posts` | `posts.store` |
| GET | `/posts/{post}` | `posts.show` |
| GET | `/posts/{post}/edit` | `posts.edit` |
| PUT | `/posts/{post}` | `posts.update` |
| DELETE | `/posts/{post}` | `posts.destroy` |

Every handler is a real, separately-typed function - there's no
conventionally-named-method-on-a-controller-class magic the way Laravel's
version resolves `PostController@index` by string. You pass each function
explicitly, so a missing or misnamed method is a compile error, not a
runtime 404 that only shows up when someone clicks the link.

## Combining routers: `.merge()` vs `.group()`

Both take a prefix and another `Router`, but they mean different things:

- **`.group(prefix, \|r\| ...)`** - "these routes are part of the same
  route tree, and inherit whatever middleware is already on this
  router." Use it to scope middleware to a subset of routes you're
  defining right here.
- **`.merge(prefix, other)`** - "combine two *independent* route trees;
  `other`'s routes stay immune to whatever middleware this router already
  has attached." This is exactly why `routes/web.rs` (CSRF, sessions,
  cookie auth) and `routes/api.rs` (stateless, no CSRF) are combined via
  `.merge(app.config().api_prefix, routes::api::routes())` in `lib.rs`,
  not `.group(...)` - CSRF verification must never reach `/api/*`.

Getting this backwards is a real, previously-shipped bug in this
framework's own history: `Router::plugin` was originally implemented as
sugar over `.merge()`, which silently exempted every plugin's routes
(including CSRF-protected ones) from an app's top-level middleware. See
[Plugins](../../digging-deeper/social-login-and-plugins#plugins) for what
changed.

## Listing every route

```bash
xr route:list
```

Prints every registered route (method, path, name) across the merged
`web` + `api` router, including framework-internal ones registered by
`.plugin(...)` calls (the `@wire`/`@live`/`@spa` runtimes, if your app
uses them).

## Helpers

`route(name)` and `route_with(name, &[("param", "value")])` resolve a
named route back to its path, from a controller or directly inside a
{% raw %}`{{ }}`{% endraw %} template interpolation:

```rust
route("posts.index")?           // "/posts"
route_with("posts.show", &[("post", &post.id.to_string())])?  // "/posts/42"
```

`route()` fails (rather than returning a broken literal path) if the
route needs a parameter you didn't supply - use `route_with` for those.
See [docs/ARCHITECTURE.md](https://github.com/wallabydesigns/Larust/blob/main/docs/ARCHITECTURE.md#helpers-route-route_with-url-asset-config)
for `url()`/`asset()`/`config()` alongside these.

## Next

[Controllers & Requests](../../the-basics/controllers-and-requests) covers what a handler
looks like and how validated input gets there.
