//! Step 2: the token endpoint. Your own `/oauth/token` route handler
//! parses the incoming `grant_type` (and whichever other form fields that
//! grant needs) into a [`TokenRequest`] and calls [`exchange_token`] -
//! this crate owns none of the HTTP-layer form parsing/routing itself,
//! the same "framework provides the mechanism, the app owns the route"
//! split this crate's own top-level doc comment already establishes.

use crate::client::find_client;
use crate::{
    constant_time_eq, ensure_tables, hash_secret, now_unix_secs, random_secret, OAuthError,
};
use larust_core::AppError;
use sqlx::AnyPool;

/// 1 hour - the same default real Passport's own `Passport::tokensExpireIn`
/// starts at.
const ACCESS_TOKEN_TTL_SECS: i64 = 3600;
/// 30 days - the same default real Passport's own
/// `Passport::refreshTokensExpireIn` starts at.
const REFRESH_TOKEN_TTL_SECS: i64 = 30 * 24 * 3600;

/// The three grant types this crate supports - see this crate's own
/// top-level doc comment for why these three and not more. Built by your
/// own `/oauth/token` handler from the incoming form body's `grant_type`
/// field and whichever other fields that grant needs.
pub enum TokenRequest {
    AuthorizationCode {
        code: String,
        redirect_uri: String,
        client_id: String,
        code_verifier: String,
    },
    ClientCredentials {
        client_id: String,
        client_secret: String,
        scope: Option<String>,
    },
    RefreshToken {
        refresh_token: String,
        client_id: String,
    },
}

/// RFC 6749 §5.1's own success-response shape, `serde::Serialize`d
/// straight out by your own route handler (`axum::Json(response)`).
#[derive(Debug, serde::Serialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: &'static str,
    pub expires_in: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    pub scope: String,
}

pub async fn exchange_token(request: TokenRequest) -> Result<TokenResponse, OAuthError> {
    let pool = larust_orm::pool()?;
    ensure_tables(pool).await?;
    match request {
        TokenRequest::AuthorizationCode {
            code,
            redirect_uri,
            client_id,
            code_verifier,
        } => {
            exchange_authorization_code(pool, &code, &redirect_uri, &client_id, &code_verifier)
                .await
        }
        TokenRequest::ClientCredentials {
            client_id,
            client_secret,
            scope,
        } => exchange_client_credentials(pool, &client_id, &client_secret, scope.as_deref()).await,
        TokenRequest::RefreshToken {
            refresh_token,
            client_id,
        } => exchange_refresh_token(pool, &refresh_token, &client_id).await,
    }
}

