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
```

| Method | Does |
|---|---|
| `.where_eq(column, value)` | `WHERE column = value` |
| `.where_in(column, values)` | `WHERE column IN (...)` - safe against an empty `Vec` (a plain `IN ()` is a SQL syntax error on SQLite; this handles it) |
| `.latest(column)` | `ORDER BY column DESC` |
| `.get()` | Runs the query, returns every matching row |
| `.first()` | Runs the query, returns `Option<T>` |
| `.paginate(per_page)` | `.get()` with a `LIMIT` applied |
| `.count()` | `SELECT COUNT(*)` over the same `WHERE` clause |
| `.exists()` | `true`/`false`, without fetching a row |

Every terminal method (`.get()`/`.first()`/`.count()`/`.exists()`) also
has an `_on(&pool)` variant (`.get_on(&pool)`, etc.) for running against
an explicit pool rather than the process-wide default - the mechanism
[testing](../../testing) relies on for a fully isolated per-test database.

This is intentionally smaller than Eloquent's query builder - there's no
`orWhere`, no arbitrary raw-expression chaining, no query-builder-level
joins. For anything this doesn't cover, drop to a real query directly:
`larust_support::orm::sqlx::query_as(...)` against `larust_support::orm::pool()?`
is a first-class, fully supported escape hatch, not a workaround - see
`PostController::destroy`'s own pivot-table cleanup in the reference app
for a real example of reaching for it.

## Next

[Models & Relationships](../../database/models-and-relationships) covers `#[derive(Model)]`
itself - fields, relationships, and eager loading.
