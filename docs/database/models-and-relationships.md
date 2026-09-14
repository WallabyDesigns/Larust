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

## Relationships

Four kinds, declared as attributes on the struct - each generates both a
lazy per-instance accessor and a batch eager-loader.

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
