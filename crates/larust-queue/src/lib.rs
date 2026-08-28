//! A durable job queue — Laravel's `dispatch(new Job)`/`queue:work`.
//! Unlike `larust-events` (in-process, synchronous, no persistence), a
//! `Job` survives the current request and even a process restart:
//! `dispatch()` enqueues it durably; a separate `xr queue:work` process
//! (backed by `work()`) claims and executes jobs until stopped.
//!
//! Backed by SQL-family storage (`Config::queue_driver == "database"`, the
//! default) or Redis (`"redis"`) — see [`dispatch`]/[`worker`]'s own doc
//! comments for the dispatch shape, and [`sql_worker`]/[`redis_worker`]
//! for the two claim/lease/retry implementations.

mod dispatch;
mod redis_conn;
mod redis_dispatch;
mod redis_worker;
mod sql_dispatch;
mod sql_worker;
mod worker;

pub use dispatch::{dispatch, Job};
pub use worker::{work, JobRegistry};

/// Uses `larust_core::try_config()`, not `config()` — see
/// `larust_cache::store::cache_driver`'s own doc comment for the identical
/// reasoning. Shared by [`dispatch`] and [`worker`].
pub(crate) fn queue_driver() -> &'static str {
    larust_core::try_config()
        .map(|config| config.queue_driver.as_str())
        .unwrap_or("database")
}

use larust_core::AppError;
use larust_orm::Backend;
use sqlx::AnyPool;
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

