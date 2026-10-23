---
title: Plugins
parent: Digging Deeper
nav_order: 7
---

# Plugins
{: .no_toc }

1. TOC
{:toc}

## `Plugin`

A `Plugin` is how a crate - yours or a third party's - contributes routes
to an app's router, without a dynamic runtime registry (a compiled
language has no dynamic loading; this is the compiler-verified
equivalent):

```rust
pub trait Plugin {
    fn routes(&self) -> Router { Router::new() }
}
```

```rust
struct HealthPlugin;
impl Plugin for HealthPlugin {
    fn routes(&self) -> Router {
        Route::get("/health", || async { "ok" })
    }
}
```

```rust
route.plugin(HealthPlugin)
```

`larust_support::wire::WirePlugin`, `spa::SpaPlugin`, `reverb::ReverbPlugin`,
and `push::PushPlugin` are all built this way - each is exactly the
routes that used to be hand-copied into every generated app's
`routes/web.rs` before plugins existed.

{: .warning }
**Call `.plugin(...)` only at the top level of a chain, never nested
inside a `.group(...)`.** `Router::group` applies the group's own
middleware to everything merged into it, including a nested plugin's
already-absolute paths - a real, previously-hit, now-regression-tested
landmine. If you need a plugin's routes gated by middleware, register the
plugin at the top level and apply middleware to the routes you actually
own instead.

## Next

[Socialite](../../optional-features/socialite) is built this way for its
OAuth provider plumbing, or jump to
[Localization](../../digging-deeper/localization).
