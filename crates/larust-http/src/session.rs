//! Cookie-based sessions over `tower-sessions` (a real, maintained session
//! engine - the cookie carries only an opaque session ID, actual data
//! lives server-side in a `Store`, which is the standard, more secure
//! pattern rather than shipping session contents in the cookie itself).
//!
//! Backed by [`AnySessionStore`] below, a small hand-written
//! `tower_sessions::SessionStore` implementation over `sqlx::AnyPool` -
//! not a third-party per-backend store crate (`tower-sessions-sqlx-store`,
//! say). That crate's `SqliteStore`/`MySqlStore` each need their own
//! concretely-typed `SqlitePool`/`MySqlPool`, but `larust_orm::pool()`
//! hands out a runtime-generic `AnyPool` (see `larust_orm::Backend`) with
//! no way to recover a concrete pool from it - so a store built directly
//! against `AnyPool`, branching its own SQL by [`larust_orm::backend`]
//! the same way every other framework crate with its own table does
//! (`larust-permissions`, `larust-queue`, ...), is both the only real
//! option and the one consistent with how the rest of this framework
//! already handles the two backends.
//!
//! Not an in-memory store: session data needs to survive a process
//! restart (a deploy, a crash, `xr dev`'s rebuild-and-restart cycle), not
//! just live for the lifetime of one process. There's deliberately no
//! in-memory option in this crate's public API: an in-memory store is a
//! real, common trap (an app that "works" in every manual test, then
//! silently logs everyone out on every deploy) - same shape as Laravel's
//! own `array` session driver being the wrong thing to ship to production.

pub use tower_sessions::{Session, SessionManagerLayer};

use async_trait::async_trait;
use larust_core::AppError;
use larust_orm::Backend;
use sqlx::AnyPool;
use tower_sessions::session::{Id, Record};
use tower_sessions::session_store::{self, ExpiredDeletion};
use tower_sessions::SessionStore;

/// How often the background cleanup task sweeps expired session rows out of
/// the sessions table. `tower-sessions` itself already treats an expired
/// session as logged-out on read (expiry is enforced regardless of this
/// task), so this only bounds how long stale rows sit in the table -
/// hourly is frequent enough that the table never grows unboundedly, and
/// infrequent enough not to matter for a single-app database.
const EXPIRED_SESSION_CLEANUP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3600);

// `Session::cycle_id()` (from `tower_sessions`) is called from
// `larust_auth::guard` on successful login, rotating the session ID to
// prevent session fixation - not this crate's concern, since login itself
// lives in `larust-auth`, but worth knowing it's already wired up if
// you're looking for it.

/// A `tower_sessions::SessionStore` over `sqlx::AnyPool` - see this
/// module's own doc comment for why this is hand-written rather than a
/// third-party store crate. The session's own `Record` (id/data/expiry) is
/// stored as one row: `data` as a JSON-serialized `TEXT` column (`serde_json`
/// is already a workspace-wide dependency; no need for a binary encoding
/// crate just for this), `expiry_date` as Unix-epoch seconds - the same
/// "epoch seconds as `INTEGER`" convention every other framework-owned
/// table in this codebase already uses (`larust-cache`'s `cache_items`,
/// `larust-queue`'s `jobs`, `larust-notifications`'s `notifications`), not
/// `tower-sessions-sqlx-store`'s own native-timestamp-column choice.
#[derive(Clone, Debug)]
pub struct AnySessionStore {
    pool: AnyPool,
}

impl AnySessionStore {
    /// Public so `larust_testing::TestClient::acting_as` can build one
    /// directly over the same pool the router's own session layer uses
    /// (via `session_layer`, above), bypassing a real `/login` round trip.
    pub fn new(pool: AnyPool) -> Self {
        Self { pool }
    }

