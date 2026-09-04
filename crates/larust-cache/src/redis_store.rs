//! The `"redis"` `cache_driver` implementation - `store.rs` dispatches to
//! this module or [`crate::sql_store`] based on `Config::cache_driver`.
//!
//! Redis's own native `SET key value EX ttl` expiry replaces everything
//! [`crate::sql_store`] needs to implement by hand: no `ensure_table`
//! bootstrap (no schema - Redis keys need no `CREATE TABLE`), no
//! `sweep_expired_if_due` background sweep, no manual "check `expires_at`,
//! `DELETE` if stale" lazy-eviction branch in `get`. Redis simply returns
//! nil for an expired key with zero application-level bookkeeping.

use larust_core::AppError;
use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::time::Duration;
use tokio::sync::OnceCell;

/// Set once per process, the first time any Redis-backed cache function
/// runs - same lazy-singleton shape as `larust_orm::pool()`'s
/// `OnceLock<AnyPool>`, just a `OnceCell` since building a
/// `ConnectionManager` is itself async. `ConnectionManager` is designed to
/// be cloned freely (a lightweight handle around a shared, auto-
/// reconnecting connection), so callers get their own cheap clone rather
/// than contending on a single handle.
static CONNECTION: OnceCell<ConnectionManager> = OnceCell::const_new();

/// `REDIS_URL` (default `redis://127.0.0.1:6379`) - deliberately a single
/// plain env var, not a typed config block the way `config/database.rs`
/// is for SQL connections: Redis has no driver/dialect choice to make the
/// way `Driver` exists for, and this framework's own cache/queue usage
/// needs nothing beyond a connection string.
async fn connection() -> Result<ConnectionManager, AppError> {
    let manager = CONNECTION
        .get_or_try_init(|| async {
            let url = larust_support_env("REDIS_URL", "redis://127.0.0.1:6379");
            let client = redis::Client::open(url).map_err(|e| AppError::Config(Box::new(e)))?;
            ConnectionManager::new(client)
                .await
                .map_err(|e| AppError::Internal(Box::new(e)))
        })
        .await?;
    Ok(manager.clone())
}

/// A tiny local stand-in for `larust_support::config_env::env_or` - this
/// crate doesn't otherwise depend on `larust-support` (that crate depends
/// on *this* one, via `larust_support::cache`), so pulling it in just for
/// one env-var read would be a real dependency cycle, not a convenience.
fn larust_support_env(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

pub(crate) async fn put<T: Serialize>(key: &str, value: &T, ttl: Duration) -> Result<(), AppError> {
    let json =
        serde_json::to_string(value).map_err(|source| AppError::Internal(Box::new(source)))?;
    let mut conn = connection().await?;
    let ttl_secs = ttl.as_secs().max(1);
    conn.set_ex::<_, _, ()>(key, json, ttl_secs)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    Ok(())
}

pub(crate) async fn get<T: DeserializeOwned>(key: &str) -> Result<Option<T>, AppError> {
    let mut conn = connection().await?;
    let value: Option<String> = conn
        .get(key)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    let Some(json) = value else {
        return Ok(None);
    };
    // Same "a caller bug, not a miss" reasoning `sql_store::get`'s own doc
    // comment gives - see that function for the full explanation.
    serde_json::from_str(&json)
        .map(Some)
        .map_err(|source| AppError::Internal(Box::new(source)))
}

pub(crate) async fn forget(key: &str) -> Result<(), AppError> {
    let mut conn = connection().await?;
    conn.del::<_, ()>(key)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    Ok(())
}
