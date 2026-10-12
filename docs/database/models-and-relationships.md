---
title: Models & Relationships
parent: Database
nav_order: 2
---

# Models & Relationships
{: .no_toc }

1. TOC
{:toc}

## `#[derive(Model)]`

```rust
use larust_support::orm::sqlx;
use larust_support::Model;

#[derive(Model, sqlx::FromRow)]
#[table("posts")]
#[belongs_to(User, foreign_key = "user_id")]
#[has_many(Comment, foreign_key = "post_id")]
pub struct Post {
    #[primary_key]
    pub id: i64,
    pub user_id: i64,
    pub title: String,
    pub content: String,
}
```

`#[table("posts")]` names the table explicitly - there's no automatic
pluralization-of-the-struct-name convention to fight with when it guesses
wrong. `#[primary_key]` marks the id field (any `i64` field; the column
name itself is whatever you named the field). `sqlx::FromRow` is a real,
separate derive you always add alongside `Model` - see [Coming from
Rust](../../coming-from-rust#its-axum-sqlx-and-tower-sessions-underneath---not-a-reimplementation)
for why it can't be folded into `Model` itself.

This generates, for every model:

```rust
pub struct NewPost {          // every field except the primary key
    pub user_id: i64,
    pub title: String,
    pub content: String,
}

impl Post {
    pub async fn create(data: NewPost) -> Result<Post, AppError>;
    pub async fn find(id: i64) -> Result<Option<Post>, AppError>;
    pub async fn update(id: i64, data: NewPost) -> Result<Post, AppError>;
    pub async fn delete(id: i64) -> Result<(), AppError>;
    pub fn query() -> QueryBuilder<Post>;
}
```

...plus a real `FromRequestParts` impl, which is what makes [route model
binding](../../the-basics/routing#route-parameters-and-model-binding) work -
a handler parameter typed `post: Post` resolves by primary key from a
`{post}` path segment automatically, 404ing if nothing matches.

```rust
let post = Post::create(NewPost {
    user_id: user.id,
    title: "Hello".to_string(),
    content: "...".to_string(),
}).await?;

let post = Post::update(post.id, NewPost { user_id: post.user_id, title: "Edited".to_string(), content: post.content }).await?;

Post::delete(post.id).await?;
```

Every field on `NewPost` mirrors `Post`'s own real, typed fields -
there's no dynamic attribute bag, so an update always carries every
column's full value (matching the reference app's own convention of
building a `NewPost` from the existing row's other fields when only one
is actually changing, as `PostController::update`/`ProfileController`
both do).

## `created_at` / `updated_at`: `#[timestamps]`

Laravel's `$table->timestamps()` plus Eloquent's default auto-touch
behavior:

```rust
#[derive(Model, sqlx::FromRow)]
#[table("posts")]
#[timestamps]
pub struct Post {
    #[primary_key]
    pub id: i64,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
}
```

```sql
-- migration
CREATE TABLE posts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    title TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
```

Both columns are `i64` - Unix seconds, the same convention every other
framework-owned timestamp column already uses (`larust-queue`'s `jobs`
table, `larust-notifications`' own table), not a SQL `DATETIME`/
`TIMESTAMP` type. `create()` stamps both to the same value; `update()`
re-stamps `updated_at` only, leaving `created_at` untouched. Neither field
appears on the generated `NewPost` - exactly like the primary key, they're
never something you set yourself:

```rust
let post = Post::create(NewPost {
    title: "Hello".to_string(),
    // no created_at/updated_at here - #[timestamps] owns both
}).await?;
assert_eq!(post.created_at, post.updated_at); // just-created: identical

let post = Post::update(post.id, NewPost { title: "Edited".to_string() }).await?;
// post.updated_at just moved forward; post.created_at didn't
```

`#[timestamps]` is opt-in, not automatic on every model - this framework
requires an explicit `#[primary_key]` even though `id` would be an
equally obvious convention, so a model without the attribute is
completely unaffected; add both fields and the attribute only where you
actually want the behavior. `xr convert` already emits exactly this
`created_at INTEGER`/`updated_at INTEGER` shape for a Laravel migration's
`$table->timestamps()` - add `#[timestamps]` to the matching
`#[derive(Model)]` struct afterward to wire up the same auto-population
Eloquent had.

## `deleted_at`: `#[soft_deletes]`

Laravel's `SoftDeletes` trait:

```rust
#[derive(Model, sqlx::FromRow)]
#[table("posts")]
#[soft_deletes]
pub struct Post {
    #[primary_key]
    pub id: i64,
    pub title: String,
    pub deleted_at: Option<i64>,
}
```

```sql
-- migration
CREATE TABLE posts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    title TEXT NOT NULL,
    deleted_at INTEGER
);
```

`deleted_at` is `Option<i64>` (`None` until trashed, Unix seconds once
set) and, like `#[timestamps]`'s two columns, never appears on the
generated `NewPost` - it's managed only through `delete()`/`restore()`,
never set by hand. Once opted in:

```rust
Post::delete(post.id).await?;        // soft-deletes: sets deleted_at
Post::find(post.id).await?;          // None - normal reads exclude trashed rows
Post::all().await?;                  // excludes it too, same as query()

Post::find_with_trashed(post.id).await?;      // Some(post) - sees it anyway
Post::query().with_trashed().get().await?;    // every row, trashed or not
Post::query().only_trashed().get().await?;    // only trashed rows

Post::restore(post.id).await?;       // un-deletes it
Post::force_delete(post.id).await?;  // the real delete - bypasses deleted_at entirely
post.trashed();                      // true/false, on an already-loaded row
```

`.with_trashed()`/`.only_trashed()` only exist on a `QueryBuilder<T>`
for a `T` that's actually opted into `#[soft_deletes]` - calling either
on a model that hasn't is a compile error, not a silent no-op. A
`#[belongs_to_many(...)]` relationship also respects the *related*
model's own `#[soft_deletes]` automatically - a trashed tag disappears
from `post.tags()` the same way it would from `Tag::query()` directly.
`has_many`/`has_one`/`belongs_to` already get this for free, since they
route through the related model's own `query()`/`find()`.

Deliberately not included: deleting a row still never runs `down()` on
anything, since migrations stay forward-only (see [Migrations & the
Query Builder](../database/migrations-and-query-builder#no-rollback---xr-migratefresh-instead)) -
`#[soft_deletes]` is about individual rows, not a rollback story for
schema changes.

## Model events: `#[observe(...)]`

Laravel's model-event observers - a plain function, not a class:

```rust
#[derive(Model, sqlx::FromRow)]
#[table("posts")]
#[observe(creating = PostObserver::creating, created = PostObserver::created)]
pub struct Post {
    #[primary_key]
    pub id: i64,
    pub title: String,
}

struct PostObserver;

impl PostObserver {
    async fn creating(data: NewPost) -> Result<NewPost, AppError> {
        // Transform the data before it's inserted, or veto the write
        // entirely by returning `Err(...)`.
        Ok(NewPost { title: data.title.trim().to_string(), ..data })
    }

    async fn created(post: &Post) -> Result<(), AppError> {
        larust_support::event::dispatch(PostCreated { id: post.id });
        Ok(())
    }
}
```

Six hooks, each a plain path to an `async fn` with a fixed signature -
`creating`/`updating` (before the write, can transform the data or veto
by returning `Err`), `created`/`updated`/`deleted` (after, with the real
row), `deleting` (before the real/soft delete). A typo'd hook name
(`#[observe(creatign = ...)]`) or a hook with the wrong signature is a
compile error naming exactly what's wrong - not a silently-skipped
default, since there's no trait with optional methods behind this.

Fires only through `create`/`update`/`delete` (which includes model
factories and `Repository<T>`, since both forward to these same
methods) - never for a raw `sqlx` call, and never for pivot `attach`/
`detach`/`sync`. `deleting`/`deleted` don't fire at all for an id that
never existed - `delete()` keeps its existing "not an error if it
didn't exist" behavior unchanged.

Not yet supported: `restoring`/`restored`/`force_deleting`/
`force_deleted` for `#[soft_deletes]`'s own `restore()`/`force_delete()`.

## Relationships

Five kinds, declared as attributes on the struct. The first four each
generate both a lazy per-instance accessor and a batch eager-loader;
`has_many_through` (below) doesn't have the eager-loader form yet.

### `belongs_to` / `has_one`

```rust
#[belongs_to(User, foreign_key = "user_id")]   // this row's own user_id points at the related row
struct Post { ... }

#[has_one(Profile, foreign_key = "user_id")]   // the *related* row's foreign key points back at this one
struct User { ... }
```

Generates `post.user() -> Result<Option<User>, AppError>` /
`user.profile() -> Result<Option<Profile>, AppError>` - the difference
between the two is only which side owns the foreign key, exactly like
Laravel's own `belongsTo`/`hasOne`.

### `has_many`

```rust
#[has_many(Comment, foreign_key = "post_id")]
struct Post { ... }
```

Generates `post.comments() -> Result<Vec<Comment>, AppError>`. The
default method name is the related type's name, pluralized and
snake-cased (`Comment` → `comments`) - override it with `method =
"replies"` if you need a different name (e.g. two `has_many` relations to
the same related type on one model).

### `belongs_to_many` (many-to-many, through a pivot table)

```rust
#[belongs_to_many(
    Tag,
    through = "post_tag",
    foreign_key = "post_id",
    related_pivot_key = "tag_id"
)]
struct Post { ... }
```

Generates four methods:

```rust
post.tags().await?;                 // Vec<Tag>, via an INNER JOIN through post_tag
post.attach_tag(tag_id).await?;     // insert one pivot row - a no-op if already attached, not a UNIQUE error
post.detach_tag(tag_id).await?;     // remove one pivot row
post.sync_tags(vec![id1, id2]).await?;  // replace the full set in one transaction
```

`attach_*`/`detach_*`/`sync_*` are named after the *singular*/*plural*
related type name respectively - `attach_tag`/`detach_tag`/`sync_tags`
for `Tag`, following from the same naming rule as `has_many`.

Deleting a row doesn't automatically clean up its pivot rows - if a
`Post` is deleted, its `post_tag` rows aren't cascade-deleted for you
unless your migration's own `REFERENCES ... ON DELETE CASCADE` says so.
The reference app's `PostController::destroy` shows the explicit
alternative: a plain `DELETE FROM post_tag WHERE post_id = ?` run before
`Post::delete(...)`.

If `Tag` is itself marked `#[soft_deletes]`, `post.tags()` automatically
excludes a trashed tag, the same as `Tag::query()` would.

### `has_many_through` (a relationship through a third model)

```rust
#[has_many_through(Post, through = User, first_key = "country_id", second_key = "user_id")]
struct Country { ... }
```

Laravel's `hasManyThrough` - `Country` has many `Post`, through `User`
(every post belongs to a user, every user belongs to a country). Note
`through = User` is a type, not a string, since the two keys *are*
strings: `first_key` is the column on `through` (`users`) that points
back at *this* model (`users.country_id`), `second_key` is the column on
the related table (`posts`) that points at `through` (`posts.user_id`).
Both required - no guessing from naming convention.

```rust
country.posts().await?;                       // Vec<Post>, across users
country.posts_query().latest(Post::ID).limit(5).get().await?;  // composes like any other query
```

`posts_query()` returns a plain `QueryBuilder<Post>` - chain `.scope(...)`/
`.where_op(...)`/`.latest(...)` etc. on it exactly like `Post::query()`
itself, before running it with `.posts()`'s own default `.get()` or your
own terminal method. No eager-loading (`load_*`) form yet - a real,
documented gap, not an oversight.

## Eager loading (avoiding N+1)

Every relationship's batch loader takes a slice of rows and returns a
**lookup map**, not a mutated collection - a deliberate difference from
Laravel's `::with(...)`, which attaches results back onto each model in
place:

```rust
let posts = Post::query().paginate(20).await?;
let authors: HashMap<i64, User> = Post::load_user(&posts).await?;   // one query, not one per post

for post in &posts {
    let author = authors.get(&post.user_id);   // no query here
}
```

Why a map instead of mutation: a generated `Post` struct has no field to
attach a `User` onto (adding one would mean every `Post` value carries an
`Option<User>` slot whether or not it was ever loaded), and Rust's own
ownership rules make "mutate this vec of structs to attach borrowed
related data" considerably more awkward than in a dynamically-typed
language. A `HashMap` keyed by id, looked up per row in your own template
or handler code, is the one that actually fits real Rust ergonomics.

`load_*` deduplicates ids before querying (several rows sharing one
author only queries that author once) - the same discipline
`PostController::index`'s own eager-loaded author list is built and
verified against in this framework's own test suite (2 queries for a
whole page of posts + authors, not N+1, checked directly rather than
assumed).

## Next

[Digging Deeper](../../digging-deeper/) covers everything else an app tends
to need - auth, mail, queues, and the reactive `@wire`/`@live` components
that make Larust more than "Laravel's directory layout with a Rust
compiler bolted on."
