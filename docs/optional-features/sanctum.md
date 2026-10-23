---
title: Sanctum
parent: Optional Features
nav_order: 1
---

# Sanctum
{: .no_toc }

API bearer-token authentication for non-browser clients - Laravel
Sanctum's personal-access-token flow.

1. TOC
{:toc}

## Adding this feature

```bash
# At scaffold time
xr new myapp --features sanctum

# Or retrofit onto an existing app
xr add sanctum
```

Both are equivalent to adding `"sanctum"` to `larust-support`'s
`features = [...]` list in your app's own `Cargo.toml` by hand.

## Issuing a token

```rust
let plaintext = larust_sanctum::create_token(&user, "cli-tool", None).await?;
// "42|f3a1...c9" - shown to the caller exactly once; only its hash is
// ever stored, so it can't be recovered from a database dump later.
```

## Protecting a route

```rust
pub async fn me(ApiAuth(user): ApiAuth<User>) -> Json<UserResponse> { ... }
```

`ApiAuth<U>` is the API-route counterpart to `Auth<U>` (see
[Authentication & Authorization](../../digging-deeper/authentication-and-authorization)) -
it authenticates via a `Bearer <token>` header instead of a session
cookie, resolving the same `Authenticatable` type.

## Revoking tokens

```rust
larust_sanctum::revoke_token(id).await?;
larust_sanctum::revoke_all_tokens_for(&user).await?;
```

Laravel's `$token->delete()`/`$user->tokens()->delete()`.

## Next

[Passport](../../optional-features/passport) covers the fuller OAuth2
authorization server - a different job from Sanctum's simple bearer
tokens, for issuing scoped tokens to third-party clients of your own
API. Back to [Optional Features](../../optional-features/) for the full
list.
