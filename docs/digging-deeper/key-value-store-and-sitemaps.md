---
title: Key-Value Store & Sitemaps
parent: Digging Deeper
nav_order: 8
---

# Key-Value Store & Sitemaps
{: .no_toc }

1. TOC
{:toc}

## Database admin dashboard (`larust-db`)

An optional `xr new` feature (`--features db`) that's really two things
sharing one dashboard: a **phpMyAdmin/Adminer-style SQL admin tool** for
your app's actual database (its headline purpose), plus a secondary,
embedded key-value store.

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

## Sitemaps (`larust-sitemap`)

Unlike the plugins covered elsewhere in this section, there's no
`SitemapPlugin` - a sitemap needs your own app-specific logic to decide
what belongs in it (this crate has no visibility into your `Post` model
or any other app-defined data), so you write the route yourself:

```rust
async fn sitemap() -> impl IntoResponse {
    let public_routes: Vec<_> = routes().routes().into_iter()
        .filter(|r| !r.path.starts_with("/__larust_"))   // skip framework-internal routes
        .collect();

    let mut entries = sitemap::from_static_routes(&url(""), &public_routes);

    for post in Post::all().await.unwrap_or_default() {
        entries.push(
            sitemap::SitemapEntry::new(url(&format!("/posts/{}", post.id)))
                .change_freq(sitemap::ChangeFreq::Weekly)
        );
    }

    sitemap::response(&entries)
}
```

`from_static_routes` covers every static `GET` page straight from your
router's own route table; add dynamic, per-record entries (as above) by
hand, since only your app knows what those are. `sitemap::response(...)`
builds the final XML with the correct content type.

`larust-sitemap` itself owns no caching - if your sitemap has no
per-viewer state baked in (no CSRF token, no auth-dependent content, as
is normally true for a sitemap), it's a good candidate for
[response caching](../../the-basics/error-handling) via
`larust_http::responsecache::for_minutes(60)` - one of the few pages
where that's actually safe to do without leaking one visitor's session
into another's cached response.

## Next

You've now seen every major feature area. [The CLI Reference](../../cli-reference)
covers `xr` end to end, or jump to [Testing](../../testing) /
[Deployment](../../deployment-and-desktop-apps).
