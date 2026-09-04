use larust_core::AppError;
use sqlx::any::{AnyConnectOptions, AnyPoolOptions};
use sqlx::AnyPool;
use std::future::Future;
use std::path::Path;
use std::str::FromStr;

/// Laravel's `RefreshDatabase` (not `DatabaseTransactions` - see below):
/// runs `body` against a brand-new, freshly migrated, fully isolated
/// SQLite database that nothing else ever touches. Unlike `test_db()`
/// (additive, one shared database across every `#[tokio::test]` fn in the
/// same test binary), each `test_transaction()` call gets its own
/// dedicated database file and doesn't need the "one test per file"
/// workaround every other process-wide mechanism in this crate relies on
/// - see the last paragraph below.
///
/// `body` receives the isolated pool directly (the same shape `test_db()`
/// already returns one for - pass it to `TestClient::new(router, &pool)`/
/// `Router::with_sessions(&pool, ..)` when the test needs a real router,
/// or query it directly to assert on rows). Every `#[derive(Model)]`
/// method and `QueryBuilder` call `body` makes also resolves to this same
/// isolated pool automatically, via `larust_orm::pool()`'s task-local
/// override (`larust_orm::with_pool_override`) - no parameter threading
/// needed anywhere in generated code.
///
/// **Why this is `RefreshDatabase`-shaped, not real transaction rollback**
/// (a real `BEGIN`-before/`ROLLBACK`-after design was tried first, and
/// abandoned - worth recording why, not just what shipped): wrapping
/// `body` in a raw SQL transaction (bypassing sqlx's own transaction
/// bookkeeping, to avoid `larust_orm::pool()`'s return type ever having to
/// change) works fine for direct `Model`/`QueryBuilder` calls, but breaks
/// the moment anything *else* on the same connection tries to open a
/// **real** `sqlx::Transaction` - which `tower-sessions-sqlx-store`
/// (already a real dependency, used for every session-backed route) does
/// internally on every session save. Since sqlx's own transaction
/// tracking has no idea a raw `BEGIN` already opened one, its `pool.
/// begin()` call issues a second, literal `BEGIN` on the same connection
/// - SQLite rejects nested `BEGIN`s outright ("cannot start a transaction
/// within a transaction"), so *any* test using a real router with
/// sessions (`TestClient` against CSRF/auth-gated routes - the single
/// most common, most valuable kind of test in this codebase) broke
/// immediately. A fresh, dedicated database per call has no shared
/// transaction state for anything else to collide with, so it works
/// unconditionally - at the cost of paying a real migration run per call
/// instead of reusing one already-migrated schema and undoing only the
/// data, which is what a true `DatabaseTransactions`-style design would
/// save. For this codebase's migration counts, that cost is small; see
/// `docs/ARCHITECTURE.md`'s Testing section for the full comparison.
///
/// **Why this doesn't need "one test per file"**: `tokio::task_local!` is
/// scoped per-*task*, not process-wide - unlike `larust_orm::connect()`,
/// the event-listener registry, `larust_mail::fake()`'s recorder, or any
/// table-bootstrap `OnceCell` in this codebase, nothing here is a
/// process-wide `OnceLock`/`OnceCell` that a second call could collide
/// with. Multiple independent `#[tokio::test]` fns - even in the same
/// file, even running concurrently - each get their own dedicated
/// database and task-local scope. `body` also has no `Send`/`'static`
/// bound of any kind (there's no `tokio::spawn` anywhere in this
/// implementation), matching every other `larust-testing` helper's
/// ergonomics exactly - a real router (`larust_http::Router` stores its
/// middleware as `Vec<Box<dyn Fn(..) -> ..>>` with no `+ Send` bound) can
/// be built directly inside `body`.
///
/// **A real, known gap this design doesn't close**: `larust-cache` and
/// `larust-queue` each lazily self-bootstrap their own table
/// (`cache_items`; `jobs`/`failed_jobs`) behind their *own*
/// process-wide `tokio::sync::OnceCell<()>`, memoized once per process -
/// not once per pool. If one `test_transaction()` call exercises code
/// that touches Cache or Queue, its bootstrap fires (correctly, against
/// that call's own isolated pool, via the same task-local override this
/// function relies on) and then never fires again for the rest of the
/// process - so a *second*, independent `test_transaction()` call that
/// also touches Cache/Queue hits "no such table," since its own fresh
/// pool never got the table created. Hit directly while building this
/// feature's own demo test (`demo/tests/transaction_test.rs` was
/// originally written against `PostController::store`, which calls
/// `cache::forget(...)`, before being rewritten to avoid it for exactly
/// this reason). Out of scope to fix here - it would mean changing how
/// `larust-cache`/`larust-queue` track "is my table ready" from
/// per-process to per-pool, a real change to two other crates' own
/// designs, not something `test_transaction()` can paper over from the
/// outside. Tests combining `test_transaction()` with Cache/Queue in the
/// same process should stick to a single call, or avoid exercising
/// those two crates' bootstrap path more than once.
pub async fn test_transaction<F, Fut, T>(migrations_dir: &Path, body: F) -> T
where
    F: FnOnce(AnyPool) -> Fut,
    Fut: Future<Output = T>,
{
    // Returns `T` directly and `.expect()`s on setup failure, unlike
    // `test_db()`'s `Result<AnyPool, AppError>` - deliberate, not an
    // oversight: `T` is frequently not itself a `Result` (most bodies
    // just assert and return `()`), so a `Result`-wrapping return here
    // would force every caller to unwrap two independent failure layers
    // (this function's own setup, and whatever `body` returns) instead of
    // one. An isolated test database that fails to even connect/migrate
    // is an unrecoverable test-infra problem, the same class of failure
    // `.expect()`/`.unwrap()` is reserved for throughout this codebase's
    // test helpers.
    let pool = connect_isolated(migrations_dir)
        .await
        .expect("failed to set up an isolated test-transaction database");

    // `(*pool).clone()`, not `pool.clone()` - `pool` is `&'static
    // AnyPool`, and `&T`'s own blanket `Clone` impl would otherwise be
    // found first, "cloning" the reference itself rather than producing
    // the owned `AnyPool` `body` expects (cheap either way -
    // `AnyPool` is `Arc`-backed internally).
    let scoped_pool = (*pool).clone();
    larust_orm::with_pool_override(pool, body(scoped_pool)).await
}

