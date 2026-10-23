---
title: Passport
parent: Optional Features
nav_order: 4
---

# Passport
{: .no_toc }

Laravel Passport's core: a full OAuth2 authorization server (RFC 6749),
narrowed to the three grant types nearly every real integration actually
needs. Your app issues and validates its own tokens for third-party
clients calling its API - a different job from
[`larust-socialite`](../../optional-features/socialite) (your app as an
OAuth *client*, "login with GitHub") or
[`larust-sanctum`](../../optional-features/sanctum) (simple bearer-token
API auth with no client/scope concept at all).

1. TOC
{:toc}

## Adding this feature

```bash
# At scaffold time
xr new myapp --features passport

# Or retrofit onto an existing app
xr add passport
```

Both are equivalent to adding `"passport"` to `larust-support`'s
`features = [...]` list in your app's own `Cargo.toml` by hand.

## What's provided vs. what you wire yourself

`larust-passport` owns the OAuth protocol mechanics - client storage,
code/token issuance and validation, PKCE verification, rotation. It owns
no HTTP routes and no UI: your own app registers `/oauth/authorize` and
`/oauth/token`, and (if you support the Authorization Code grant) your
own consent screen. That split matches every other optional crate in
this framework - `larust-sanctum`/`larust-socialite` don't auto-register
routes either.

## Registering a client

```rust
let client = passport::create_client("Mobile App", "myapp://callback", false).await?;
// client.client_id   - always present
// client.client_secret - Some(...) only for a confidential client (the third argument, `true`)
```

There's no `passport:client`-equivalent interactive CLI - call this from
your own seeder, console command, or a one-off script. A **confidential**
client (a server you control, holding a real secret) can use the Client
Credentials grant; a **public** client (a mobile app, a single-page app -
anything that can't keep a secret truly secret) relies entirely on PKCE
instead. One redirect URI per client - a client needing more than one
registers more than one row.

## The Authorization Code grant (with mandatory PKCE)

The flow a real third-party app (web or mobile) uses to get a token on a
user's behalf. PKCE is **required unconditionally** - `S256` only, never
`"plain"` - for every client, not just public ones (there's no real
downside to requiring it, and a real upside: a leaked client secret alone
is no longer sufficient to redeem a stolen authorization code).

Your own `/oauth/authorize` route authenticates the user (via your app's
existing session/`Auth<U>`) and shows them a consent screen - once they
approve, issue the code:

```rust
async fn authorize(auth: Auth<User>, Query(params): Query<AuthorizeParams>) -> impl IntoResponse {
    // ... render/confirm consent here, using your own UI ...
    let code = passport::create_authorization_code(passport::AuthorizeRequest {
        client_id: params.client_id,
        redirect_uri: params.redirect_uri,
        user_id: auth.0.id,
        scopes: params.scope.split_whitespace().map(str::to_string).collect(),
        code_challenge: params.code_challenge,
        code_challenge_method: passport::PkceMethod::parse(&params.code_challenge_method)
            .ok_or_else(|| /* 400 */)?,
    }).await?;

    Redirect::to(&format!("{}?code={code}&state={}", params.redirect_uri, params.state))
}
```

Your own `/oauth/token` route exchanges it:

```rust
async fn token(Form(body): Form<TokenParams>) -> Result<Json<passport::TokenResponse>, passport::OAuthError> {
    let response = passport::exchange_token(passport::TokenRequest::AuthorizationCode {
        code: body.code,
        redirect_uri: body.redirect_uri,
        client_id: body.client_id,
        code_verifier: body.code_verifier,
    }).await?;
    Ok(Json(response))
}
```

The authorization code is short-lived (10 minutes) and single-use by
construction - a replayed or wrong-`code_verifier` exchange attempt fails
without ever consuming it, so a legitimate client's own retry with the
corrected values still works.

## Client Credentials (machine-to-machine)

For a confidential client authenticating as itself, no user involved:

```rust
let response = passport::exchange_token(passport::TokenRequest::ClientCredentials {
    client_id: client.client_id,
    client_secret: client.client_secret.unwrap(),
    scope: Some("server-to-server".to_string()),
}).await?;
// response.refresh_token is always None here - the client already holds
// long-lived credentials and can simply request a fresh token the same
// way again any time.
```

A public (non-confidential) client is rejected outright for this grant -
it has no secret to authenticate with.

## Refresh Token rotation

```rust
let refreshed = passport::exchange_token(passport::TokenRequest::RefreshToken {
    refresh_token: response.refresh_token.unwrap(),
    client_id: client.client_id,
}).await?;
```

Every refresh issues a brand-new access/refresh pair **and immediately
revokes the old one** - never left valid alongside the new pair. This
makes a stolen refresh token's theft detectable: the legitimate client's
own next refresh attempt fails loudly (instead of both the attacker and
the real client silently continuing to mint tokens from the same
secret), rather than quietly succeeding forever.

## Protecting a route

```rust
// Needs a resolvable user - rejects a token with no bound user (one
// issued via Client Credentials) the same as a missing/invalid token.
async fn me(auth: passport::OAuthAuth<User>) -> Json<UserResponse> {
    Json(UserResponse::from(auth.user))
}

// Works for *any* valid token, user-bound or not - the machine-to-machine
// counterpart.
async fn stats(auth: passport::OAuthClientAuth) -> Json<Stats> {
    passport::require_scope(&auth.scopes, "read-stats")?;
    Json(compute_stats())
}
```

`has_scope`/`require_scope` are plain, explicit checks your own handler
calls - not a const-generic extractor variant per scope name. Scopes are
app-chosen strings with no fixed vocabulary this crate could enumerate at
compile time the way a single app's own `PermissionName` enum can.

## Revoking tokens

```rust
passport::revoke_access_token(&token).await?;   // also revokes its own refresh token
passport::revoke_refresh_token(&refresh_token).await?;  // narrower - leaves the access token alone
```

Not an error to revoke a token that's already revoked, expired, or never
existed - RFC 7009 itself requires a revocation endpoint to respond the
same way regardless, specifically so a client can't use the response to
probe which tokens are real.

## Deliberately out of scope

- **One redirect URI per client** - see [Registering a client](#registering-a-client).
- **No `passport:client`-equivalent interactive CLI.**
- **No scopes UI/consent-screen rendering** - your own `/oauth/authorize`
  route owns that.
- **No `@feature`/`@endfeature`-style Blade directive** for scope checks -
  call `has_scope`/`require_scope` directly.

## Next

Back to [Optional Features](../../optional-features/) for the full list,
or [Authentication & Authorization](../../digging-deeper/authentication-and-authorization)
for session-based auth and [Sanctum](../../optional-features/sanctum)'s
simpler bearer tokens.
