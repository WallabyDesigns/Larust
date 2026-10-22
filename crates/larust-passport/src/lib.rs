//! Laravel's `laravel/passport` - a full OAuth2 authorization server (RFC
//! 6749), narrowed to the three grant types nearly every real integration
//! actually needs: Authorization Code with PKCE (RFC 7636, required -
//! "plain" challenge method is rejected outright, not just discouraged -
//! for every client, not only public ones, since there is no real
//! downside to requiring it and a real upside: a leaked/misconfigured
//! client secret stops being single-handedly sufficient to redeem a
//! stolen authorization code), Client Credentials (machine-to-machine,
//! confidential clients only), and Refresh Token (with rotation - see
//! [`token`]'s own doc comment for why the old refresh token is always
//! invalidated the moment a new one is issued, not left valid alongside
//! it).
//!
//! **Narrower than real Passport in three specific ways, all deliberate:**
//! - **One redirect URI per client**, not Passport's comma-separated
//!   list. A real client registering multiple callback URLs (web +
//!   mobile, say) registers multiple [`client::create_client`] rows
//!   instead - more rows, not a parsing/validation surface for a
//!   redirect-URI list this crate would otherwise own.
//! - **No `php artisan passport:client`-equivalent interactive CLI.**
//!   [`client::create_client`] is a plain async function - call it from
//!   your own seeder, console command, or `xr tinker`-equivalent script.
//!   Every other optional Tier-1 shim crate in this workspace
//!   (`larust-sanctum`/`larust-socialite`/...) is a pure library with no
//!   bespoke `xr` subcommand of its own; this one follows the same
//!   precedent rather than being the first exception.
//! - **No scopes UI/consent-screen rendering.** [`authorize::
//!   create_authorization_code`] is called by *your own* `/oauth/authorize`
//!   route handler once *you've* shown the user an approval screen and
//!   they've approved - the same "framework provides the mechanism, the
//!   app owns the actual route and its UI" split `larust-sanctum`/
//!   `larust-socialite` already establish, not a `laravel/passport`-style
//!   auto-registered route+Blade view this crate would otherwise have to
//!   own and let every app style on its own.
//!
//! See each submodule's own doc comment for the full design of its own
//! piece: [`client`] (registration), [`authorize`] (step 1 - issuing a
//! code), [`token`] (step 2 - exchanging a code/refreshing/client-
//! credentials), [`extractors`] (request-time bearer-token validation).
//!
//! Re-exported through `larust_support::passport` (see `crates/
//! larust-support/src/lib.rs`) so generated apps depend only on
//! `larust-support`, never on this crate directly.

pub mod authorize;
pub mod client;
pub mod extractors;
pub mod token;

pub use authorize::{create_authorization_code, AuthorizeRequest, PkceMethod};
pub use client::{create_client, NewClient};
pub use extractors::{has_scope, require_scope, OAuthAuth, OAuthClientAuth};
pub use token::{
    exchange_token, revoke_access_token, revoke_refresh_token, TokenRequest, TokenResponse,
};

use larust_core::AppError;
use larust_orm::Backend;
use sqlx::AnyPool;
use std::time::{SystemTime, UNIX_EPOCH};

