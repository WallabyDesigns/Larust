---
title: Testing
nav_order: 9
---

# Testing
{: .no_toc }

1. TOC
{:toc}

## Why a generated app has a library target

Every `xr new` app gets a `src/lib.rs` alongside `src/main.rs`
specifically so `tests/*.rs` can depend on `app::controllers`,
`app::models`, and so on as a real, ordinary crate dependency - the same
relationship any integration test has with a published library, not a
special testing mode.

## `TestClient` - real, in-process HTTP requests

```rust
use larust_testing::TestClient;

#[tokio::test]
async fn only_the_owner_may_edit_a_post() {
    let pool = larust_testing::test_db(Path::new("database/migrations")).await.unwrap();
    let router = build_router(&pool).await;   // your own routes() + .with_sessions(...)

    let mut client = TestClient::new(router, &pool);
    let csrf = client.get("/posts/create").await.csrf_token().unwrap();

    let response = client.post_form("/posts", &[
        ("_csrf_token", &csrf),
        ("title", "Hello"),
        ("content", "World"),
    ]).await;

    response.assert_status(StatusCode::SEE_OTHER);
    response.assert_redirect_to("/posts");
}
```

| Method | Sends |
|---|---|
| `.get(path)` | A GET request |
| `.post_form(path, &[(k, v), ...])` | `application/x-www-form-urlencoded` - what a real `<form>` submits, and what a `#[derive(FormRequest)]` extractor reads |
| `.post_multipart(path, ...)` | A file upload |
| `.post_json(path, &body)` | A JSON body, for API routes |
| `.acting_as(&user)` | Simulates an already-logged-in session, without going through a real login POST |

`TestResponse` gives you `.status()`, `.body()`, `.header(name)`,
`.csrf_token()`/`.meta_csrf_token()` (pulls a real token straight out of
the rendered page, so a test never hardcodes one), and chainable
assertions: `.assert_status(...)`, `.assert_redirect_to(...)`,
`.assert_body_contains(...)`.

## A fresh, fully migrated database per test

```rust
let pool = larust_testing::test_db(Path::new("database/migrations")).await?;
```

Laravel's `RefreshDatabase`, not `DatabaseTransactions` - a real
`BEGIN`/`ROLLBACK`-per-test design was tried and abandoned when it broke
session-backed routes (a session write needs its own committed
transaction to be visible to the *next* request in the same test, which
a wrapping rollback transaction prevents). `test_db` gives you a
brand-new, freshly migrated database every call instead - no "one test
per file" constraint, unlike some of this framework's other
process-wide mechanisms (see the callout below).

For a test that just needs a scoped, disposable pool without wiring a
whole router by hand:

```rust
larust_testing::test_transaction(Path::new("database/migrations"), |pool| async move {
    // Inside this scope, `larust_orm::pool()` transparently resolves to
    // `pool` above via a task-local override - `Post::create`/`::find`/
    // `::query()` need no special "pass the pool explicitly" variant.
    let post = Post::create(NewPost { /* ... */ }).await?;
    assert_eq!(Post::query().count().await?, 1);
}).await;
```

{: .warning }
**A real, known gap**: `larust-cache` and `larust-queue` each lazily
create their own table (`cache_items`; `jobs`/`failed_jobs`) behind a
*process-wide* one-time guard, not a per-pool one. If one
`test_transaction()` call exercises code touching Cache or Queue, a
**second**, independent `test_transaction()` call in the same test
*binary* that also touches Cache/Queue will fail with "no such table" -
its fresh pool never got the bootstrap that already fired once,
permanently, for the process. Stick to a single `test_transaction()` call
per test binary if it touches either, or use `test_db` with your own
router instead.

## Model factories

```rust
use larust_support::orm::Factory;

pub struct PostFactory;

impl Factory for PostFactory {
    type Model = Post;

    fn definition(seq: u64) -> NewPost {
        NewPost { title: format!("Post {seq}"), user_id: 0 }
    }
}
```

`xr make:factory Post` generates this shell into `database/factories/`
(mounted into the app's own library target automatically, so both
`tests/*.rs` and `database/seeders` can reach it). `seq` is a
process-wide, ever-incrementing counter - enough to keep a title/email/slug
unique across calls with no faker dependency.

```rust
let post = PostFactory::new().create().await?;                 // builds + persists
let draft = PostFactory::new().make();                          // builds only, no database write

let post = PostFactory::new()
    .state(|p| NewPost { user_id: user.id, ..p })                // override, via struct-update syntax
    .create()
    .await?;

let posts = PostFactory::new().create_many(10).await?;          // sequential - see warning below
```

Eloquent's `make()`/`create()` split, kept as-is: `make()` only ever
builds the typed `New{Model}` value (no I/O at all), `create()` persists
it through the model's own generated `create()`. `.state(...)` queues an
override applied after `definition` - there's no array-merge magic behind
it, just an ordinary closure over the whole typed value.

{: .warning }
`create_many`/`make_many` build sequentially, in the calling task - never
spawned. Unlike `test_db`'s pool, `test_transaction`'s isolated pool is
only reachable through a task-local override; spawning a factory's writes
onto a different task would silently miss it and hit the wrong database
(or none at all).

## `database/seeders`

```rust
// database/seeders/mod.rs
pub async fn run() -> Result<(), larust_core::AppError> {
    PostFactory::new().create_many(10).await?;
    Ok(())
}
```

```bash
xr db:seed                  # runs database/seeders::run() against the current database
xr migrate:fresh --seed     # drop + reapply every migration, then seed
```

Plain functions, not a `Seeder` trait - there's only ever one `run()` an
app actually calls, so there's nothing a trait would add here.

## Faking mail

```rust
larust_testing::fake();
// ... exercise a route that sends mail ...
larust_testing::assert_sent::<WelcomeMail>(|sent| sent.to.contains(&user.email));
```

Reached only through `larust-testing`, never the production
`larust_support::mail` facade - see [Mail &
Notifications](../digging-deeper/mail-and-notifications#testing-mail).

## Running the suite

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --check
```

All three run in this framework's own CI, on both Linux and Windows -
worth doing the same in your own app if you're targeting more than one
platform (zero-downtime restart, path handling, and process management
all have genuinely different code paths per OS).

## Next

[Deployment & Desktop Apps](../deployment-and-desktop-apps).
