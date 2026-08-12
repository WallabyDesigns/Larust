use larust_core::AppError;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::future::Future;
use std::str::FromStr;
use std::sync::OnceLock;

static POOL: OnceLock<SqlitePool> = OnceLock::new();

tokio::task_local! {
    /// Set only by `larust_testing::test_transaction` (via
    /// `with_pool_override`) — everywhere else this is simply never set,
    /// and `pool()` falls through to the process-wide `POOL` exactly as
    /// before. Task-local, not process-wide: unlike `POOL` itself, this
    /// doesn't need "first writer wins" semantics, since each test that
    /// uses it sets its own value in its own task, isolated from every
    /// other task doing the same thing concurrently.
    static POOL_OVERRIDE: &'static SqlitePool;
}

/// Connects to the database and stores the pool process-wide (same
/// `OnceLock` pattern as `larust-http`'s route-name registry). Call once
/// at startup, after config/`.env` has been loaded.
pub async fn connect(database_url: &str) -> Result<(), AppError> {
    let options = SqliteConnectOptions::from_str(database_url)
        .map_err(|e| AppError::Config(Box::new(e)))?
        .create_if_missing(true);

    let pool = SqlitePoolOptions::new()
        .connect_with(options)
        .await
        .map_err(|e| AppError::Internal(Box::new(e)))?;

    POOL.set(pool).map_err(|_| {
        AppError::Internal(Box::new(std::io::Error::other(
            "connect() called more than once",
        )))
    })?;

    Ok(())
}

/// Returns the pool every `#[derive(Model)]` method and `QueryBuilder`
/// call resolves its connection through — this single resolution point
/// (not a parameter threaded through every generated method) is what
/// makes `with_pool_override` below work at all. Checks the task-local
/// override first (set only inside `larust_testing::test_transaction`),
/// then falls back to the process-wide pool. Errors (rather than panics)
/// if neither is set — a misconfigured startup order is a real
/// possibility (e.g. a route handler running before `main` finishes
/// wiring up the database), not a truly unreachable state.
pub fn pool() -> Result<&'static SqlitePool, AppError> {
    if let Ok(overridden) = POOL_OVERRIDE.try_with(|pool| *pool) {
        return Ok(overridden);
    }

    POOL.get().ok_or_else(|| {
        AppError::Internal(Box::new(std::io::Error::other(
            "database not connected; call larust_orm::connect() \
             (via larust_support::orm::connect) at startup before serving requests",
        )))
    })
}

/// Runs `fut` with `pool` resolved by every `pool()` call made from
/// within it — and from anything it directly `.await`s, since a
/// `tokio::task_local!` is visible throughout one task's execution. A
/// future `fut` hands off to `tokio::spawn` as a *separate* detached
/// task would **not** see this override (spawned tasks don't inherit
/// their parent's task-locals) — confirmed nothing in `larust-orm`'s or
/// `larust-macros`' generated code does that (`grep` for `tokio::spawn`/
/// `join_all` in both turns up nothing), so this is safe for every
/// existing `#[derive(Model)]`/`QueryBuilder` call path today.
///
/// Used by `larust_testing::test_transaction`; not meant for application
/// code — there is deliberately no equivalent re-exported through
/// `larust_support::orm`.
pub async fn with_pool_override<F: Future>(pool: &'static SqlitePool, fut: F) -> F::Output {
    POOL_OVERRIDE.scope(pool, fut).await
}