pub(crate) async fn ensure_tables(pool: &AnyPool) -> Result<(), AppError> {
    TABLES_READY
        .get_or_try_init(|| async {
            let (jobs_table, failed_jobs_table) = match larust_orm::backend() {
                Backend::Sqlite => (
                    "CREATE TABLE IF NOT EXISTS jobs (\
                        id INTEGER PRIMARY KEY AUTOINCREMENT, \
                        job_type TEXT NOT NULL, \
                        payload TEXT NOT NULL, \
                        created_at INTEGER NOT NULL, \
                        attempts INTEGER NOT NULL DEFAULT 0, \
                        reserved_at INTEGER, \
                        available_at INTEGER NOT NULL DEFAULT 0\
                     )",
                    "CREATE TABLE IF NOT EXISTS failed_jobs (\
                        id INTEGER PRIMARY KEY AUTOINCREMENT, \
                        job_type TEXT NOT NULL, \
                        payload TEXT NOT NULL, \
                        error TEXT NOT NULL, \
                        failed_at INTEGER NOT NULL\
                     )",
                ),
                // `VARCHAR`, not MySQL's own `TEXT` — confirmed
                // empirically (a real, live MySQL server) that `sqlx`'s
                // `Any` driver maps every MySQL `TEXT`-family column to
                // its own generic `Blob` kind unconditionally, and
                // `Decode<Any> for String` only accepts `Text`-kind
                // values — so a `TEXT` column here would fail to decode
                // back as `String` at all (only `CHAR`/`VARCHAR` map to
                // `Any`'s `Text` kind; see `larust-http::session`'s own
                // `AnySessionStore::migrate` for the same finding in more
                // detail). `payload`/`error` get a generous cap
                // (`VARCHAR(4000)`) rather than an arbitrary-size one —
                // a real, documented trade-off for a large job payload.
                Backend::MySql => (
                    "CREATE TABLE IF NOT EXISTS jobs (\
                        id INTEGER PRIMARY KEY AUTO_INCREMENT, \
                        job_type VARCHAR(255) NOT NULL, \
                        payload VARCHAR(4000) NOT NULL, \
                        created_at INTEGER NOT NULL, \
                        attempts INTEGER NOT NULL DEFAULT 0, \
                        reserved_at INTEGER, \
                        available_at INTEGER NOT NULL DEFAULT 0\
                     )",
                    "CREATE TABLE IF NOT EXISTS failed_jobs (\
                        id INTEGER PRIMARY KEY AUTO_INCREMENT, \
                        job_type VARCHAR(255) NOT NULL, \
                        payload VARCHAR(4000) NOT NULL, \
                        error VARCHAR(4000) NOT NULL, \
                        failed_at INTEGER NOT NULL\
                     )",
                ),
                // Postgres has native, unbounded `TEXT` (no `Any`-driver
                // decode gap forcing MySQL's `VARCHAR(n)` workaround) and
                // its own `GENERATED ... AS IDENTITY` auto-increment syntax
                // — the modern, SQL-standard replacement for the older
                // `SERIAL` pseudo-type.
                Backend::Postgres => (
                    "CREATE TABLE IF NOT EXISTS jobs (\
                        id INTEGER GENERATED ALWAYS AS IDENTITY PRIMARY KEY, \
                        job_type TEXT NOT NULL, \
                        payload TEXT NOT NULL, \
                        created_at INTEGER NOT NULL, \
                        attempts INTEGER NOT NULL DEFAULT 0, \
                        reserved_at INTEGER, \
                        available_at INTEGER NOT NULL DEFAULT 0\
                     )",
                    "CREATE TABLE IF NOT EXISTS failed_jobs (\
                        id INTEGER GENERATED ALWAYS AS IDENTITY PRIMARY KEY, \
                        job_type TEXT NOT NULL, \
                        payload TEXT NOT NULL, \
                        error TEXT NOT NULL, \
                        failed_at INTEGER NOT NULL\
                     )",
                ),
            };

            sqlx::query(jobs_table)
                .execute(pool)
                .await
                .map_err(|source| AppError::Internal(Box::new(source)))?;

            // Compatibility upgrade for an app created before `attempts`/
            // `reserved_at`/`available_at` existed — SQLite-only: the
            // `CREATE TABLE` above already includes them for a brand-new
            // database, so a fresh MySQL app (MySQL support didn't exist
            // before these columns did) never has a pre-existing table
            // missing them, and there's no need to match MySQL's
            // differently-worded duplicate-column error text at all.
            if larust_orm::backend() == Backend::Sqlite {
                for statement in [
                    "ALTER TABLE jobs ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0",
                    "ALTER TABLE jobs ADD COLUMN reserved_at INTEGER",
                    "ALTER TABLE jobs ADD COLUMN available_at INTEGER NOT NULL DEFAULT 0",
                ] {
                    if let Err(error) = sqlx::query(statement).execute(pool).await {
                        let duplicate_column = matches!(&error, sqlx::Error::Database(database)
                            if database.message().contains("duplicate column name"));
                        if !duplicate_column {
                            return Err(AppError::Internal(Box::new(error)));
                        }
                    }
                }
            }
            // MySQL's `CREATE INDEX` has no `IF NOT EXISTS` clause at all
            // (unlike its `CREATE TABLE IF NOT EXISTS`, which is
            // standard) — so on MySQL this has to attempt the plain
            // `CREATE INDEX` and tolerate the specific "already exists"
            // error on every run after the first, the same error-text-
            // tolerance shape the SQLite `ALTER TABLE ADD COLUMN`
            // compatibility shim above already uses for its own
            // once-only-really-an-error case.
            match larust_orm::backend() {
                // Postgres supports `IF NOT EXISTS` on `CREATE INDEX`
                // (unlike MySQL) — same statement shape as SQLite.
                Backend::Sqlite | Backend::Postgres => {
                    sqlx::query(
                        "CREATE INDEX IF NOT EXISTS idx_jobs_available \
                         ON jobs (reserved_at, available_at, id)",
                    )
                    .execute(pool)
                    .await
                    .map_err(|source| AppError::Internal(Box::new(source)))?;
                }
                Backend::MySql => {
                    if let Err(error) = sqlx::query(
                        "CREATE INDEX idx_jobs_available ON jobs (reserved_at, available_at, id)",
                    )
                    .execute(pool)
                    .await
                    {
                        let duplicate_key = matches!(&error, sqlx::Error::Database(database)
                            if database.message().contains("Duplicate key name"));
                        if !duplicate_key {
                            return Err(AppError::Internal(Box::new(error)));
                        }
                    }
                }
            }

            sqlx::query(failed_jobs_table)
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
