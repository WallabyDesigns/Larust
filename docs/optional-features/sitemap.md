---
title: Sitemap
parent: Optional Features
nav_order: 10
---

# Sitemap
{: .no_toc }

XML sitemap builder - Larust's port of `spatie/laravel-sitemap`.

1. TOC
{:toc}

## Adding this feature

```bash
# At scaffold time
xr new myapp --features sitemap

# Or retrofit onto an existing app
xr add sitemap
```

Both are equivalent to adding `"sitemap"` to `larust-support`'s
`features = [...]` list in your app's own `Cargo.toml` by hand.

## Building a sitemap

Unlike a plugin-shaped feature, there's no `SitemapPlugin` - a sitemap
needs your own app-specific logic to decide what belongs in it (this
crate has no visibility into your `Post` model or any other app-defined
data), so you write the route yourself:

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

## Caching a sitemap response

`larust-sitemap` itself owns no caching - if your sitemap has no
per-viewer state baked in (no CSRF token, no auth-dependent content, as
is normally true for a sitemap), it's a good candidate for
[response caching](../../the-basics/error-handling) via
`larust_http::responsecache::for_minutes(60)` - one of the few pages
where that's actually safe to do without leaking one visitor's session
into another's cached response.

Shared response caching is deliberately conservative: Larust will not store
a response that sets a cookie or marks itself `Cache-Control: private`,
`no-cache`, or `no-store`. That is a useful backstop, not a substitute for
choosing the right cache scope. `for_minutes` is keyed only by URL, so reserve
it for content that is genuinely identical for every visitor. A personalized
response that happens not to set a cookie can still be wrong to share.

| Response | Cache middleware |
| --- | --- |
| Public sitemap, marketing page, or other identical `GET` response | `for_minutes(...)` |
| Expensive response with per-session state | `for_minutes_per_session(...)` |
| Cookie-setting, explicitly private, or uncertain response | Do not use the shared cache |

## Next

Back to [Db](../../optional-features/db) or
[Optional Features](../../optional-features/) for the full list.
