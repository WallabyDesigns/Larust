---
title: Pennant
parent: Optional Features
nav_order: 5
---

# Pennant
{: .no_toc }

Persisted, scope-aware boolean feature flags - Larust's port of
`laravel/pennant`.

1. TOC
{:toc}

## Adding this feature

```bash
# At scaffold time
xr new myapp --features pennant

# Or retrofit onto an existing app
xr add pennant
```

Both are equivalent to adding `"pennant"` to `larust-support`'s
`features = [...]` list in your app's own `Cargo.toml` by hand.

## Checking a flag

No `Feature::define(...)` resolver registry: the caller supplies its own
default right where a flag is checked, instead of registering one
somewhere else the whole app has to agree on up front.

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

## Setting a flag

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

## Deliberately out of scope

Boolean-only - no non-boolean "rich" values the way real Pennant also
supports, and no `@feature`/`@endfeature` Blade directive (`larust-view`'s
own directive set is a fixed list in its parser, not something an
optional crate can extend - a real but separate, larger change). Check a
flag from a controller or view-context-building code the same way
`Feature::active(...)` is usable outside Blade in real Laravel too.

## Next

[Scout](../../optional-features/scout) covers full-text search, the
other "flagged-off by default, persisted state" feature in this list.
Back to [Optional Features](../../optional-features/) for the full
list.
