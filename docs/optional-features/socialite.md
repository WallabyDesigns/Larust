---
title: Socialite
parent: Optional Features
nav_order: 8
---

# Socialite
{: .no_toc }

OAuth "Sign in with GitHub/Google" social login - Larust's port of
`laravel/socialite`.

1. TOC
{:toc}

## Adding this feature

```bash
# At scaffold time
xr new myapp --features socialite

# Or retrofit onto an existing app
xr add socialite
```

Both are equivalent to adding `"socialite"` to `larust-support`'s
`features = [...]` list in your app's own `Cargo.toml` by hand.

Built-in providers: GitHub and Google, each reading its own
`{PROVIDER}_CLIENT_ID`/`{PROVIDER}_CLIENT_SECRET`/`{PROVIDER}_REDIRECT_URL`
env vars and failing with a clear, named-variable `AppError::Config` if
any are missing - never sending a broken authorize URL to the browser.

## Mapping a provider user to your own schema

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

## The redirect/callback flow

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

## Next

Back to [Optional Features](../../optional-features/) for the full
list, or [Plugins](../../digging-deeper/plugins) for how a crate
contributes its own routes to your app's router.
