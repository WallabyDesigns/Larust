//! The Redis connection singleton shared by [`crate::redis_dispatch`] and
//! [`crate::redis_worker`] — same lazy-singleton shape as
//! `larust_orm::pool()`'s `OnceLock<AnyPool>`, just a `OnceCell` since
//! building a `ConnectionManager` is itself async. Not shared with
//! `larust-cache`'s own identical-looking helper — these are two
//! independent crates, each already tolerating this amount of duplication
//! elsewhere (`larust-permissions`/`larust-notifications`/`larust-sanctum`
//! each own a near-identical `ensure_table` pattern too).

use larust_core::AppError;
use redis::aio::ConnectionManager;
use tokio::sync::OnceCell;

static CONNECTION: OnceCell<ConnectionManager> = OnceCell::const_new();

/// `REDIS_URL` (default `redis://127.0.0.1:6379`) — deliberately a single
/// plain env var, not a typed config block: Redis has no driver/dialect
/// choice to make the way `larust_orm::config::Driver` exists for.
pub(crate) async fn connection() -> Result<ConnectionManager, AppError> {
    let manager = CONNECTION
        .get_or_try_init(|| async {
            let url =
                std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
            let client = redis::Client::open(url).map_err(|e| AppError::Config(Box::new(e)))?;
            ConnectionManager::new(client)
                .await
                .map_err(|e| AppError::Internal(Box::new(e)))
        })
        .await?;
    Ok(manager.clone())
}