async fn exchange_authorization_code(
    pool: &AnyPool,
    code: &str,
    redirect_uri: &str,
    client_id: &str,
    code_verifier: &str,
) -> Result<TokenResponse, OAuthError> {
    let backend = larust_orm::backend();
    let code_hash = hash_secret(code);

    let select_sql = format!(
        "SELECT client_id, user_id, redirect_uri, scopes, code_challenge, expires_at \
         FROM oauth_auth_codes WHERE code_hash = {}",
        larust_orm::placeholder(backend, 1)
    );
    let row: Option<(String, i64, String, String, String, i64)> = sqlx::query_as(&select_sql)
        .bind(&code_hash)
        .fetch_optional(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    let Some((row_client_id, user_id, row_redirect_uri, scopes, code_challenge, expires_at)) = row
    else {
        return Err(OAuthError::InvalidGrant(
            "the authorization code is invalid".to_string(),
        ));
    };

    // Every validation happens *before* the code is ever touched -
    // deliberately: a failed check here (wrong verifier, expired, wrong
    // client/redirect_uri) must leave the code exactly as it was, so a
    // legitimate client that made an honest mistake can still retry with
    // the corrected values within the code's own TTL. Confirmed directly,
    // not assumed: an earlier version of this function deleted the row
    // first and validated after, so a single wrong `code_verifier` attempt
    // - not even a malicious one, just a client bug - silently burned the
    // code for every later attempt, including the correct one.
    if expires_at <= now_unix_secs() {
        return Err(OAuthError::InvalidGrant(
            "the authorization code has expired".to_string(),
        ));
    }
    if row_client_id != client_id {
        return Err(OAuthError::InvalidGrant(
            "the authorization code was not issued to this client".to_string(),
        ));
    }
    if row_redirect_uri != redirect_uri {
        return Err(OAuthError::InvalidGrant(
            "redirect_uri does not match the one used to request this code".to_string(),
        ));
    }
    if !verify_pkce(code_verifier, &code_challenge) {
        return Err(OAuthError::InvalidGrant(
            "code_verifier does not match the original code_challenge".to_string(),
        ));
    }

    // Only *now*, once every check above has already passed, is the code
    // actually claimed - the identical race-safety shape `larust_queue::
    // sql_worker::claim_next`'s own doc comment already establishes, for
    // the same reason: two *concurrent, both otherwise-valid* exchange
    // attempts with this exact code (a confused client retry, or a real
    // attacker who intercepted it mid-flight) must never both succeed.
    // Deleting unconditionally on `code_hash` and checking
    // `rows_affected()` is what actually closes that race - whichever
    // request's `DELETE` affects the row first is the only one that gets
    // to proceed to issuing tokens; the other sees `0` affected rows and
    // fails as `invalid_grant`, same as if the code had simply never
    // existed - never reachable by a plain validation failure anymore.
    let delete_sql = format!(
        "DELETE FROM oauth_auth_codes WHERE code_hash = {}",
        larust_orm::placeholder(backend, 1)
    );
    let deleted = sqlx::query(&delete_sql)
        .bind(&code_hash)
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    if deleted.rows_affected() != 1 {
        return Err(OAuthError::InvalidGrant(
            "the authorization code has already been used".to_string(),
        ));
    }

    issue_tokens(pool, &row_client_id, Some(user_id), &scopes, true).await
}

async fn exchange_client_credentials(
    pool: &AnyPool,
    client_id: &str,
    client_secret: &str,
    scope: Option<&str>,
) -> Result<TokenResponse, OAuthError> {
    let client = find_client(pool, client_id)
        .await?
        .ok_or_else(|| OAuthError::InvalidClient("unknown client_id".to_string()))?;
    if !client.is_confidential {
        return Err(OAuthError::UnauthorizedClient(
            "this client has no secret and cannot use the client_credentials grant".to_string(),
        ));
    }
    let secret_hash = client
        .client_secret_hash
        .as_deref()
        .expect("a confidential client always has a secret hash");
    let verified = larust_auth::verify_password(secret_hash, client_secret)?;
    if !verified {
        return Err(OAuthError::InvalidClient(
            "client_secret is incorrect".to_string(),
        ));
    }

    // No refresh token for this grant - RFC 6749 §4.4.3's own guidance:
    // the client already holds long-lived credentials (its own secret)
    // and can simply request a fresh access token the same way again at
    // any time, so a refresh token would only be a second, redundant
    // long-lived secret to protect.
    issue_tokens(pool, client_id, None, scope.unwrap_or(""), false).await
}

/// `(refresh.id, refresh.access_token_id, refresh.revoked,
/// refresh.expires_at, access.client_id, access.user_id, access.scopes)` -
/// named purely so `exchange_refresh_token`'s own `query_as` call reads
/// as what it actually fetches (a refresh token row joined against the
/// access token it belongs to) instead of a seven-element tuple clippy's
/// own `type_complexity` lint already flags as too easy to miscount.
type RefreshTokenRow = (i64, i64, i64, i64, String, Option<i64>, String);

async fn exchange_refresh_token(
    pool: &AnyPool,
    refresh_token: &str,
    client_id: &str,
) -> Result<TokenResponse, OAuthError> {
    let backend = larust_orm::backend();
    let token_hash = hash_secret(refresh_token);

    let select_sql = format!(
        "SELECT r.id, r.access_token_id, r.revoked, r.expires_at, \
                a.client_id, a.user_id, a.scopes \
         FROM oauth_refresh_tokens r \
         JOIN oauth_access_tokens a ON a.id = r.access_token_id \
         WHERE r.token_hash = {}",
        larust_orm::placeholder(backend, 1)
    );
    let row: Option<RefreshTokenRow> = sqlx::query_as(&select_sql)
        .bind(&token_hash)
        .fetch_optional(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    let Some((refresh_id, access_token_id, revoked, expires_at, row_client_id, user_id, scopes)) =
        row
    else {
        return Err(OAuthError::InvalidGrant(
            "the refresh token is invalid".to_string(),
        ));
    };

    if revoked != 0 {
        return Err(OAuthError::InvalidGrant(
            "the refresh token has already been used or revoked".to_string(),
        ));
    }
    if expires_at <= now_unix_secs() {
        return Err(OAuthError::InvalidGrant(
            "the refresh token has expired".to_string(),
        ));
    }
    if row_client_id != client_id {
        return Err(OAuthError::InvalidGrant(
            "the refresh token was not issued to this client".to_string(),
        ));
    }

    // Rotation: the old refresh token *and* the access token it belongs
    // to are both revoked together, the moment a new pair is issued -
    // never left alongside the new one. See this module's own doc
    // comment for why: leaving the old access token valid would mean two
    // simultaneously-valid tokens existing indefinitely after every
    // refresh, and leaving the old refresh token valid would make a
    // single intercepted refresh token reusable forever instead of
    // single-use, defeating the entire point of rotation (a stolen
    // refresh token's theft becomes detectable - the legitimate client's
    // own next refresh attempt fails loudly instead of both parties
    // silently continuing to mint tokens from the same stolen secret).
    revoke_refresh_row(pool, refresh_id).await?;
    revoke_access_row(pool, access_token_id).await?;

    issue_tokens(pool, &row_client_id, user_id, &scopes, true).await
}

/// Shared by every grant above - inserts a fresh access token row (and,
/// when `with_refresh` is true, a fresh refresh token bound to it),
/// returning the plaintext pair. `scopes` is already the final,
/// space-separated string to store/return - callers decide what belongs
/// in it (an auth code's own stored scopes, or whatever `scope` a
/// client-credentials request asked for).
async fn issue_tokens(
    pool: &AnyPool,
    client_id: &str,
    user_id: Option<i64>,
    scopes: &str,
    with_refresh: bool,
) -> Result<TokenResponse, OAuthError> {
    let backend = larust_orm::backend();
    let access_token = random_secret();
    let access_expires_at = now_unix_secs() + ACCESS_TOKEN_TTL_SECS;

    let insert_access_sql = format!(
        "INSERT INTO oauth_access_tokens (token_hash, client_id, user_id, scopes, revoked, \
         expires_at) VALUES ({}, {}, {}, {}, 0, {})",
        larust_orm::placeholder(backend, 1),
        larust_orm::placeholder(backend, 2),
        larust_orm::placeholder(backend, 3),
        larust_orm::placeholder(backend, 4),
        larust_orm::placeholder(backend, 5),
    );
    sqlx::query(&insert_access_sql)
        .bind(hash_secret(&access_token))
        .bind(client_id)
        .bind(user_id)
        .bind(scopes)
        .bind(access_expires_at)
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    // Portable "what id did that INSERT just create" - MySQL has no
    // `RETURNING` clause (the identical constraint `release_slots.rs`'s
    // own doc comment already documents working around), so the freshly
    // inserted row is looked up again by its own unique `token_hash`
    // instead of trusting a driver-specific last-insert-id call.
    let access_token_id = find_access_token_id(pool, &access_token).await?;

    let refresh_token = if with_refresh {
        let refresh_token = random_secret();
        let insert_refresh_sql = format!(
            "INSERT INTO oauth_refresh_tokens (token_hash, access_token_id, revoked, \
             expires_at) VALUES ({}, {}, 0, {})",
            larust_orm::placeholder(backend, 1),
            larust_orm::placeholder(backend, 2),
            larust_orm::placeholder(backend, 3),
        );
        sqlx::query(&insert_refresh_sql)
            .bind(hash_secret(&refresh_token))
            .bind(access_token_id)
            .bind(now_unix_secs() + REFRESH_TOKEN_TTL_SECS)
            .execute(pool)
            .await
            .map_err(|source| AppError::Internal(Box::new(source)))?;
        Some(refresh_token)
    } else {
        None
    };

    Ok(TokenResponse {
        access_token,
        token_type: "Bearer",
        expires_in: ACCESS_TOKEN_TTL_SECS,
        refresh_token,
        scope: scopes.to_string(),
    })
}

async fn find_access_token_id(pool: &AnyPool, plaintext_token: &str) -> Result<i64, AppError> {
    let select_sql = format!(
        "SELECT id FROM oauth_access_tokens WHERE token_hash = {}",
        larust_orm::placeholder(larust_orm::backend(), 1)
    );
    let (id,): (i64,) = sqlx::query_as(&select_sql)
        .bind(hash_secret(plaintext_token))
        .fetch_one(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    Ok(id)
}

/// RFC 7636 §4.6's own verification: `BASE64URL-ENCODE(SHA256(ASCII(
/// code_verifier)))` must equal the `code_challenge` supplied at
/// authorization time. Compared in constant time - see
/// [`crate::constant_time_eq`]'s own doc comment for why.
fn verify_pkce(code_verifier: &str, code_challenge: &str) -> bool {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(code_verifier.as_bytes());
    let computed = URL_SAFE_NO_PAD.encode(digest);
    constant_time_eq(&computed, code_challenge)
}

async fn revoke_access_row(pool: &AnyPool, id: i64) -> Result<(), AppError> {
    let sql = format!(
        "UPDATE oauth_access_tokens SET revoked = 1 WHERE id = {}",
        larust_orm::placeholder(larust_orm::backend(), 1)
    );
    sqlx::query(&sql)
        .bind(id)
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    Ok(())
}

async fn revoke_refresh_row(pool: &AnyPool, id: i64) -> Result<(), AppError> {
    let sql = format!(
        "UPDATE oauth_refresh_tokens SET revoked = 1 WHERE id = {}",
        larust_orm::placeholder(larust_orm::backend(), 1)
    );
    sqlx::query(&sql)
        .bind(id)
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    Ok(())
}

/// Revokes an access token by its own plaintext value - RFC 7009's
/// `/oauth/revoke` endpoint, wired up by your own route the same way
/// every other endpoint in this crate is. Also revokes the refresh token
/// it was issued alongside, if any - revoking only the access token and
/// leaving its own refresh token valid would let the client simply mint
/// a replacement access token right back, defeating the point of
/// revoking anything at all. Not an error to revoke a token that's
/// already revoked, expired, or never existed - RFC 7009 §2.2 itself
/// requires a revocation endpoint to respond the same way (`200 OK`)
/// regardless, specifically so a client can't use the response to probe
/// which tokens are real.
pub async fn revoke_access_token(token: &str) -> Result<(), AppError> {
    let pool = larust_orm::pool()?;
    ensure_tables(pool).await?;
    let token_hash = hash_secret(token);

    let select_sql = format!(
        "SELECT id FROM oauth_access_tokens WHERE token_hash = {}",
        larust_orm::placeholder(larust_orm::backend(), 1)
    );
    let row: Option<(i64,)> = sqlx::query_as(&select_sql)
        .bind(&token_hash)
        .fetch_optional(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    let Some((access_token_id,)) = row else {
        return Ok(());
    };

    revoke_access_row(pool, access_token_id).await?;

    let revoke_refresh_sql = format!(
        "UPDATE oauth_refresh_tokens SET revoked = 1 WHERE access_token_id = {}",
        larust_orm::placeholder(larust_orm::backend(), 1)
    );
    sqlx::query(&revoke_refresh_sql)
        .bind(access_token_id)
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    Ok(())
}

/// Revokes a refresh token by its own plaintext value, without touching
/// the access token it was issued alongside (which may still have real
/// remaining life and in-flight use) - the narrower sibling to
/// [`revoke_access_token`], for a client that specifically wants to stop
/// being able to mint *new* access tokens without forcing its current
/// one to stop working immediately. Same "not an error either way" RFC
/// 7009 tolerance.
pub async fn revoke_refresh_token(token: &str) -> Result<(), AppError> {
    let pool = larust_orm::pool()?;
    ensure_tables(pool).await?;
    let sql = format!(
        "UPDATE oauth_refresh_tokens SET revoked = 1 WHERE token_hash = {}",
        larust_orm::placeholder(larust_orm::backend(), 1)
    );
    sqlx::query(&sql)
        .bind(hash_secret(token))
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    Ok(())
}

// The DB-touching scenarios for every grant type live in `lib.rs`'s own
// consolidated test, not here - see `client.rs`'s own comment for why
// (the process-wide `connect()` constraint, across a multi-file crate).
