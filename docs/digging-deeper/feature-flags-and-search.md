---
title: Feature Flags & Search
parent: Digging Deeper
nav_order: 11
---

# Feature Flags & Search
{: .no_toc }

1. TOC
{:toc}

## Feature flags: `larust-pennant`

`xr new --features pennant` - persisted, scope-aware boolean flags
(Laravel's `laravel/pennant`), with no `Feature::define(...)` resolver
registry: the caller supplies its own default right where a flag is
checked, instead of registering one somewhere else the whole app has to
agree on up front.

```rust
use larust_support::pennant;

// Checked with an explicit default - "what if nothing's been decided
// yet for this exact (name, scope) pair".
let enabled = pennant::active("new-dashboard", None, false).await?;

// Scoped per user (or any other string key - a tenant slug, an email,
// whatever your app's own rollout logic cares about).
let scope = format!("user:{}", user.id);
let beta = pennant::active("beta-ui", Some(&scope), false).await?;
```

Persisting a decision - from an admin panel, a console command, a
percentage-rollout job, wherever your own app decides - is just as
explicit:

```rust
pennant::activate("new-dashboard", None).await?;           // on for everyone
pennant::activate("beta-ui", Some(&scope)).await?;          // on for one user
pennant::deactivate("beta-ui", Some(&scope)).await?;        // off again
pennant::forget("beta-ui", Some(&scope)).await?;            // back to whatever default future checks pass
pennant::purge("beta-ui").await?;                           // retire the flag entirely, every scope
```

Boolean-only - no non-boolean "rich" values the way real Pennant also
supports, and no `@feature`/`@endfeature` Blade directive (`larust-view`'s
own directive set is a fixed list in its parser, not something an
optional crate can extend - a real but separate, larger change). Check a
flag from a controller or view-context-building code the same way
`Feature::active(...)` is usable outside Blade in real Laravel too.

## Full-text search: `larust-scout`

`xr new --features scout` - full-text search (Laravel's `laravel/scout`),
Meilisearch only: self-hostable, matching this framework's own "no
required paid service" posture elsewhere (`larust-reverb`'s own
WebSocket server, `larust-db`'s embedded key-value store).

```
# .env
MEILISEARCH_URL=http://127.0.0.1:7700   # default if unset
MEILISEARCH_KEY=your-master-key         # optional - omitted if a local dev instance has none
```

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

No index configuration (searchable/filterable/sortable attributes, typo
tolerance, synonyms), filters, sorting, or facets in this version - plain
`q`-only search, covering the common case; reach for `reqwest` directly
against Meilisearch's own settings endpoint for anything beyond that.

## Next

[OAuth2 Authorization Server](oauth2-authorization-server) covers
`larust-passport`, or back to [Optional Features](optional-features) for
the full list.
