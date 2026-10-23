---
title: Db
parent: Optional Features
nav_order: 9
---

# Db
{: .no_toc }

Really two things sharing one dashboard: a **phpMyAdmin/Adminer-style SQL
admin tool** for your app's actual database (its headline purpose), plus
a secondary, embedded key-value store.

1. TOC
{:toc}

## Adding this feature

```bash
# At scaffold time
xr new myapp --features db

# Or retrofit onto an existing app
```

{: .note }
`xr add db` is refused - unlike every other feature on this list, `db`
needs a route wired into your app's own `routes/web.rs` at scaffold
time, not just a `Cargo.toml` feature flag. Add it when you first run
`xr new`, or wire the route in by hand afterward.

Both the `xr new` flag and adding `"db"` to `larust-support`'s own
`features = [...]` list in your app's `Cargo.toml` enable the underlying
crate; `xr new` additionally wires the dashboard route for you.

## The SQL admin dashboard

```
# .env
DB_DASHBOARD_PASSWORD=some-local-only-password   # unset = dashboard refuses to serve at all (fails closed)
DB_DASHBOARD_PATH=xr-db                          # mount path - /xr-db by default
```

Visit `/xr-db` (double-gated: the password above, *and* `APP_DEBUG=true`
- never reachable in a real deploy that leaves debug mode off) to:

- Browse, edit, and delete rows in any table of your actual configured
  database (SQLite/MySQL/Postgres, via `sqlx::AnyPool`) - schema
  (columns, indexes, foreign keys) introspected live, not hardcoded.
- Run raw SQL directly - deliberately unrestricted by design (the same
  posture phpMyAdmin's own SQL tab takes; its safety is the password/
  debug-mode gate above, not query validation).
- Import a `.sql` file.
- Run `migrate:fresh` (drop every table except the framework's own
  `sessions`, reapply every migration) from a button, for when local dev
  state gets into a mess `xr migrate` alone can't fix.

## The embedded key-value store

The same feature also gives you a small embedded key-value store (backed
by [`redb`](https://github.com/cberner/redb), pure Rust, no C toolchain
needed - a real, narrow advantage over SQLite for musl/cross-compile/
minimal-Docker scenarios), for app-local data that never needed
relations (feature flags, small local caches):

```rust
db::put("maintenance_mode", &true).await?;
let enabled: Option<bool> = db::get("maintenance_mode").await?;
db::forget("maintenance_mode").await?;
let all_keys: Vec<String> = db::keys().await?;
```

```bash
xr db:list
xr db:get maintenance_mode
xr db:put maintenance_mode true      # parsed as JSON when possible
xr db:forget maintenance_mode
```

This isn't a second SQL backend - `#[derive(Model)]` needs `sqlx::FromRow`,
which a KV store structurally can't supply. It's additive: reach for it
only for the kind of data that never needed a relational shape in the
first place.

## Next

Back to [Optional Features](../../optional-features/) for the full
list, or [Sitemap](../../optional-features/sitemap) for another small,
additive optional feature.
