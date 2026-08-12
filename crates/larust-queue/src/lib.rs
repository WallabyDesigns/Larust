//! A durable, SQLite-backed job queue — Laravel's `dispatch(new Job)`/
//! `queue:work`. Unlike `larust-events` (in-process, synchronous, no
//! persistence), a `Job` survives the current request and even a process
//! restart: `dispatch()` inserts a row; a separate `xr queue:work` process
//! (backed by `work()`) claims and executes rows until stopped.

mod dispatch;
mod worker;

pub use dispatch::{dispatch, Job};
pub use worker::{work, JobRegistry};

use larust_core::AppError;
use sqlx::SqlitePool;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::OnceCell;

/// Set once per process, the first time `dispatch()` or `work()` runs —
/// same lazy self-bootstrap idiom `larust-cache` already established for
/// `cache_items` (see that crate's `store.rs` for the fuller rationale: it
/// goes a step further than `larust_orm::migrate::run`'s or
/// `larust_http::session`'s self-bootstrapping tables, neither of which
/// runs its `CREATE TABLE IF NOT EXISTS` lazily on first *use* — both
/// still need one explicit call at startup/wiring time).
static TABLES_READY: OnceCell<()> = OnceCell::const_new();

pub(crate) async fn ensure_tables(pool: &SqlitePool) -> Result<(), AppError> {
    TABLES_READY
        .get_or_try_init(|| async {
            sqlx::query(
                "CREATE TABLE IF NOT EXISTS jobs (\
                    id INTEGER PRIMARY KEY AUTOINCREMENT, \
                    job_type TEXT NOT NULL, \
                    payload TEXT NOT NULL, \
                    created_at INTEGER NOT NULL\
                 )",
            )
            .execute(pool)
            .await
            .map_err(|source| AppError::Internal(Box::new(source)))?;

            sqlx::query(
                "CREATE TABLE IF NOT EXISTS failed_jobs (\
                    id INTEGER PRIMARY KEY AUTOINCREMENT, \
                    job_type TEXT NOT NULL, \
                    payload TEXT NOT NULL, \
                    error TEXT NOT NULL, \
                    failed_at INTEGER NOT NULL\
                 )",
            )
            .execute(pool)
            .await
            .map_err(|source| AppError::Internal(Box::new(source)))?;

            Ok(())
        })
        .await?;
    Ok(())
}

pub(crate) fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_secs() as i64
}
