//! Laravel's `laravel/sanctum` — narrowed to its bearer-token half, the
//! part with no existing Larust equivalent at all. `larust_auth::Auth<U>`
//! already covers Sanctum's other job (a first-party frontend
//! authenticating via session cookie) natively, so this crate doesn't
//! touch that path — it only adds what's missing: a plain API client (no
//! cookie jar, no browser) authenticating via `Authorization: Bearer
//! {token}`.
//!
//! **A separate crate from `larust-auth`, deliberately.** `larust-auth`
//! currently has zero direct dependency on `larust-orm`/`sqlx` — every bit
//! of persistence is delegated to the app's own
//! [`Authenticatable::find_for_auth`] impl, keeping the auth *logic*
//! storage-agnostic. A `personal_access_tokens` table is new storage this
//! functionality has to own, so it lives here instead, the same way
//! `larust-permissions`/`larust-notifications`/`larust-cache` each own
//! their own table rather than being folded into a crate that otherwise
//! has none. This crate depends on `larust-auth` (for
//! [`Authenticatable`]/`AppError` conventions), never the other way round.
//!
//! Re-exported through `larust_support::sanctum` (see `crates/
//! larust-support/src/lib.rs`) so generated apps depend only on
//! `larust-support`, never on this crate directly.
//!
//! ## Deliberately out of scope for this version
//!
//! - **No token abilities/scopes** (Sanctum's `can()`/ability-string
//!   gating). A real separate feature layering on top of the existing
//!   `larust_auth::Policy` trait — not attempted here.
//! - **No SPA/stateful cookie mode.** See this doc comment's own opening
//!   paragraph — `Auth<U>` already covers it.
//! - **No `auth:sanctum` middleware-string recognition in `xr convert`.**
//!   `crates/larust-convert/src/routes.rs` already blanket-defers every
//!   `Route::middleware(...)->group(...)` call, deliberately — this crate
//!   doesn't special-case Sanctum's own alias within that existing
//!   boundary, the same choice `larust-permissions`'s own doc comment
//!   already made for `role:`/`permission:` middleware strings.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, StatusCode};
use larust_auth::Authenticatable;
use larust_core::AppError;
use rand::Rng;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use std::fmt::Write as _;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Extracts the user identified by a valid `Authorization: Bearer {token}`
/// header, or rejects with `401` — the bearer-token sibling to
/// `larust_auth::Auth<U>`, never a replacement for it. A route picks
/// whichever extractor matches how it's actually authenticated, the same
/// way a Laravel route explicitly chooses `auth:sanctum` vs `auth:web`
/// middleware; `Auth<U>` gains no branch point for "no session, try a
/// header instead."
pub struct ApiAuth<U>(pub U);

// GOTCHAS.md: axum-core declares `FromRequestParts` via `#[async_trait]`,
// not native async-fn-in-traits — an impl written as a plain `async fn`
// fails with a confusing E0195 lifetime error instead of a clear message
// about the mismatch. Same note `larust_auth::Auth<U>`'s own impl carries.
#[axum::async_trait]
impl<S, U> FromRequestParts<S> for ApiAuth<U>
where
    S: Send + Sync,
    U: Authenticatable,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let user_id = authenticate(&parts.headers).await?;
        U::find_for_auth(user_id)
            .await?
            .map(ApiAuth)
            .ok_or_else(unauthorized)
    }
}

/// One generic message for every failure mode below (missing header,
/// malformed token, unknown id, hash mismatch, expired, deleted user) —
/// never reveals *which* check failed, the same instinct a password check
/// already follows.
fn unauthorized() -> AppError {
    AppError::Http {
        status: StatusCode::UNAUTHORIZED,
        message: "invalid or missing API token".to_string(),
    }
}