    /// Idempotent `CREATE TABLE IF NOT EXISTS` - called once, from
    /// [`session_layer`], before the layer is ever handed a request.
    async fn migrate(&self) -> Result<(), sqlx::Error> {
        let create_table = match larust_orm::backend() {
            Backend::Sqlite => {
                "CREATE TABLE IF NOT EXISTS sessions (\
                    id TEXT PRIMARY KEY, \
                    data TEXT NOT NULL, \
                    expiry_at INTEGER NOT NULL\
                 )"
            }
            // `id`: a `TEXT`/`BLOB` column needs an explicit key length to
            // be usable as a MySQL key at all - `tower_sessions::session::Id`
            // always renders as a fixed 22-character URL-safe base64
            // string (see its own `Display` impl), so `VARCHAR(32)` is a
            // safe, generous cap.
            //
            // `data`: `VARCHAR`, not MySQL's own `TEXT` - confirmed
            // empirically (a real, live MySQL server, not just reading
            // source) that `sqlx`'s `Any` driver maps *every* MySQL
            // `TEXT`/`TINYTEXT`/`MEDIUMTEXT`/`LONGTEXT` column to its own
            // generic `Blob` kind, unconditionally, regardless of the
            // column's actual charset (`sqlx-mysql`'s `Any` adapter keys
            // off the wire-protocol `ColumnType` alone, which doesn't
            // distinguish TEXT from BLOB the way the column's real
            // charset does) - and `Decode<Any> for String` only ever
            // accepts `Text`-kind values, so decoding a MySQL `TEXT`
            // column as `String` through `Any` fails outright ("Rust type
            // `String` is not compatible with SQL type `BLOB`"). Only
            // `CHAR`/`VARCHAR` map to `Any`'s `Text` kind. `VARCHAR(4000)`
            // (the largest that comfortably fits one `utf8mb4` row
            // alongside this table's other columns) is a practical,
            // generous cap for session data specifically - nowhere near
            // enough for arbitrary large content, but session payloads
            // are small structured data (auth id, CSRF token, a handful
            // of flash values), never user-uploaded content.
            Backend::MySql => {
                "CREATE TABLE IF NOT EXISTS sessions (\
                    id VARCHAR(32) PRIMARY KEY, \
                    data VARCHAR(4000) NOT NULL, \
                    expiry_at INTEGER NOT NULL\
                 )"
            }
            // Postgres has native, unbounded `TEXT` and no MySQL-style key-
            // length requirement - same shape as SQLite's own arm.
            Backend::Postgres => {
                "CREATE TABLE IF NOT EXISTS sessions (\
                    id TEXT PRIMARY KEY, \
                    data TEXT NOT NULL, \
                    expiry_at INTEGER NOT NULL\
                 )"
            }
        };
        sqlx::query(create_table).execute(&self.pool).await?;
        Ok(())
    }
}

fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_secs() as i64
}

fn backend_error(source: sqlx::Error) -> session_store::Error {
    session_store::Error::Backend(source.to_string())
}

fn encode(record: &Record) -> session_store::Result<String> {
    serde_json::to_string(record).map_err(|source| session_store::Error::Encode(source.to_string()))
}

fn decode(data: &str) -> session_store::Result<Record> {
    serde_json::from_str(data).map_err(|source| session_store::Error::Decode(source.to_string()))
}

#[async_trait]
impl SessionStore for AnySessionStore {
    // No custom `create` override - the default implementation (calling
    // `save`, which upserts) is safe here: `Id` is a cryptographically
    // random 128-bit value, making a real collision astronomically
    // unlikely, and the crate's own default-impl doc comment agrees this
    // is a reasonable simplification for a store that doesn't need to
    // treat a collision as a hard error.

