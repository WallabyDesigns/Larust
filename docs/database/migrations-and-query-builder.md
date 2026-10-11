---
title: Migrations & the Query Builder
parent: Database
nav_order: 1
---

# Migrations & the Query Builder
{: .no_toc }

1. TOC
{:toc}

## Migrations are plain, forward-only SQL

```
database/migrations/
├── 0001_create_posts_table.sql
├── 0002_create_users_table.sql
└── 0003_create_comments_table.sql
```

```sql
-- 0001_create_posts_table.sql
CREATE TABLE posts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id INTEGER NOT NULL REFERENCES users(id),
    title TEXT NOT NULL
);
```

There's no PHP-class migration API, no `Schema::create(...)`/`Blueprint`
builder, and - the real, deliberate departure from Laravel - **no
`down()` method and no rollback**. Every migration is real SQL, applied
once, in order, by filename. `xr make:migration create_categories_table`
creates a new empty, correctly-numbered file for you to fill in.

```bash
xr migrate
```

```
Migrated: 0001_create_posts_table.sql
Migrated: 0002_create_users_table.sql
Migrated: 0003_create_comments_table.sql
```

Applies every migration that hasn't run yet, tracked the same way
Laravel's own migration table works - already-applied files are skipped
on the next run.

### No rollback - `xr migrate:fresh` instead

Laravel's `migrate:rollback` (undo the last batch via each migration's
`down()`) has no honest Larust equivalent, because there's no `down()`
anywhere in this codebase to run. What you get instead is the tool this
framework can actually stand behind:

```bash
xr migrate:fresh
```

Drops every table (except the framework's own `sessions` table - dropping
that out from under an already-running server's session middleware would
break every logged-in user with no recovery short of a restart) and
reapplies every migration from scratch. It's a fresh start, not a
targeted undo - reach for it in local dev, not as a production rollback
tool.

## The Query Builder

`#[derive(Model)]` gives every model a `::query()` entry point into a
small, chainable `QueryBuilder<T>`:

```rust
let recent_posts = Post::query()
    .where_eq("user_id", user.id)
    .latest("id")
    .paginate(20)
    .await?;

let drafts_and_published = Post::query()
    .where_in("status", vec!["draft", "published"])
    .get()
    .await?;

let exists = Post::query().where_eq("title", &title).exists().await?;

// Richer comparisons, OR, parenthesized groups, reusable scopes, and a
// subquery filter in place of a real JOIN:
let popular_from_country = Post::query()
    .where_op(Post::VOTES, Op::Gt, 10i64)
    .scope(Post::published)
    .where_in_query(Post::USER_ID, User::ID, User::query().where_eq(User::COUNTRY_ID, 1i64))
    .order_by(Post::VOTES, Order::Desc)
    .limit(20)
    .get()
    .await?;
```

| Method | Does |
|---|---|
| `.where_eq(column, value)` | `WHERE column = value` |
| `.where_op(column, op, value)` | `WHERE column <op> value` - `Op::{Ne,Gt,Gte,Lt,Lte,Like}` |
| `.where_null(column)` / `.where_not_null(column)` | `WHERE column IS [NOT] NULL` |
| `.where_in(column, values)` | `WHERE column IN (...)` - safe against an empty `Vec` (a plain `IN ()` is a SQL syntax error on SQLite; this handles it) |
| `.or_where_eq`/`.or_where_op`/`.or_where_in`/`.or_where_null` | Same as their non-`or_` siblings, joined with `OR` instead of `AND` |
| `.where_group(\|q\| ...)` / `.or_where_group(\|q\| ...)` | A parenthesized `(...)` sub-clause - only `q`'s own added conditions become the group's contents |
| `.scope(function)` | A reusable, named `WHERE` preset (Eloquent's local scopes) - automatically parenthesized only if the scope itself uses `OR`, so it can never leak precedence into the rest of the query |
| `.where_in_query(column, sub_column, sub_builder)` | `WHERE column IN (SELECT sub_column FROM ... [WHERE ...])` - a subquery filter against another model's table; the deliberate stand-in for a real `JOIN` (see below) |
| `.latest(column)` / `.oldest(column)` / `.order_by(column, Order)` | `ORDER BY` - each call adds a sort key, same as chaining Eloquent's `->orderBy(...)` |
| `.limit(n)` | Caps the row count, independent of `.paginate(...)` |
| `.get()` | Runs the query, returns every matching row |
| `.first()` | Runs the query, returns `Option<T>` |
| `.paginate(per_page)` | `.get()` with a `LIMIT` applied |
| `.count()` | `SELECT COUNT(*)` over the same `WHERE` clause |
| `.exists()` | `true`/`false`, without fetching a row |

Every terminal method (`.get()`/`.first()`/`.count()`/`.exists()`) also
has an `_on(&pool)` variant (`.get_on(&pool)`, etc.) for running against
an explicit pool rather than the process-wide default - the mechanism
[testing](../../testing) relies on for a fully isolated per-test database.

### Why `where_in_query`, not a real `JOIN`

Still intentionally smaller than Eloquent's query builder in one
specific way: there's no query-builder-level `JOIN`. `QueryBuilder`
decodes every row with `SELECT *` into one `sqlx::FromRow` struct, and
its column constants (`Post::TITLE`) are bare, unqualified names - both
assumptions a real multi-table `JOIN` would break (an ambiguous `id`
column, an ambiguous bare column name shared by two tables) without a
breaking redesign (a qualified `Column { table, name }` type everywhere
instead of a plain `&'static str`). `.where_in_query(...)` covers the
same "filter by a related table's rows" need (Eloquent's `whereHas`,
and what `hasManyThrough` is built on) with neither problem, since its
subquery still only ever returns one column. For anything this doesn't
cover either, drop to a real query directly:
`larust_support::orm::sqlx::query_as(...)` against `larust_support::orm::pool()?`
is a first-class, fully supported escape hatch, not a workaround - see
`PostController::destroy`'s own pivot-table cleanup in the reference app
for a real example of reaching for it.

## Next

[Models & Relationships](../../database/models-and-relationships) covers `#[derive(Model)]`
itself - fields, relationships, and eager loading.