async fn authenticate(headers: &HeaderMap) -> Result<i64, AppError> {
    let raw = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(unauthorized)?;
    let token = raw.strip_prefix("Bearer ").ok_or_else(unauthorized)?;
    // The row id is embedded ahead of the plaintext (`"{id}|{plaintext}"`,
    // matching real Sanctum's own token shape) so lookup below is a single
    // O(1) primary-key read — never a full-table scan hashing every stored
    // token looking for a match.
    let (id_part, plaintext) = token.split_once('|').ok_or_else(unauthorized)?;
    let id: i64 = id_part.parse().map_err(|_| unauthorized())?;

    let pool = larust_orm::pool()?;
    ensure_table(pool).await?;

    let row: Option<(i64, String, Option<i64>)> = sqlx::query_as(
        "SELECT user_id, token_hash, expires_at FROM personal_access_tokens WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|source| AppError::Internal(Box::new(source)))?;
    let Some((user_id, token_hash, expires_at)) = row else {
        return Err(unauthorized());
    };

    if !hashes_match(&hash_token(plaintext), &token_hash) {
        return Err(unauthorized());
    }
    if expires_at.is_some_and(|expires_at| expires_at <= now_unix_secs()) {
        return Err(unauthorized());
    }

    // Best-effort — a bookkeeping write failing here is never a reason to
    // fail the request itself, same tolerance `larust-cache`'s own expiry
    // sweep applies to its background cleanup.
    if let Err(error) =
        sqlx::query("UPDATE personal_access_tokens SET last_used_at = ? WHERE id = ?")
            .bind(now_unix_secs())
            .bind(id)
            .execute(pool)
            .await
    {
        tracing::warn!(%error, "failed to record API token last_used_at");
    }

    Ok(user_id)
}

/// SHA-256, not `larust_auth::hash_password`'s Argon2id — a deliberate
/// choice, matching real Sanctum's own: the token itself is already 32
/// bytes of CSPRNG output, not a user-chosen secret that needs a slow KDF
/// to resist brute-forcing. Argon2 on every single API request would add
/// real, unnecessary latency for no security benefit here.
fn hash_token(plaintext: &str) -> String {
    let digest = Sha256::digest(plaintext.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Constant-time comparison of two fixed-length hex digests — a plain
/// `==` short-circuits on the first differing byte, leaking timing
/// information proportional to how much of the token an attacker guessed
/// correctly. Both inputs here are always 64-char SHA-256 hex digests, so
/// a hand-written fixed-width XOR-fold is enough; no new dependency
/// needed for it.
fn hashes_match(a: &str, b: &str) -> bool {
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

fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_secs() as i64
}

/// `byte_len` cryptographically-strong random bytes, hex-encoded. A small
/// local duplicate of `larust_http::random_hex`'s own few lines rather
/// than adding `larust-http` as a dependency of this crate for one
/// utility fn — the same "just call the primitive directly, don't share a
/// bespoke wrapper" style every shim crate here already follows for
/// `larust_orm::pool()`.
fn random_hex(byte_len: usize) -> String {
    let mut bytes = vec![0u8; byte_len];
    rand::thread_rng().fill(&mut bytes[..]);
    let mut hex = String::with_capacity(byte_len * 2);
    for byte in bytes {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Same lazy self-bootstrap idiom `larust-notifications`'s `ensure_table`
/// establishes — plain `CREATE TABLE IF NOT EXISTS`, no migration file and
/// no explicit startup call needed anywhere. Deliberately **not** memoized
/// behind a `OnceCell`: `larust_testing::test_transaction`/a fresh
/// per-test database means a process-wide completion flag can point at a
/// since-discarded database — the exact regression that crate's own doc
/// comment documents hitting once already. `IF NOT EXISTS` makes
/// re-running this on every call cheap enough that giving up the
/// memoization is the better trade.
///
/// `user_id` is a plain `INTEGER`, not a typed foreign key into an
/// app-owned `users` table this crate has no visibility into — the same
/// reasoning `larust-notifications`'s own `notifiable_id` column and
/// `larust-permissions`'s own `user_id` columns already use.
async fn ensure_table(pool: &SqlitePool) -> Result<(), AppError> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS personal_access_tokens (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            user_id INTEGER NOT NULL, \
            name TEXT NOT NULL, \
            token_hash TEXT NOT NULL UNIQUE, \
            expires_at INTEGER, \
            last_used_at INTEGER, \
            created_at INTEGER NOT NULL\
         )",
    )
    .execute(pool)
    .await
    .map_err(|source| AppError::Internal(Box::new(source)))?;
    Ok(())
}

/// Issues a new token for `user`, returning the plaintext value
/// (`"{id}|{plaintext}"`) — Laravel's `$user->createToken('name')`. This
/// exact string is the only time the plaintext is ever available; only its
/// hash is stored, so it can't be recovered from the database later (a
/// stolen DB dump hands out no usable tokens). `ttl` mirrors
/// `larust_cache::put`'s own optional-expiry convention — `None` for a
/// token that never expires, matching Sanctum's own default.
pub async fn create_token(
    user: &impl Authenticatable,
    name: &str,
    ttl: Option<Duration>,
) -> Result<String, AppError> {
    let pool = larust_orm::pool()?;
    ensure_table(pool).await?;

    let plaintext = random_hex(32);
    let token_hash = hash_token(&plaintext);
    let created_at = now_unix_secs();
    let expires_at = ttl.map(|ttl| created_at + ttl.as_secs() as i64);

    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO personal_access_tokens (user_id, name, token_hash, expires_at, created_at) \
         VALUES (?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(user.auth_id())
    .bind(name)
    .bind(&token_hash)
    .bind(expires_at)
    .bind(created_at)
    .fetch_one(pool)
    .await
    .map_err(|source| AppError::Internal(Box::new(source)))?;

    Ok(format!("{id}|{plaintext}"))
}

/// Revokes one token by its row id (the `id` portion of the token string
/// returned by [`create_token`]) — Laravel's
/// `$user->tokens()->where('id', $id)->delete()`. Not an error to revoke
/// an id that doesn't exist or was already revoked, same "no meaningful
/// double-removal error" reasoning `larust-notifications`'s `mark_as_read`
/// already applies.
pub async fn revoke_token(id: i64) -> Result<(), AppError> {
    let pool = larust_orm::pool()?;
    ensure_table(pool).await?;
    sqlx::query("DELETE FROM personal_access_tokens WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    Ok(())
}

/// Revokes every token `user` currently holds — Laravel's
/// `$user->tokens()->delete()`, commonly used for a "log out everywhere"
/// action.
pub async fn revoke_all_tokens_for(user: &impl Authenticatable) -> Result<(), AppError> {
    let pool = larust_orm::pool()?;
    ensure_table(pool).await?;
    sqlx::query("DELETE FROM personal_access_tokens WHERE user_id = ?")
        .bind(user.auth_id())
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt;

    #[derive(Clone)]
    struct TestUser {
        id: i64,
    }

    impl Authenticatable for TestUser {
        fn auth_id(&self) -> i64 {
            self.id
        }

        async fn find_for_auth(id: i64) -> Result<Option<Self>, AppError> {
            // Mirrors a real `#[derive(Model)]`-backed lookup closely enough
            // for these tests: id `1` exists, everything else doesn't —
            // exercising the "token valid but the user is gone" rejection
            // path without needing a real `users` table.
            Ok((id == 1).then_some(TestUser { id }))
        }
    }

    async fn connect_test_db() {
        let dir = tempfile::tempdir().unwrap().keep();
        let database_url = format!("sqlite://{}/test.sqlite", dir.display());
        larust_orm::connect(&database_url).await.unwrap();
    }

    fn router() -> Router {
        Router::new().route(
            "/me",
            get(|ApiAuth(user): ApiAuth<TestUser>| async move { user.id.to_string() }),
        )
    }

    async fn get_with_auth_header(router: &Router, header: Option<&str>) -> (StatusCode, String) {
        let mut request = axum::http::Request::get("/me");
        if let Some(header) = header {
            request = request.header(header::AUTHORIZATION, header);
        }
        let response = router
            .clone()
            .oneshot(request.body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    /// All scenarios share one test function, not several — `larust_orm::
    /// connect()` sets a process-wide pool exactly once, the same
    /// constraint `larust-permissions`'/`larust-notifications`'s own test
    /// suites document and work around.
    #[tokio::test]
    async fn sanctum_crate_behaves_correctly_across_every_scenario() {
        connect_test_db().await;
        let router = router();
        let alice = TestUser { id: 1 };

        // No header at all, a header missing the `Bearer ` prefix, and a
        // malformed token (no `|` separator) are all rejected before ever
        // touching the database.
        let (status, _) = get_with_auth_header(&router, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = get_with_auth_header(&router, Some("not-bearer-shaped")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = get_with_auth_header(&router, Some("Bearer no-pipe-here")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // A freshly created token authenticates successfully.
        let token = create_token(&alice, "test-token", None).await.unwrap();
        let (status, body) = get_with_auth_header(&router, Some(&format!("Bearer {token}"))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "1");

        // A well-formed-but-wrong token (right id, wrong plaintext) fails.
        let (id_part, _) = token.split_once('|').unwrap();
        let wrong = format!(
            "Bearer {id_part}|0000000000000000000000000000000000000000000000000000000000000000"
        );
        let (status, _) = get_with_auth_header(&router, Some(&wrong)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // An unknown row id fails too (never created, or already deleted).
        let (status, _) = get_with_auth_header(&router, Some("Bearer 999999|whatever")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // last_used_at was populated by the successful auth above.
        let pool = larust_orm::pool().unwrap();
        let (id_num,): (i64,) =
            sqlx::query_as("SELECT id FROM personal_access_tokens WHERE user_id = ? AND name = ?")
                .bind(alice.id)
                .bind("test-token")
                .fetch_one(pool)
                .await
                .unwrap();
        let (last_used,): (Option<i64>,) =
            sqlx::query_as("SELECT last_used_at FROM personal_access_tokens WHERE id = ?")
                .bind(id_num)
                .fetch_one(pool)
                .await
                .unwrap();
        assert!(last_used.is_some());

        // A token issued with a TTL in the past is rejected as expired.
        let expired = create_token(&alice, "expired", Some(Duration::from_secs(0)))
            .await
            .unwrap();
        // `now_unix_secs() + 0` can equal "now" exactly, and the expiry
        // check is `<=` — give it a moment so the clock has definitely
        // moved past `expires_at`.
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let (status, _) = get_with_auth_header(&router, Some(&format!("Bearer {expired}"))).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // Revoking a token invalidates it immediately.
        revoke_token(id_num).await.unwrap();
        let (status, _) = get_with_auth_header(&router, Some(&format!("Bearer {token}"))).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // revoke_all_tokens_for revokes every token a user holds, not just
        // one — bob has two, alice (id 1) is untouched.
        let bob = TestUser { id: 2 };
        let bob_token_a = create_token(&bob, "a", None).await.unwrap();
        let bob_token_b = create_token(&bob, "b", None).await.unwrap();
        revoke_all_tokens_for(&bob).await.unwrap();
        for token in [bob_token_a, bob_token_b] {
            let (status, _) = get_with_auth_header(&router, Some(&format!("Bearer {token}"))).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
        }

        // A token for a user id `find_for_auth` can't resolve (deleted
        // after issuance) fails at the final step, not the lookup itself.
        let ghost = TestUser { id: 404 };
        let ghost_token = create_token(&ghost, "ghost", None).await.unwrap();
        let (status, _) =
            get_with_auth_header(&router, Some(&format!("Bearer {ghost_token}"))).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}