    async fn save(&self, record: &Record) -> session_store::Result<()> {
        let data = encode(record)?;
        let expiry_at = record.expiry_date.unix_timestamp();
        let upsert_sql = match larust_orm::backend() {
            Backend::Sqlite => {
                "INSERT INTO sessions (id, data, expiry_at) VALUES (?, ?, ?) \
                 ON CONFLICT(id) DO UPDATE SET data = excluded.data, expiry_at = excluded.expiry_at"
            }
            Backend::MySql => {
                "INSERT INTO sessions (id, data, expiry_at) VALUES (?, ?, ?) \
                 ON DUPLICATE KEY UPDATE data = VALUES(data), expiry_at = VALUES(expiry_at)"
            }
            Backend::Postgres => {
                "INSERT INTO sessions (id, data, expiry_at) VALUES ($1, $2, $3) \
                 ON CONFLICT(id) DO UPDATE SET data = excluded.data, expiry_at = excluded.expiry_at"
            }
        };
        sqlx::query(upsert_sql)
            .bind(record.id.to_string())
            .bind(data)
            .bind(expiry_at)
            .execute(&self.pool)
            .await
            .map_err(backend_error)?;
        Ok(())
    }

    async fn load(&self, session_id: &Id) -> session_store::Result<Option<Record>> {
        let backend = larust_orm::backend();
        let sql = format!(
            "SELECT data FROM sessions WHERE id = {} AND expiry_at > {}",
            larust_orm::placeholder(backend, 1),
            larust_orm::placeholder(backend, 2),
        );
        let row: Option<(String,)> = sqlx::query_as(&sql)
            .bind(session_id.to_string())
            .bind(now_unix_secs())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend_error)?;

        row.map(|(data,)| decode(&data)).transpose()
    }

    async fn delete(&self, session_id: &Id) -> session_store::Result<()> {
        let sql = format!(
            "DELETE FROM sessions WHERE id = {}",
            larust_orm::placeholder(larust_orm::backend(), 1)
        );
        sqlx::query(&sql)
            .bind(session_id.to_string())
            .execute(&self.pool)
            .await
            .map_err(backend_error)?;
        Ok(())
    }
}

#[async_trait]
impl ExpiredDeletion for AnySessionStore {
    async fn delete_expired(&self) -> session_store::Result<()> {
        let sql = format!(
            "DELETE FROM sessions WHERE expiry_at <= {}",
            larust_orm::placeholder(larust_orm::backend(), 1)
        );
        sqlx::query(&sql)
            .bind(now_unix_secs())
            .execute(&self.pool)
            .await
            .map_err(backend_error)?;
        Ok(())
    }
}

