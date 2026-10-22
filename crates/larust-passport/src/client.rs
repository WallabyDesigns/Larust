//! OAuth client registration - Laravel's `php artisan passport:client`,
//! as a plain async function instead (see this crate's own doc comment
//! for why there's no bespoke `xr` subcommand).
//!
//! One redirect URI per client, not a list - see this crate's own doc
//! comment for why. `is_confidential` decides which grants a client can
//! ever use: only a confidential client (one with a real secret) can use
//! the Client Credentials grant at all, since that grant *is* "prove who
//! you are with your secret, get a token" - a public client (mobile app,
//! SPA) has no secret to prove with, by design (anything baked into an
//! app binary or shipped to a browser isn't actually secret), and relies
//! entirely on PKCE instead for the Authorization Code grant.

use crate::{ensure_tables, now_unix_secs, random_secret};
use larust_core::AppError;

/// What [`create_client`] hands back - `client_secret` is `Some` only for
/// a confidential client, and only ever available *here*, at creation
/// time: only its hash is stored, so a stolen database dump can't recover
/// a usable secret from it, the same guarantee `larust-sanctum`'s own
/// `create_token` already gives its own plaintext tokens.
#[derive(Debug, Clone)]
pub struct NewClient {
    pub client_id: String,
    pub client_secret: Option<String>,
}

/// Registers a new OAuth client. `redirect_uri` must be an exact match at
/// both authorization-code-issuance time ([`crate::authorize::
/// create_authorization_code`]) and token-exchange time ([`crate::token::
/// exchange_token`]) - RFC 6749's own "redirect URI must match exactly"
/// requirement, not a prefix/pattern match, since a loose match is a real,
/// well-known open-redirect-adjacent attack surface on an authorization
/// server.
pub async fn create_client(
    name: &str,
    redirect_uri: &str,
    confidential: bool,
) -> Result<NewClient, AppError> {
    let pool = larust_orm::pool()?;
    ensure_tables(pool).await?;

    let client_id = random_secret();
    let client_secret = confidential.then(random_secret);
    let client_secret_hash = client_secret
        .as_deref()
        .map(hash_client_secret)
        .transpose()?;

    let backend = larust_orm::backend();
    let insert_sql = format!(
        "INSERT INTO oauth_clients (client_id, client_secret_hash, name, redirect_uri, \
         is_confidential, created_at) VALUES ({}, {}, {}, {}, {}, {})",
        larust_orm::placeholder(backend, 1),
        larust_orm::placeholder(backend, 2),
        larust_orm::placeholder(backend, 3),
        larust_orm::placeholder(backend, 4),
        larust_orm::placeholder(backend, 5),
        larust_orm::placeholder(backend, 6),
    );
    sqlx::query(&insert_sql)
        .bind(&client_id)
        .bind(&client_secret_hash)
        .bind(name)
        .bind(redirect_uri)
        .bind(i64::from(confidential))
        .bind(now_unix_secs())
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    Ok(NewClient {
        client_id,
        client_secret,
    })
}

/// `argon2` (via `larust_auth::hash`), not [`crate::hash_secret`]'s plain
/// SHA-256 - see that function's own doc comment for the full reasoning:
/// a client secret is operator-generated here (not user-chosen), but it's
/// still a long-lived, reusable *credential* an app stores and presents
/// repeatedly, closer in kind to a password than to a single-use,
/// ephemeral authorization code or access token - the same password-grade
/// hash Laravel Passport's own `Hash::make()` already applies to it. A
/// confidential client typically authenticates far less often than a
/// bearer token is checked (service startup, not every API request), so
/// the slower hash costs comparatively little in practice.
fn hash_client_secret(secret: &str) -> Result<String, AppError> {
    larust_auth::hash_password(secret)
}

/// Loaded once per client lookup (authorization, every grant type) -
/// never cached, since a client's own secret/redirect URI can change
/// between requests and this framework has no cross-request cache
/// invalidation story for it (the same "read fresh every time" choice
/// `larust_orm::backend()`'s own callers already make implicitly by
/// never memoizing *their* own reads of app-mutable state).
pub(crate) struct ClientRecord {
    pub client_secret_hash: Option<String>,
    pub redirect_uri: String,
    pub is_confidential: bool,
}

pub(crate) async fn find_client(
    pool: &sqlx::AnyPool,
    client_id: &str,
) -> Result<Option<ClientRecord>, AppError> {
    let select_sql = format!(
        "SELECT client_secret_hash, redirect_uri, is_confidential FROM oauth_clients \
         WHERE client_id = {}",
        larust_orm::placeholder(larust_orm::backend(), 1)
    );
    // `(.., i64)`, not `(.., bool)` - `sqlx`'s `Any` driver has no portable
    // `bool` decode across every backend this crate supports, so the
    // `!= 0` conversion is done by hand; the identical pattern
    // `larust-permissions`'s own `has_role`/`has_permission_to` already
    // establish for their own `SELECT EXISTS(...)` columns.
    let row: Option<(Option<String>, String, i64)> = sqlx::query_as(&select_sql)
        .bind(client_id)
        .fetch_optional(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    Ok(row.map(
        |(client_secret_hash, redirect_uri, is_confidential)| ClientRecord {
            client_secret_hash,
            redirect_uri,
            is_confidential: is_confidential != 0,
        },
    ))
}

// No `#[cfg(test)] mod tests` here - this crate's own `larust_orm::
// connect()` sets a process-wide pool exactly once (the same constraint
// `larust-sanctum`/`larust-permissions`/`larust-notifications`'s own test
// suites document and work around by keeping every DB-touching scenario
// in *one* shared test function, never several that could run
// concurrently and race for that single `connect()` call). This crate
// spans multiple files/modules, so that single shared test lives in
// `lib.rs`'s own test module instead of being split per-file - see its
// doc comment for the full reasoning.
