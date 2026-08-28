//! Laravel's `Cache::put()`/`get()`/`forget()`/`remember()` — dispatches
//! to [`crate::sql_store`] (the default, `Config::cache_driver ==
//! "database"`) or [`crate::redis_store`] (`"redis"`) at each call,
//! mirroring `larust_mail::send::deliver`'s own `match config.mail_driver
//! .as_str() { ... }` shape — the one existing "a config string picks a
//! runtime code path" precedent in this codebase. Neither backend module
//! is reachable from outside this crate (both `pub(crate)`); every public
//! signature here is unchanged from before Redis support existed, so
//! nothing about *using* this crate depends on which driver is active.

use larust_core::AppError;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::future::Future;
use std::time::Duration;

/// Uses `larust_core::try_config()`, not `config()` — this crate's own
/// functions have never required `Application::new()` to have run first
/// (they only ever needed `larust_orm::connect()`), and some callers
/// (narrow test helpers building a bare pool directly, matching
/// `larust_http::session::cookie_name()`'s own identical reasoning) still
/// don't call it. Falling back to `"database"` (the same default
/// `Config::cache_driver` itself has) when no config has been published
/// yet keeps this a purely additive change rather than a new panic path
/// for code that worked before Redis support existed.
fn cache_driver() -> &'static str {
    larust_core::try_config()
        .map(|config| config.cache_driver.as_str())
        .unwrap_or("database")
}

/// Stores `value` under `key`, serialized as JSON, expiring after `ttl`.
/// Overwrites any existing entry under the same key (Laravel's own `put()`
/// semantics — not an error to reuse a key).
pub async fn put<T: Serialize>(key: &str, value: &T, ttl: Duration) -> Result<(), AppError> {
    match cache_driver() {
        "redis" => crate::redis_store::put(key, value, ttl).await,
        _ => crate::sql_store::put(key, value, ttl).await,
    }
}

/// Returns `Ok(None)` for a missing or expired key (an ordinary cache
/// miss). A key that exists but whose stored JSON can't be coerced into
/// `T` — e.g. reading a key back with an incompatible type than it was
/// `put` with — is a caller bug, not a miss, so it surfaces as
/// `Err(AppError::Internal)` rather than silently degrading to `None` the
/// way Laravel's own cache would.
pub async fn get<T: DeserializeOwned>(key: &str) -> Result<Option<T>, AppError> {
    match cache_driver() {
        "redis" => crate::redis_store::get(key).await,
        _ => crate::sql_store::get(key).await,
    }
}

/// Removes `key`, if present. Not an error to forget a key that was never
/// set or has already expired.
pub async fn forget(key: &str) -> Result<(), AppError> {
    match cache_driver() {
        "redis" => crate::redis_store::forget(key).await,
        _ => crate::sql_store::forget(key).await,
    }
}

/// Returns the cached value under `key` if present and unexpired;
/// otherwise calls `f`, stores its result under `key` for `ttl`, and
/// returns it. `f` is a plain generic closure, not a trait method, so
/// there's no async-fn-in-traits `Send` pitfall to work around here (see
/// `docs/GOTCHAS.md`). Implemented purely in terms of `get`/`put` above,
/// so it needs no driver-specific logic of its own.
///
/// Not race-safe under concurrent callers missing on the same key at once
/// — same accepted tradeoff as this crate's own
/// `PostController::find_or_create_tag` in `demo`/`examples/blog`. Both
/// would run `f` and both would `put`; harmless (both drivers' `put` is
/// last-write-wins), just not exactly-once.
pub async fn remember<T, F, Fut>(key: &str, ttl: Duration, f: F) -> Result<T, AppError>
where
    T: Serialize + DeserializeOwned,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, AppError>>,
{
    if let Some(value) = get::<T>(key).await? {
        return Ok(value);
    }

    let value = f().await?;
    put(key, &value, ttl).await?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn connect_test_db() {
        let dir = tempfile::tempdir().unwrap().keep();
        let database_url = format!("sqlite://{}/test.sqlite", dir.display());
        larust_orm::connect(&database_url).await.unwrap();
    }

    /// No `Application::new()` call anywhere in this test — proves
    /// `put`/`get`/`forget`/`remember` still work with no published
    /// `Config` at all (the `"database"` driver, via `cache_driver()`'s
    /// own fallback), the same guarantee this crate has always given,
    /// unaffected by Redis support existing now.
    #[tokio::test]
    async fn cache_round_trips_through_the_database_driver_with_no_published_config() {
        connect_test_db().await;

        put("greeting", &"hello", Duration::from_secs(60))
            .await
            .unwrap();
        let value: Option<String> = get("greeting").await.unwrap();
        assert_eq!(value, Some("hello".to_string()));

        forget("greeting").await.unwrap();
        let value: Option<String> = get("greeting").await.unwrap();
        assert_eq!(value, None);

        let remembered = remember("computed", Duration::from_secs(60), || async {
            Ok::<_, AppError>("computed value".to_string())
        })
        .await
        .unwrap();
        assert_eq!(remembered, "computed value");
    }
}
