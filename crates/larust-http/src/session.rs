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

    // `tower_sessions` defaults the cookie name to a bare `"id"`, with no
    // domain/port scoping — and browsers scope cookies by host+path only,
    // never by port (RFC 6265), so two *different* Larust apps both
    // running on `localhost`/`127.0.0.1` (any ports) would silently share
    // one browser-side cookie slot: logging into one overwrites the other
    // app's session cookie out from under it, and since the CSRF token is
    // itself stored in the session (see `csrf.rs`), that surfaces as a
    // CSRF mismatch rather than a plain logout. Naming the cookie after
    // `Config::app_name` (already the same value `channel_address` keys
    // the `xr dev`/`xr restart` admin channel by, so it's already the
    // thing that's supposed to distinguish one app from another on this
    // machine) keeps every app's session cookie in its own slot.
    Ok(SessionManagerLayer::new(store)
        .with_secure(secure)
        .with_name(cookie_name()))
}

/// This app's own session cookie name, derived from `Config::app_name` —
/// public so `larust_testing::TestClient::acting_as` can adopt a session by
/// hand-crafting the exact same `Cookie` header the router's own session
/// layer (above) would issue, without needing a real `/login` round trip.
/// Must stay the single source of truth for the name: any second place
/// that re-derives it independently risks drifting out of sync with
/// whatever `sqlite_session_layer` actually configured.
///
/// Uses `larust_core::try_config()`, not `config()` — a handful of narrow
/// router-building test helpers (see e.g.
/// `examples/blog/tests/store_post_test.rs`) build a session-bearing
/// router directly off a bare `SqlitePool`, with no `Application::new()`
/// call anywhere in the test at all, since nothing else they exercise
/// needs one. Falling back to a fixed name in that case (rather than
/// panicking) keeps this a purely additive change — every real app
/// (`main.rs` always calls `Application::new()` first) still gets a
/// properly app-scoped cookie name; a test with no `Application` just
/// gets a stable shared one instead, which is harmless since nothing
/// about in-process `oneshot()`-driven tests risks the actual cross-app
/// cookie collision this scoping exists to prevent in a real browser.
pub fn cookie_name() -> String {
    let app_name = larust_core::try_config()
        .map(|config| config.app_name.as_str())
        .unwrap_or("app");
    session_cookie_name(app_name)
}

/// Same ASCII-alphanumeric-or-underscore sanitization as
/// `larust_core::lifecycle::admin::channel_address` — reused by name/shape,
/// not by call, since that helper lives in a different crate and produces a
/// pipe/socket-address string, not a cookie-token-safe one; a cookie name
/// has the same "alphanumeric plus a few symbols" constraint an admin
/// channel address does, so the same replace-anything-unsafe approach fits.
fn session_cookie_name(app_name: &str) -> String {
    let safe: String = app_name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("larust_{safe}_session")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_name_is_derived_from_the_app_name() {
        assert_eq!(session_cookie_name("blog"), "larust_blog_session");
    }

    #[test]
    fn cookie_name_sanitizes_characters_a_cookie_token_can_t_contain() {
        assert_eq!(
            session_cookie_name("My App! 2.0"),
            "larust_My_App__2_0_session"
        );
    }

    #[test]
    fn different_app_names_never_collide_on_the_same_cookie_name() {
        assert_ne!(session_cookie_name("Larust"), session_cookie_name("blog"));
    }
}