/// A dedicated, freshly migrated pool - deliberately *not* registered
/// with `larust_orm::connect()`'s process-wide `OnceLock` (that's
/// `test_db()`'s job; this function needs its own, separate pool object
/// per call, not a shared global one). Leaked via `Box::leak` to get a
/// genuine `&'static AnyPool` - an `Arc`-backed pool handle (and its
/// temp directory) per `test_transaction()` call, in a short-lived test
/// process, the same "acceptable, deliberate leak in test-only code"
/// reasoning `test_db()`'s own `.keep()`'d tempdir already relies on -
/// though not quite the same *scale*: `test_db()` leaks once per test
/// *binary* (memoized behind its own `OnceCell`), while this leaks once
/// per *call*, so a test suite making many `test_transaction()` calls in
/// one binary accumulates that many permanently-open connection pools
/// and temp files for the life of the process. Still bounded and
/// reclaimed at process exit, just a faster growth rate - worth knowing
/// if a test suite built on this one ever grows large.
async fn connect_isolated(migrations_dir: &Path) -> Result<&'static AnyPool, AppError> {
    sqlx::any::install_default_drivers();
    let dir = tempfile::tempdir()
        .map_err(|source| AppError::Internal(Box::new(source)))?
        .keep();
    let database_url = format!("sqlite://{}/test.sqlite", dir.display());

    // Sets `larust_orm::backend()` without touching the process-wide
    // `larust_orm::pool()` singleton (see `ensure_backend`'s own doc
    // comment) - every framework crate's own bootstrap SQL branches on
    // `backend()`, so it has to resolve correctly here too, even though
    // this function deliberately keeps its own pool separate from
    // `larust_orm::connect()`'s.
    larust_orm::ensure_backend(&database_url)?;
    let database_url = larust_orm::normalize_sqlite_url(&database_url);
    let connect_url = if database_url.contains('?') {
        format!("{database_url}&mode=rwc")
    } else {
        format!("{database_url}?mode=rwc")
    };

    let options = AnyConnectOptions::from_str(&connect_url)
        .map_err(|source| AppError::Config(Box::new(source)))?;
    let pool = AnyPoolOptions::new()
        .connect_with(options)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    let pool: &'static AnyPool = Box::leak(Box::new(pool));

    larust_orm::with_pool_override(pool, larust_orm::migrate(migrations_dir)).await?;

    Ok(pool)
}