/// Every table this crate owns, created together - same lazy, `IF NOT
/// EXISTS`-based self-bootstrap idiom `larust-sanctum`/
/// `larust-notifications`'s own `ensure_table` establishes (see either's
/// doc comment for why this is deliberately **not** memoized behind a
/// `OnceCell`: a process-wide completion flag could point at a since-
/// discarded test database). Every public entry point in this crate calls
/// this first, the same way every one of `larust-sanctum`'s own public
/// functions calls its own `ensure_table` first.
pub(crate) async fn ensure_tables(pool: &AnyPool) -> Result<(), AppError> {
    let statements: &[&str] = match larust_orm::backend() {
        Backend::Sqlite => &[
            "CREATE TABLE IF NOT EXISTS oauth_clients (\
                id INTEGER PRIMARY KEY AUTOINCREMENT, \
                client_id TEXT NOT NULL UNIQUE, \
                client_secret_hash TEXT, \
                name TEXT NOT NULL, \
                redirect_uri TEXT NOT NULL, \
                is_confidential INTEGER NOT NULL, \
                created_at INTEGER NOT NULL\
             )",
            "CREATE TABLE IF NOT EXISTS oauth_auth_codes (\
                id INTEGER PRIMARY KEY AUTOINCREMENT, \
                code_hash TEXT NOT NULL UNIQUE, \
                client_id TEXT NOT NULL, \
                user_id INTEGER NOT NULL, \
                redirect_uri TEXT NOT NULL, \
                scopes TEXT NOT NULL, \
                code_challenge TEXT NOT NULL, \
                expires_at INTEGER NOT NULL\
             )",
            "CREATE TABLE IF NOT EXISTS oauth_access_tokens (\
                id INTEGER PRIMARY KEY AUTOINCREMENT, \
                token_hash TEXT NOT NULL UNIQUE, \
                client_id TEXT NOT NULL, \
                user_id INTEGER, \
                scopes TEXT NOT NULL, \
                revoked INTEGER NOT NULL DEFAULT 0, \
                expires_at INTEGER NOT NULL\
             )",
            "CREATE TABLE IF NOT EXISTS oauth_refresh_tokens (\
                id INTEGER PRIMARY KEY AUTOINCREMENT, \
                token_hash TEXT NOT NULL UNIQUE, \
                access_token_id INTEGER NOT NULL, \
                revoked INTEGER NOT NULL DEFAULT 0, \
                expires_at INTEGER NOT NULL\
             )",
        ],
        // `VARCHAR`, not MySQL's own `TEXT` - the identical `Any`-driver
        // `Text`-kind-decoding lesson `larust-cache`/`larust-sanctum`'s own
        // `ensure_table` doc comments already document in detail (only
        // `CHAR`/`VARCHAR` decode back as `String` through `sqlx`'s `Any`
        // driver). `redirect_uri`/`scopes` get a generous cap (a scope
        // list can legitimately be long) rather than an arbitrary-size one.
        Backend::MySql => &[
            "CREATE TABLE IF NOT EXISTS oauth_clients (\
                id INTEGER PRIMARY KEY AUTO_INCREMENT, \
                client_id VARCHAR(255) NOT NULL UNIQUE, \
                client_secret_hash VARCHAR(255), \
                name VARCHAR(255) NOT NULL, \
                redirect_uri VARCHAR(2048) NOT NULL, \
                is_confidential INTEGER NOT NULL, \
                created_at INTEGER NOT NULL\
             )",
            "CREATE TABLE IF NOT EXISTS oauth_auth_codes (\
                id INTEGER PRIMARY KEY AUTO_INCREMENT, \
                code_hash VARCHAR(255) NOT NULL UNIQUE, \
                client_id VARCHAR(255) NOT NULL, \
                user_id INTEGER NOT NULL, \
                redirect_uri VARCHAR(2048) NOT NULL, \
                scopes VARCHAR(2000) NOT NULL, \
                code_challenge VARCHAR(255) NOT NULL, \
                expires_at INTEGER NOT NULL\
             )",
            "CREATE TABLE IF NOT EXISTS oauth_access_tokens (\
                id INTEGER PRIMARY KEY AUTO_INCREMENT, \
                token_hash VARCHAR(255) NOT NULL UNIQUE, \
                client_id VARCHAR(255) NOT NULL, \
                user_id INTEGER, \
                scopes VARCHAR(2000) NOT NULL, \
                revoked INTEGER NOT NULL DEFAULT 0, \
                expires_at INTEGER NOT NULL\
             )",
            "CREATE TABLE IF NOT EXISTS oauth_refresh_tokens (\
                id INTEGER PRIMARY KEY AUTO_INCREMENT, \
                token_hash VARCHAR(255) NOT NULL UNIQUE, \
                access_token_id INTEGER NOT NULL, \
                revoked INTEGER NOT NULL DEFAULT 0, \
                expires_at INTEGER NOT NULL\
             )",
        ],
        // Postgres has native, unbounded `TEXT` - same shape as SQLite's
        // own arm, just `GENERATED ALWAYS AS IDENTITY` for the primary key
        // (the identical convention `larust-sanctum`'s own Postgres arm
        // already uses).
        Backend::Postgres => &[
            "CREATE TABLE IF NOT EXISTS oauth_clients (\
                id INTEGER GENERATED ALWAYS AS IDENTITY PRIMARY KEY, \
                client_id TEXT NOT NULL UNIQUE, \
                client_secret_hash TEXT, \
                name TEXT NOT NULL, \
                redirect_uri TEXT NOT NULL, \
                is_confidential INTEGER NOT NULL, \
                created_at INTEGER NOT NULL\
             )",
            "CREATE TABLE IF NOT EXISTS oauth_auth_codes (\
                id INTEGER GENERATED ALWAYS AS IDENTITY PRIMARY KEY, \
                code_hash TEXT NOT NULL UNIQUE, \
                client_id TEXT NOT NULL, \
                user_id INTEGER NOT NULL, \
                redirect_uri TEXT NOT NULL, \
                scopes TEXT NOT NULL, \
                code_challenge TEXT NOT NULL, \
                expires_at INTEGER NOT NULL\
             )",
            "CREATE TABLE IF NOT EXISTS oauth_access_tokens (\
                id INTEGER GENERATED ALWAYS AS IDENTITY PRIMARY KEY, \
                token_hash TEXT NOT NULL UNIQUE, \
                client_id TEXT NOT NULL, \
                user_id INTEGER, \
                scopes TEXT NOT NULL, \
                revoked INTEGER NOT NULL DEFAULT 0, \
                expires_at INTEGER NOT NULL\
             )",
            "CREATE TABLE IF NOT EXISTS oauth_refresh_tokens (\
                id INTEGER GENERATED ALWAYS AS IDENTITY PRIMARY KEY, \
                token_hash TEXT NOT NULL UNIQUE, \
                access_token_id INTEGER NOT NULL, \
                revoked INTEGER NOT NULL DEFAULT 0, \
                expires_at INTEGER NOT NULL\
             )",
        ],
    };
    for statement in statements {
        sqlx::query(statement)
            .execute(pool)
            .await
            .map_err(|source| AppError::Internal(Box::new(source)))?;
    }
    Ok(())
}

