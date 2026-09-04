use larust_core::AppError;
use sqlx::AnyPool;
use std::path::Path;
use tokio::sync::OnceCell;

static TEST_DB: OnceCell<AnyPool> = OnceCell::const_new();

/// Connects to a fresh, migrated SQLite database for tests (Rust's own
/// natural test-isolation boundary: `cargo test` already compiles each
/// `tests/*.rs` file as a separate process, so this is one database per
/// test *file*, not per test function - every `#[tokio::test]` fn in the
/// same file shares it).
///
/// Idempotent within a process: the first call creates the database and
/// runs migrations; every later call in the same test binary (i.e. every
/// other `#[tokio::test]` fn in that file) just returns a clone of the
/// already-connected pool, sidestepping `larust_support::orm::connect`'s
/// documented "second call errors" behavior entirely.
///
/// Backed by an on-disk `tempfile` database, not `sqlite::memory:` - a
/// pooled in-memory SQLite database would give each connection its own
/// private, empty database unless opened with SQLite's shared-cache URI
/// mode, which has its own real multi-connection subtleties not worth
/// risking here. The temp directory is intentionally leaked (kept alive
/// for the process's lifetime, not tied to any single test function's
/// stack frame) rather than cleaned up - the OS reclaims temp directories
/// over time, and no test function is a safe place to drop the guard
/// without deleting the database out from under a *different* test
/// function that runs afterward and shares the same pool.
///
/// This is an additive, no-isolation-between-tests tradeoff, not
/// Laravel's `DatabaseTransactions` (a per-test rollback sandbox): every
/// generated `#[derive(Model)]` method and `QueryBuilder` call goes
/// through `larust_orm`'s single process-wide pool directly, with no
/// injectable executor to route a test through its own transaction.
/// Write assertions scoped to the specific rows a test creates, not broad
/// counts across the whole table.
pub async fn test_db(migrations_dir: &Path) -> Result<AnyPool, AppError> {
    let pool = TEST_DB
        .get_or_try_init(|| async {
            let dir = tempfile::tempdir()
                .map_err(|source| AppError::Internal(Box::new(source)))?
                .keep();
            let database_url = format!("sqlite://{}/test.sqlite", dir.display());

            larust_support::orm::connect(&database_url).await?;
            larust_support::orm::migrate(migrations_dir).await?;

            larust_support::orm::pool().cloned()
        })
        .await?;

    Ok(pool.clone())
}
