//! Step 1 of the Authorization Code grant: issuing a code once *your own*
//! `/oauth/authorize` route has authenticated the user (via your app's
//! existing session/`larust_auth::Auth<U>`) and they've approved the
//! request - this crate owns none of that UI, see this crate's own doc
//! comment for why.

use crate::client::find_client;
use crate::{ensure_tables, hash_secret, now_unix_secs, random_secret, OAuthError};
use sqlx::AnyPool;

/// RFC 7636 §4.3's own two challenge methods - `Plain` is accepted here
/// only so a malformed/misconfigured client request produces a clear
/// `invalid_request` error naming exactly what's wrong, never so it can
/// actually succeed: [`create_authorization_code`] rejects it outright.
/// See this crate's own top-level doc comment for why S256 is required
/// unconditionally, not just for public clients the way the RFC itself
/// only strictly mandates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PkceMethod {
    S256,
    Plain,
}

impl PkceMethod {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "S256" => Some(PkceMethod::S256),
            "plain" => Some(PkceMethod::Plain),
            _ => None,
        }
    }
}

/// What *your* `/oauth/authorize` handler has already resolved by the
/// time it calls [`create_authorization_code`]: `user_id` from your own
/// session/`Auth<U>`, everything else from the request's own query
/// parameters (`client_id`, `redirect_uri`, `scope` - space-separated,
/// `code_challenge`, `code_challenge_method`).
pub struct AuthorizeRequest {
    pub client_id: String,
    pub redirect_uri: String,
    pub user_id: i64,
    pub scopes: Vec<String>,
    pub code_challenge: String,
    pub code_challenge_method: PkceMethod,
}

/// Short-lived (10 minutes - the same window real Passport's own default
/// uses) and single-use by construction: [`crate::token::exchange_token`]
/// deletes the row the moment it's redeemed, so a replayed code (a second
/// exchange attempt with the same value, whether from a confused client
/// retry or a real attacker who intercepted it) always fails as
/// `invalid_grant`, never succeeds a second time.
const AUTH_CODE_TTL_SECS: i64 = 600;

/// Issues an authorization code, returning its plaintext (only ever
/// available here - only its hash is stored, same guarantee
/// [`crate::client::NewClient`]'s own doc comment already gives client
/// secrets). Your own `/oauth/authorize` handler redirects the user's
/// browser back to `redirect_uri` with this as the `code` query
/// parameter (plus `state`, which this crate never touches at all - it's
/// pure CSRF-defense bookkeeping between your own authorize route and
/// your own callback route, carrying no meaning this crate needs to
/// understand).
pub async fn create_authorization_code(request: AuthorizeRequest) -> Result<String, OAuthError> {
    if request.code_challenge_method != PkceMethod::S256 {
        return Err(OAuthError::InvalidRequest(
            "code_challenge_method must be S256 - \"plain\" is never accepted".to_string(),
        ));
    }
    if request.code_challenge.is_empty() {
        return Err(OAuthError::InvalidRequest(
            "code_challenge is required".to_string(),
        ));
    }

    let pool = larust_orm::pool()?;
    ensure_tables(pool).await?;

    let client = find_client(pool, &request.client_id)
        .await?
        .ok_or_else(|| OAuthError::InvalidClient("unknown client_id".to_string()))?;
    if client.redirect_uri != request.redirect_uri {
        return Err(OAuthError::InvalidRequest(
            "redirect_uri does not match the client's registered redirect URI".to_string(),
        ));
    }

    let code = random_secret();
    insert_code(pool, &code, &request).await?;
    Ok(code)
}

async fn insert_code(
    pool: &AnyPool,
    code: &str,
    request: &AuthorizeRequest,
) -> Result<(), OAuthError> {
    let backend = larust_orm::backend();
    let insert_sql = format!(
        "INSERT INTO oauth_auth_codes (code_hash, client_id, user_id, redirect_uri, scopes, \
         code_challenge, expires_at) VALUES ({}, {}, {}, {}, {}, {}, {})",
        larust_orm::placeholder(backend, 1),
        larust_orm::placeholder(backend, 2),
        larust_orm::placeholder(backend, 3),
        larust_orm::placeholder(backend, 4),
        larust_orm::placeholder(backend, 5),
        larust_orm::placeholder(backend, 6),
        larust_orm::placeholder(backend, 7),
    );
    sqlx::query(&insert_sql)
        .bind(hash_secret(code))
        .bind(&request.client_id)
        .bind(request.user_id)
        .bind(&request.redirect_uri)
        .bind(request.scopes.join(" "))
        .bind(&request.code_challenge)
        .bind(now_unix_secs() + AUTH_CODE_TTL_SECS)
        .execute(pool)
        .await
        .map_err(|source| larust_core::AppError::Internal(Box::new(source)))?;
    Ok(())
}

// DB-touching scenarios for this module live in `lib.rs`'s own
// consolidated test instead of here - see `client.rs`'s own comment on
// why (the process-wide `connect()` constraint, across a multi-file
// crate).
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_method_parse_recognizes_both_rfc_values_and_rejects_anything_else() {
        assert_eq!(PkceMethod::parse("S256"), Some(PkceMethod::S256));
        assert_eq!(PkceMethod::parse("plain"), Some(PkceMethod::Plain));
        assert_eq!(PkceMethod::parse("whatever"), None);
        assert_eq!(PkceMethod::parse(""), None);
    }
}