pub(crate) fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_secs() as i64
}

/// Hashes a high-entropy random secret (an authorization code, access
/// token, or refresh token plaintext) for storage - plain SHA-256, not
/// `argon2` (see [`client::hash_client_secret`]'s own doc comment for why
/// client secrets specifically get the slower, password-grade hash
/// instead). The identical trade-off `larust-sanctum`'s own `hash_token`
/// already makes for its bearer tokens: a brute-force attacker already
/// needs to search the same astronomically large keyspace a slow hash
/// would also force them through, so the only thing a slow hash would add
/// here is making every legitimate request slower for no real benefit -
/// unlike a human-chosen password, which actually lives in a small enough
/// keyspace that slow hashing meaningfully raises the cost of a dictionary
/// attack.
pub(crate) fn hash_secret(secret: &str) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    let digest = Sha256::digest(secret.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Constant-time comparison for two hex-digest strings - avoids leaking a
/// hash's own matching prefix length through a short-circuiting `==`
/// timing side channel. Mirrors `larust-sanctum`'s own `hashes_match`
/// (not imported from it - a cross-dependency just for one ten-line
/// helper would be a real coupling, not a convenience, the same
/// reasoning `larust-cache::redis_store`'s own tiny local `env` helper
/// doc comment already gives for an identical choice).
pub(crate) fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Generates a high-entropy random secret, hex-encoded - the identical
/// shape `larust-sanctum`'s own token generator already establishes (not
/// imported, for the same "ten-line helper, not worth a cross-dependency"
/// reasoning [`constant_time_eq`]'s own doc comment gives). 32 bytes (256
/// bits) throughout this crate - generous for every secret it issues
/// (authorization codes, access tokens, refresh tokens, client secrets),
/// never a tunable, since there is no real scenario that needs less.
pub(crate) fn random_secret() -> String {
    use rand::Rng;
    use std::fmt::Write as _;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill(&mut bytes[..]);
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// RFC 6749 §5.2's own error vocabulary - every variant renders as `400
/// Bad Request` with the exact `{"error": "...", "error_description":
/// "..."}` JSON shape a real OAuth2 client parses programmatically (`axum`
/// route handlers calling into this crate return `Result<_, OAuthError>`
/// directly; its own [`axum::response::IntoResponse`] impl handles the
/// rest). `invalid_client` is RFC-permitted at `400` when client
/// authentication arrives in the request body (every grant this crate
/// supports does) rather than the `Authorization` header, which is the
/// only context this crate ever uses it in - `401` is correct only for
/// the header-based case, not attempted here.
///
/// `Internal` is the one variant that isn't part of RFC 6749's own
/// vocabulary at all - a genuine unexpected failure (a database error)
/// gets this framework's own ordinary [`AppError`] handling (the debug
/// page in development, a generic 500 in production), not a misleading
/// `invalid_request` dressed up as if the *caller* did something wrong.
#[derive(Debug)]
pub enum OAuthError {
    InvalidRequest(String),
    InvalidClient(String),
    InvalidGrant(String),
    UnauthorizedClient(String),
    UnsupportedGrantType(String),
    InvalidScope(String),
    Internal(AppError),
}

impl OAuthError {
    fn code(&self) -> &'static str {
        match self {
            OAuthError::InvalidRequest(_) => "invalid_request",
            OAuthError::InvalidClient(_) => "invalid_client",
            OAuthError::InvalidGrant(_) => "invalid_grant",
            OAuthError::UnauthorizedClient(_) => "unauthorized_client",
            OAuthError::UnsupportedGrantType(_) => "unsupported_grant_type",
            OAuthError::InvalidScope(_) => "invalid_scope",
            OAuthError::Internal(_) => "server_error",
        }
    }

    fn description(&self) -> &str {
        match self {
            OAuthError::InvalidRequest(message)
            | OAuthError::InvalidClient(message)
            | OAuthError::InvalidGrant(message)
            | OAuthError::UnauthorizedClient(message)
            | OAuthError::UnsupportedGrantType(message)
            | OAuthError::InvalidScope(message) => message,
            OAuthError::Internal(_) => "an internal error occurred",
        }
    }
}

impl From<AppError> for OAuthError {
    fn from(error: AppError) -> Self {
        OAuthError::Internal(error)
    }
}

impl axum::response::IntoResponse for OAuthError {
    fn into_response(self) -> axum::response::Response {
        // A real unexpected failure gets this framework's own ordinary
        // error handling (the debug page in development, logging, a
        // generic 500 in production) - see this type's own doc comment
        // for why `Internal` is never rendered as a fake `invalid_*` body.
        if let OAuthError::Internal(error) = self {
            return error.into_response();
        }
        let status = axum::http::StatusCode::BAD_REQUEST;
        let body = serde_json::json!({
            "error": self.code(),
            "error_description": self.description(),
        });
        (status, axum::Json(body)).into_response()
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    pub async fn connect_test_db() {
        let dir = tempfile::tempdir().unwrap().keep();
        let database_url = format!("sqlite://{}/test.sqlite", dir.display());
        larust_orm::connect(&database_url).await.unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorize::{create_authorization_code, AuthorizeRequest, PkceMethod};
    use crate::client::{create_client, find_client};
    use crate::extractors::test_fixtures::{get_with_auth, router};
    use crate::token::{exchange_token, revoke_access_token, TokenRequest};
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use sha2::{Digest, Sha256};

    /// Every DB-touching scenario across this entire crate, in one
    /// function - `larust_orm::connect()` sets a process-wide pool
    /// exactly once, and this crate spans multiple files/modules, each
    /// of which originally had its own `#[tokio::test]` scenario calling
    /// it independently. Confirmed directly, not assumed: `cargo test`'s
    /// default parallelism ran all four concurrently in the same test
    /// binary, and every one but whichever happened to win the race
    /// failed with `"connect() called more than once"`. `larust-sanctum`/
    /// `larust-permissions`/`larust-notifications` each avoid this
    /// naturally, since their own DB-touching scenarios already live in
    /// one file/one function each - this crate just has that same
    /// one-function's-worth of scenarios spread across more source files
    /// for readability, so the single shared test has to live somewhere
    /// that can see all of them: here.
    #[tokio::test]
    async fn passport_crate_behaves_correctly_across_every_scenario() {
        test_support::connect_test_db().await;

        // --- Client registration ---
        let confidential = create_client("Server App", "https://app.test/callback", true)
            .await
            .unwrap();
        assert_eq!(confidential.client_id.len(), 64);
        let secret = confidential
            .client_secret
            .clone()
            .expect("confidential client must get a secret");
        assert_eq!(secret.len(), 64);

        let public = create_client("Mobile App", "myapp://callback", false)
            .await
            .unwrap();
        assert!(public.client_secret.is_none());
        assert_ne!(confidential.client_id, public.client_id);

        let pool = larust_orm::pool().unwrap();
        let found = find_client(pool, &confidential.client_id)
            .await
            .unwrap()
            .expect("just-created confidential client should be found");
        assert_eq!(found.redirect_uri, "https://app.test/callback");
        assert!(found.is_confidential);
        assert!(found.client_secret_hash.is_some());

        let found_public = find_client(pool, &public.client_id)
            .await
            .unwrap()
            .expect("just-created public client should be found");
        assert!(!found_public.is_confidential);
        assert!(found_public.client_secret_hash.is_none());
        assert!(find_client(pool, "does-not-exist").await.unwrap().is_none());

        // --- Authorization code issuance ---
        let redirect_uri = "https://app.test/callback";
        fn authorize_request(client_id: &str, redirect_uri: &str) -> AuthorizeRequest {
            AuthorizeRequest {
                client_id: client_id.to_string(),
                redirect_uri: redirect_uri.to_string(),
                user_id: 1,
                scopes: vec!["read-posts".to_string()],
                code_challenge: "a-real-challenge-value".to_string(),
                code_challenge_method: PkceMethod::S256,
            }
        }

        let throwaway_code =
            create_authorization_code(authorize_request(&confidential.client_id, redirect_uri))
                .await
                .unwrap();
        assert_eq!(throwaway_code.len(), 64);

        let error = create_authorization_code(authorize_request("not-a-real-client", redirect_uri))
            .await
            .unwrap_err();
        assert!(matches!(error, OAuthError::InvalidClient(_)));

        let error = create_authorization_code(authorize_request(
            &confidential.client_id,
            "https://evil.test/callback",
        ))
        .await
        .unwrap_err();
        assert!(matches!(error, OAuthError::InvalidRequest(_)));

        let mut plain_request = authorize_request(&confidential.client_id, redirect_uri);
        plain_request.code_challenge_method = PkceMethod::Plain;
        let error = create_authorization_code(plain_request).await.unwrap_err();
        assert!(matches!(error, OAuthError::InvalidRequest(_)));

        let mut empty_challenge = authorize_request(&confidential.client_id, redirect_uri);
        empty_challenge.code_challenge = String::new();
        let error = create_authorization_code(empty_challenge)
            .await
            .unwrap_err();
        assert!(matches!(error, OAuthError::InvalidRequest(_)));

        // --- Token exchange: authorization_code grant + PKCE ---
        fn pkce_pair() -> (String, String) {
            let verifier = random_secret();
            let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
            (verifier, challenge)
        }
        let (verifier, challenge) = pkce_pair();
        let code = create_authorization_code(AuthorizeRequest {
            client_id: confidential.client_id.clone(),
            redirect_uri: redirect_uri.to_string(),
            user_id: 42,
            scopes: vec!["read-posts".to_string(), "write-posts".to_string()],
            code_challenge: challenge,
            code_challenge_method: PkceMethod::S256,
        })
        .await
        .unwrap();

        let wrong_verifier_result = exchange_token(TokenRequest::AuthorizationCode {
            code: code.clone(),
            redirect_uri: redirect_uri.to_string(),
            client_id: confidential.client_id.clone(),
            code_verifier: "not-the-real-verifier".to_string(),
        })
        .await;
        assert!(matches!(
            wrong_verifier_result,
            Err(OAuthError::InvalidGrant(_))
        ));

        let response = exchange_token(TokenRequest::AuthorizationCode {
            code: code.clone(),
            redirect_uri: redirect_uri.to_string(),
            client_id: confidential.client_id.clone(),
            code_verifier: verifier.clone(),
        })
        .await
        .unwrap();
        assert_eq!(response.token_type, "Bearer");
        assert_eq!(response.scope, "read-posts write-posts");
        assert!(response.refresh_token.is_some());

        let replay_result = exchange_token(TokenRequest::AuthorizationCode {
            code,
            redirect_uri: redirect_uri.to_string(),
            client_id: confidential.client_id.clone(),
            code_verifier: verifier,
        })
        .await;
        assert!(matches!(replay_result, Err(OAuthError::InvalidGrant(_))));

        // --- Token exchange: client_credentials grant ---
        let cc_response = exchange_token(TokenRequest::ClientCredentials {
            client_id: confidential.client_id.clone(),
            client_secret: secret.clone(),
            scope: Some("admin".to_string()),
        })
        .await
        .unwrap();
        assert!(cc_response.refresh_token.is_none());
        assert_eq!(cc_response.scope, "admin");

        let wrong_secret_result = exchange_token(TokenRequest::ClientCredentials {
            client_id: confidential.client_id.clone(),
            client_secret: "not-the-real-secret".to_string(),
            scope: None,
        })
        .await;
        assert!(matches!(
            wrong_secret_result,
            Err(OAuthError::InvalidClient(_))
        ));

        let public_cc_result = exchange_token(TokenRequest::ClientCredentials {
            client_id: public.client_id.clone(),
            client_secret: "anything".to_string(),
            scope: None,
        })
        .await;
        assert!(matches!(
            public_cc_result,
            Err(OAuthError::UnauthorizedClient(_))
        ));

        // --- Token exchange: refresh_token grant, with rotation ---
        let refreshed = exchange_token(TokenRequest::RefreshToken {
            refresh_token: response.refresh_token.clone().unwrap(),
            client_id: confidential.client_id.clone(),
        })
        .await
        .unwrap();
        assert_ne!(refreshed.access_token, response.access_token);
        assert_ne!(refreshed.refresh_token, response.refresh_token);
        assert_eq!(refreshed.scope, "read-posts write-posts");

        let reuse_old_refresh_result = exchange_token(TokenRequest::RefreshToken {
            refresh_token: response.refresh_token.clone().unwrap(),
            client_id: confidential.client_id.clone(),
        })
        .await;
        assert!(matches!(
            reuse_old_refresh_result,
            Err(OAuthError::InvalidGrant(_))
        ));

        // --- Revocation ---
        revoke_access_token(&refreshed.access_token).await.unwrap();
        let revoked_refresh_result = exchange_token(TokenRequest::RefreshToken {
            refresh_token: refreshed.refresh_token.clone().unwrap(),
            client_id: confidential.client_id.clone(),
        })
        .await;
        assert!(matches!(
            revoked_refresh_result,
            Err(OAuthError::InvalidGrant(_))
        ));

        // --- Request-time extractors ---
        // A freshly-minted token, independent of `response`/`refreshed`
        // above (both already revoked, deliberately, by the rotation/
        // revocation steps this same test just exercised) - the
        // extractor checks below need a token whose own lifecycle this
        // section fully controls.
        let (fresh_verifier, fresh_challenge) = pkce_pair();
        let fresh_code = create_authorization_code(AuthorizeRequest {
            client_id: confidential.client_id.clone(),
            redirect_uri: redirect_uri.to_string(),
            user_id: 99,
            scopes: vec!["read-posts".to_string()],
            code_challenge: fresh_challenge,
            code_challenge_method: PkceMethod::S256,
        })
        .await
        .unwrap();
        let extractor_tokens = exchange_token(TokenRequest::AuthorizationCode {
            code: fresh_code,
            redirect_uri: redirect_uri.to_string(),
            client_id: confidential.client_id.clone(),
            code_verifier: fresh_verifier,
        })
        .await
        .unwrap();

        let api_router = router();
        let (status, _) = get_with_auth(&api_router, "/me", None).await;
        assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);
        let (status, _) = get_with_auth(&api_router, "/me", Some("not-bearer-shaped")).await;
        assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);
        let (status, _) = get_with_auth(&api_router, "/me", Some("Bearer not-a-real-token")).await;
        assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);

        let (status, body) = get_with_auth(
            &api_router,
            "/me",
            Some(&format!("Bearer {}", extractor_tokens.access_token)),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(body, "99:read-posts");

        // A client_credentials token has no bound user - rejected by
        // `OAuthAuth<U>` specifically, accepted fine by `OAuthClientAuth`.
        let (status, _) = get_with_auth(
            &api_router,
            "/me",
            Some(&format!("Bearer {}", cc_response.access_token)),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);

        let (status, body) = get_with_auth(
            &api_router,
            "/client",
            Some(&format!("Bearer {}", cc_response.access_token)),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert!(body.contains(&confidential.client_id));
        assert!(body.contains("\"has_admin\":true"));

        // Revoking `extractor_tokens.access_token` specifically must not
        // affect `cc_response.access_token` - revocation only ever
        // touches the one token it was asked to revoke.
        revoke_access_token(&extractor_tokens.access_token)
            .await
            .unwrap();
        let (status, _) = get_with_auth(
            &api_router,
            "/me",
            Some(&format!("Bearer {}", extractor_tokens.access_token)),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);
        let (status, _) = get_with_auth(
            &api_router,
            "/client",
            Some(&format!("Bearer {}", cc_response.access_token)),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
    }

    #[test]
    fn hash_secret_is_deterministic_and_sensitive_to_input() {
        assert_eq!(hash_secret("abc"), hash_secret("abc"));
        assert_ne!(hash_secret("abc"), hash_secret("abd"));
    }

    #[test]
    fn random_secret_produces_64_hex_characters_and_is_never_repeated() {
        let a = random_secret();
        let b = random_secret();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn constant_time_eq_matches_identical_strings() {
        assert!(constant_time_eq("abc123", "abc123"));
    }

    #[test]
    fn constant_time_eq_rejects_a_single_differing_character() {
        assert!(!constant_time_eq("abc123", "abc124"));
    }

    #[test]
    fn constant_time_eq_rejects_differing_lengths() {
        assert!(!constant_time_eq("abc", "abcd"));
    }

    #[test]
    fn oauth_error_renders_the_exact_rfc_6749_json_shape() {
        let error = OAuthError::InvalidGrant("the authorization code is invalid".to_string());
        assert_eq!(error.code(), "invalid_grant");
        assert_eq!(error.description(), "the authorization code is invalid");
    }
}
