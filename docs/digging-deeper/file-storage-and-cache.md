---
title: File Storage & Cache
parent: Digging Deeper
nav_order: 4
---

# File Storage & Cache
{: .no_toc }

1. TOC
{:toc}

## File Storage

Two fixed disks - Laravel's `Storage::disk('local')`/`Storage::disk('public')`,
as plain functions rather than a config-driven registry you can add a
third named disk to:

```rust
storage::local().put("reports/2024.csv", &bytes).await?;   // storage/app/ - never web-accessible
storage::public().put("uploads/avatar.png", &bytes).await?; // public/ - served at the URL root
let url = storage::public().url("uploads/avatar.png")?;      // Some("/uploads/avatar.png")
let bytes = storage::public().get("uploads/avatar.png").await?;  // Option<Vec<u8>>
storage::public().exists("uploads/avatar.png").await?;
storage::public().delete("uploads/avatar.png").await?;
```

Every path is checked for traversal (`../`, an absolute path, an
existing symlink pointing outside the disk root) before touching disk -
`put`/`get`/`exists`/`delete`/`url` all reject an escaping path rather
than silently resolving it. `local()`/`public()` lazily create their root
directory (`storage/app/`/`public/`) the first time they're used, so a
freshly scaffolded app's upload route doesn't 500 just because the
directory doesn't exist yet.

## Cache

```
# .env
CACHE_DRIVER=database   # default - the same connection DB_CONNECTION points at
# CACHE_DRIVER=redis    # REDIS_URL=redis://127.0.0.1:6379
```

```rust
cache::put("homepage:post_count", &count, Duration::from_secs(300)).await?;
let count: Option<i64> = cache::get("homepage:post_count").await?;
cache::forget("homepage:post_count").await?;

let count: i64 = cache::remember("homepage:post_count", Duration::from_secs(300), || async {
    Post::query().count().await
}).await?;
```

`remember` is the one you reach for most - Laravel's own
`Cache::remember('key', $ttl, fn () => ...)`: return the cached value if
present, otherwise run the closure, cache its result, and return it. Any
`Serialize + DeserializeOwned` value works, stored as JSON.

`larust-cache` (and `larust-queue`, above) sharing one `database`/`redis`
choice per app - rather than each having its own independent driver
matrix - is a deliberate simplification: pick one, and both the cache
table and the job queue live wherever it makes sense for your deployment.

## Next

[Reactive Components](../../digging-deeper/reactive-components) - `@wire(...)`, this
framework's Livewire-shaped answer to building interactive UI without a
separate frontend build.
