---
title: Scout
parent: Optional Features
nav_order: 6
---

# Scout
{: .no_toc }

Full-text search against a Meilisearch instance - Larust's port of
`laravel/scout`, Meilisearch only: self-hostable, matching this
framework's own "no required paid service" posture elsewhere
([Reverb](../../optional-features/reverb)'s own WebSocket server,
[Db](../../optional-features/db)'s embedded key-value store).

1. TOC
{:toc}

## Adding this feature

```bash
# At scaffold time
xr new myapp --features scout

# Or retrofit onto an existing app
xr add scout
```

Both are equivalent to adding `"scout"` to `larust-support`'s
`features = [...]` list in your app's own `Cargo.toml` by hand.

```
# .env
MEILISEARCH_URL=http://127.0.0.1:7700   # default if unset
MEILISEARCH_KEY=your-master-key         # optional - omitted if a local dev instance has none
```

## Indexing and searching

```rust
use larust_support::scout;

// `document` must serialize to a JSON object with an "id" field -
// Meilisearch's own default primary-key convention, and the same `id`
// every `#[derive(Model)]` struct already has.
scout::index("posts", &post).await?;
scout::remove("posts", post.id).await?;

#[derive(serde::Deserialize)]
struct PostHit { id: i64, title: String }

let results: scout::SearchResults<PostHit> = scout::search("posts", "rust web framework").await?;
for hit in results.hits {
    println!("{}: {}", hit.id, hit.title);
}
```

No `Searchable` trait and no automatic model-lifecycle indexing - a save/
delete never silently triggers a search-index write on its own. Call
`index`/`remove` explicitly wherever your own code already handles that
write, the same way `larust_queue::dispatch`/`larust_events::dispatch`
are explicit calls rather than implicit hooks. An index that's never had
a document written to it reads as zero search results, not an error -
Meilisearch only auto-creates an index on a write, never on a search.

## Deliberately out of scope

No index configuration (searchable/filterable/sortable attributes, typo
tolerance, synonyms), filters, sorting, or facets in this version - plain
`q`-only search, covering the common case; reach for `reqwest` directly
against Meilisearch's own settings endpoint for anything beyond that.

## Next

Back to [Pennant](../../optional-features/pennant) or
[Optional Features](../../optional-features/) for the full list.
