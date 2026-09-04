//! The `"database"` (default) `queue_driver` implementation - unchanged
//! from before Redis support existed. `dispatch.rs` dispatches to this
//! module or [`crate::redis_dispatch`] based on `Config::queue_driver`.

use crate::{ensure_tables, now_unix_secs, Job};
use larust_core::AppError;

/// Serializes `job` to JSON and enqueues it - durable the moment this
/// returns `Ok`, independent of whether any `xr queue:work` process is
/// currently running to pick it up.
pub(crate) async fn dispatch<J: Job>(job: &J) -> Result<(), AppError> {
    let pool = larust_orm::pool()?;
    ensure_tables(pool).await?;

    let payload =
        serde_json::to_string(job).map_err(|source| AppError::Internal(Box::new(source)))?;

    let now = now_unix_secs();
    let backend = larust_orm::backend();
    let sql = format!(
        "INSERT INTO jobs (job_type, payload, created_at, available_at) VALUES ({}, {}, {}, {})",
        larust_orm::placeholder(backend, 1),
        larust_orm::placeholder(backend, 2),
        larust_orm::placeholder(backend, 3),
        larust_orm::placeholder(backend, 4),
    );
    sqlx::query(&sql)
        .bind(J::JOB_TYPE)
        .bind(payload)
        .bind(now)
        .bind(now)
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    Ok(())
}
