---
title: Social Login & Plugins
parent: Digging Deeper
nav_order: 7
---

# Social Login & Plugins
{: .no_toc }

1. TOC
{:toc}

## Social login: `larust-socialite`

An optional `xr new` feature (`--features socialite`). Built-in providers:
GitHub and Google, each reading its own `{PROVIDER}_CLIENT_ID`/
`{PROVIDER}_CLIENT_SECRET`/`{PROVIDER}_REDIRECT_URL` env vars and failing
with a clear, named-variable `AppError::Config` if any are missing -
never sending a broken authorize URL to the browser.

Your `Authenticatable` type implements one hook to say how a provider
user maps to your own schema:

```rust
impl SocialiteUser for User {
    async fn find_or_create_from_provider(provider: &str, user: &ProviderUser) -> Result<Self, AppError> {
        if let Some(existing) = User::query().where_eq("email", user.email.as_deref().unwrap_or("")).first().await? {
            return Ok(existing);
        }
        User::create(NewUser {
            name: user.name.clone().unwrap_or_default(),
            email: user.email.clone().unwrap_or_default(),
            password_hash: String::new(),   // no password - this account only ever logs in via OAuth
        }).await
    }
}
```

```rust
// 1. Send the user to the provider.
pub async fn redirect(session: Session) -> Result<impl IntoResponse, AppError> {
    let provider = larust_support::socialite::github()?;
    let url = larust_support::socialite::redirect_url(&session, "github", &provider).await?;
    Ok(Redirect::to(&url))
}

// 2. Handle the callback.
pub async fn callback(session: Session, Query(params): Query<CallbackParams>) -> Result<impl IntoResponse, AppError> {
    let provider = larust_support::socialite::github()?;
    let user: User = larust_support::socialite::user_from_callback(
        &session, "github", &provider, &params.code, &params.state,
    ).await?;
    larust_support::auth::login(&session, &user).await?;
    Ok(redirect().route("posts.index")?)
}
```

`redirect_url` generates and session-stores a random `state` value (keyed
per-provider, so two concurrent OAuth attempts in different tabs don't
clobber each other); `user_from_callback` verifies it in constant time
before ever exchanging the authorization code, rejecting a
missing/expired/mismatched state with one generic error - never revealing
which, the same instinct this framework's password checks already
follow. Note that `user_from_callback` **doesn't log the user in itself**
- it returns the resolved user and leaves calling `auth::login(...)` to
you, the same explicit shape `AuthController::register` already uses,
rather than authenticating a session as a side effect you didn't ask for.

## Plugins

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

[Key-Value Store & Sitemaps](../../digging-deeper/key-value-store-and-sitemaps).
