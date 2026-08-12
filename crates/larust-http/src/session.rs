//! Cookie-based sessions over `tower-sessions` (a real, maintained session
//! engine — the cookie carries only an opaque session ID, actual data
//! lives server-side in a `Store`, which is the standard, more secure
//! pattern rather than shipping session contents in the cookie itself).
//!
//! Backed by `tower-sessions-sqlx-store`'s `SqliteStore`, not an in-memory
//! store — session data needs to survive a process restart (a deploy, a
//! crash, `xr dev`'s rebuild-and-restart cycle), not just live for the
//! lifetime of one process. There's deliberately no in-memory option left
//! in this crate's public API: an in-memory store is a real, common trap
//! (an app that "works" in every manual test, then silently logs everyone
//! out on every deploy) — same shape as Laravel's own `array` session
//! driver being the wrong thing to ship to production.

pub use tower_sessions::{Session, SessionManagerLayer};

use larust_core::AppError;
use sqlx::SqlitePool;
use tower_sessions::session_store::ExpiredDeletion;
use tower_sessions_sqlx_store::SqliteStore;

/// How often the background cleanup task sweeps expired session rows out of
/// the `tower_sessions` table. `tower-sessions` itself already treats an
/// expired session as logged-out on read (expiry is enforced regardless of
/// this task), so this only bounds how long stale rows sit in the table —
/// hourly is frequent enough that the table never grows unboundedly, and
/// infrequent enough not to matter for a single-app SQLite database.
const EXPIRED_SESSION_CLEANUP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3600);

// `Session::cycle_id()` (from `tower_sessions`) is called from
// `larust_auth::guard` on successful login, rotating the session ID to
// prevent session fixation — not this crate's concern, since login itself
// lives in `larust-auth`, but worth knowing it's already wired up if
// you're looking for it.

/// Builds the session layer for a Larust app: a `SqliteStore` over the
/// app's own connection pool (`pool` — typically `larust_support::orm::pool()`,
/// though this crate doesn't depend on `larust-orm` itself, so the caller
/// passes the pool in rather than this function reaching for one).
///
/// Calls `SqliteStore::migrate()` (an idempotent `CREATE TABLE IF NOT
/// EXISTS`) before returning, so the sessions table exists the first time
/// this ever runs — no separate migration file needed in any app's
/// `database/migrations/`, and nothing to add to the app's own
/// `_migrations` bookkeeping table.
///
/// `secure` controls the cookie's `Secure` attribute (`tower-sessions`
/// defaults this to `true`). Browsers only treat loopback addresses and
/// the literal name `localhost` as secure contexts over plain HTTP — a
/// custom local dev hostname (e.g. a `.test` domain resolved via
/// `/etc/hosts`, even one that points at 127.0.0.1) is not on that list,
/// so a `Secure` cookie is silently dropped and sessions/CSRF stop working
/// with no error surfaced anywhere. `Router::with_sessions(pool, secure)`
/// is how callers set this — see `Config::session_secure_cookie` for the
/// `SESSION_SECURE_COOKIE`-env-driven value apps are expected to pass.
///
/// Also spawns a background task that periodically deletes expired session
/// rows (`SqliteStore::continuously_delete_expired`, every
/// `EXPIRED_SESSION_CLEANUP_INTERVAL`) — a persistent store means expired
/// sessions actually accumulate in the table over time, unlike the old
/// in-memory store where every session vanished on its own at the next
/// restart regardless. This doesn't affect *expiry* itself (`tower-sessions`
/// already treats an expired session as logged-out the moment it's read,
/// with or without this task) — only how long stale rows linger.
pub async fn sqlite_session_layer(
    pool: &SqlitePool,
    secure: bool,
) -> Result<SessionManagerLayer<SqliteStore>, AppError> {
    let store = SqliteStore::new(pool.clone());
    store
        .migrate()
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    let cleanup_store = store.clone();
    tokio::spawn(async move {
        if let Err(error) = cleanup_store
            .continuously_delete_expired(EXPIRED_SESSION_CLEANUP_INTERVAL)
            .await
        {
            tracing::error!(%error, "expired session cleanup task stopped unexpectedly");
        }
    });

    Ok(SessionManagerLayer::new(store).with_secure(secure))
}