/// Builds the session layer for a Larust app, over `pool` (typically
/// `larust_support::orm::pool()`). Migrates the sessions table (idempotent
/// `CREATE TABLE IF NOT EXISTS`) before returning, so it exists the first
/// time this ever runs - no separate migration file needed in any app's
/// `database/migrations/`, and nothing to add to the app's own
/// `_migrations` bookkeeping table.
///
/// `secure` controls the cookie's `Secure` attribute (`tower-sessions`
/// defaults this to `true`). Browsers only treat loopback addresses and
/// the literal name `localhost` as secure contexts over plain HTTP - a
/// custom local dev hostname (e.g. a `.test` domain resolved via
/// `/etc/hosts`, even one that points at 127.0.0.1) is not on that list,
/// so a `Secure` cookie is silently dropped and sessions/CSRF stop working
/// with no error surfaced anywhere. `Router::with_sessions(pool, secure)`
/// is how callers set this - see `Config::session_secure_cookie` for the
/// `SESSION_SECURE_COOKIE`-env-driven value apps are expected to pass.
///
/// Also spawns a background task that periodically deletes expired session
/// rows (`AnySessionStore::continuously_delete_expired`, every
/// `EXPIRED_SESSION_CLEANUP_INTERVAL`) - a persistent store means expired
/// sessions actually accumulate in the table over time, unlike an
/// in-memory store where every session vanishes on its own at the next
/// restart regardless. This doesn't affect *expiry* itself (`tower-sessions`
/// already treats an expired session as logged-out the moment it's read,
/// with or without this task) - only how long stale rows linger.
pub async fn session_layer(
    pool: &AnyPool,
    secure: bool,
) -> Result<SessionManagerLayer<AnySessionStore>, AppError> {
    let store = AnySessionStore::new(pool.clone());
    store
        .migrate()
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    let cleanup_store = store.clone();
    tokio::spawn(async move {
        // Hand-rolled rather than `ExpiredDeletion::continuously_delete_expired`
        // (the same loop that method runs) - same effect, one fewer trait
        // import to reason about.
        let mut interval = tokio::time::interval(EXPIRED_SESSION_CLEANUP_INTERVAL);
        interval.tick().await; // first tick completes immediately; skip it
        loop {
            interval.tick().await;
            if let Err(error) = cleanup_store.delete_expired().await {
                tracing::error!(%error, "expired session cleanup task stopped unexpectedly");
                break;
            }
        }
    });

    // `tower_sessions` defaults the cookie name to a bare `"id"`, with no
    // domain/port scoping - and browsers scope cookies by host+path only,
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

/// This app's own session cookie name, derived from `Config::app_name` -
/// public so `larust_testing::TestClient::acting_as` can adopt a session by
/// hand-crafting the exact same `Cookie` header the router's own session
/// layer (above) would issue, without needing a real `/login` round trip.
/// Must stay the single source of truth for the name: any second place
/// that re-derives it independently risks drifting out of sync with
/// whatever `session_layer` actually configured.
///
/// Uses `larust_core::try_config()`, not `config()` - a handful of narrow
/// router-building test helpers (see e.g.
/// `examples/blog/tests/store_post_test.rs`) build a session-bearing
/// router directly off a bare pool, with no `Application::new()` call
/// anywhere in the test at all, since nothing else they exercise needs
/// one. Falling back to a fixed name in that case (rather than panicking)
/// keeps this a purely additive change - every real app (`main.rs` always
/// calls `Application::new()` first) still gets a properly app-scoped
/// cookie name; a test with no `Application` just gets a stable shared one
/// instead, which is harmless since nothing about in-process `oneshot()`-
/// driven tests risks the actual cross-app cookie collision this scoping
/// exists to prevent in a real browser.
pub fn cookie_name() -> String {
    let app_name = larust_core::try_config()
        .map(|config| config.app_name.as_str())
        .unwrap_or("app");
    session_cookie_name(app_name)
}

/// Same ASCII-alphanumeric-or-underscore sanitization as
/// `larust_core::lifecycle::admin::channel_address` - reused by name/shape,
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

    async fn connect_test_db() -> AnyPool {
        let dir = tempfile::tempdir().unwrap().keep();
        let database_url = format!("sqlite://{}/test.sqlite", dir.display());
        larust_orm::connect(&database_url).await.unwrap();
        larust_orm::pool().unwrap().clone()
    }

    /// All three scenarios share one test function, not several:
    /// `larust_orm::connect()` sets a process-wide pool exactly once (a
    /// second call in the same test binary errors), the same
    /// singleton-per-process constraint this codebase's other test suites
    /// (`larust-notifications`, `larust-permissions`, `larust-queue`)
    /// already document and work around.
    #[tokio::test]
    async fn any_session_store_behaves_correctly_across_every_scenario() {
        let pool = connect_test_db().await;
        let store = AnySessionStore::new(pool);
        store.migrate().await.unwrap();

        // A saved session round-trips through load.
        let mut record = Record {
            id: Id::default(),
            data: Default::default(),
            expiry_date: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
        };
        record
            .data
            .insert("greeting".to_string(), serde_json::json!("hi"));
        store.save(&record).await.unwrap();
        let loaded = store.load(&record.id).await.unwrap().unwrap();
        assert_eq!(loaded.data["greeting"], serde_json::json!("hi"));

        // An expired session is not loaded.
        let expired = Record {
            id: Id::default(),
            data: Default::default(),
            expiry_date: time::OffsetDateTime::now_utc() - time::Duration::hours(1),
        };
        store.save(&expired).await.unwrap();
        assert!(store.load(&expired.id).await.unwrap().is_none());

        // delete removes a saved session.
        let doomed = Record {
            id: Id::default(),
            data: Default::default(),
            expiry_date: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
        };
        store.save(&doomed).await.unwrap();
        store.delete(&doomed.id).await.unwrap();
        assert!(store.load(&doomed.id).await.unwrap().is_none());
    }
}
