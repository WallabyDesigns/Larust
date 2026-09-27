//! Cookie-based sessions over `tower-sessions` (a real, maintained session
//! engine - the cookie carries only an opaque session ID, actual data
//! lives server-side in a `Store`, which is the standard, more secure
//! pattern rather than shipping session contents in the cookie itself).
//!
//! Two real backends, chosen by [`Config::session_driver`](larust_core::Config::session_driver)
//! (`SESSION_DRIVER` - `"database"`, the default, or `"file"`), both
//! wrapped in one [`SessionBackend`] enum so `session_layer` always
//! returns the same concrete `SessionManagerLayer<SessionBackend>` type
//! regardless of which one is active:
//!
//! - [`AnySessionStore`]: a small hand-written `tower_sessions::
//!   SessionStore` implementation over `sqlx::AnyPool` - not a third-party
//!   per-backend store crate (`tower-sessions-sqlx-store`, say). That
//!   crate's `SqliteStore`/`MySqlStore` each need their own concretely-
//!   typed `SqlitePool`/`MySqlPool`, but `larust_orm::pool()` hands out a
//!   runtime-generic `AnyPool` (see `larust_orm::Backend`) with no way to
//!   recover a concrete pool from it - so a store built directly against
//!   `AnyPool`, branching its own SQL by [`larust_orm::backend`] the same
//!   way every other framework crate with its own table does
//!   (`larust-permissions`, `larust-queue`, ...), is both the only real
//!   option and the one consistent with how the rest of this framework
//!   already handles the two backends.
//! - [`FileSessionStore`]: one file per session under `storage/sessions/` -
//!   Laravel's own `SESSION_DRIVER=file` equivalent, for an app that would
//!   rather not put session churn through its own database at all
//!   (especially now that `docs/GOTCHAS.md` documents a real production
//!   incident SQLite-backed sessions caused under write-heavy load). Only
//!   works for a single server - every process sharing this store needs to
//!   see the same directory, which a real multi-server deployment behind a
//!   load balancer can't guarantee the way a shared database can.
//!
//! Neither is an in-memory store: session data needs to survive a process
//! restart (a deploy, a crash, `xr dev`'s rebuild-and-restart cycle), not
//! just live for the lifetime of one process. There's deliberately no
//! in-memory option in this crate's public API at all: an in-memory store
//! is a real, common trap (an app that "works" in every manual test, then
//! silently logs everyone out on every deploy) - same shape as Laravel's
//! own `array` session driver being the wrong thing to ship to production.
//! `"file"` doesn't have that problem - real files on disk survive a
//! restart/crash exactly the way a database row does - which is why it's
//! offered even though `array` deliberately isn't.

pub use tower_sessions::{Session, SessionManagerLayer};

use async_trait::async_trait;
use larust_core::AppError;
use larust_orm::Backend;
use sqlx::AnyPool;
use std::io;
use std::path::{Path, PathBuf};
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

/// Reads and clears a "flash" value - like `session.remove(key)`, but only
/// actually calls it when `key` was present. Prefer this for a flash-
/// message read that runs on every page view whether or not one was
/// queued (the common Laravel-style "check for a flashed success/error
/// message on every response" pattern - see `demo/app/Http/Controllers/
/// post_controller.rs`'s `index` for a real example).
///
/// The difference matters: `tower_sessions::Session::remove` marks the
/// session modified unconditionally - confirmed by reading
/// `tower-sessions-core`'s own `remove_value`, which flips its internal
/// `is_modified` flag *before* even checking whether the underlying map
/// had the key at all. A flash-message check is exactly the case where
/// the key is usually *absent* (most page views have nothing flashed), so
/// a bare `.remove()` in a handler that runs on every request writes to
/// the session store on every single request, not just the ones that
/// actually redirected in with a message. Under enough traffic this is a
/// real, reproduced production issue, not a theoretical one - see
/// `docs/GOTCHAS.md`.
pub async fn take<T: serde::de::DeserializeOwned>(
    session: &Session,
    key: &str,
) -> Result<Option<T>, tower_sessions::session::Error> {
    if session.get_value(key).await?.is_none() {
        return Ok(None);
    }
    session.remove(key).await
}

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

/// Bounds `create`'s collision-retry loop. `Id` is a cryptographically
/// random 128-bit value - a real collision is astronomically unlikely, so
/// this is purely a safety backstop against a pathological RNG/DB state,
/// never expected to actually bite in practice.
const MAX_ID_COLLISION_RETRIES: u8 = 5;

#[async_trait]
impl SessionStore for AnySessionStore {
    /// A real `INSERT` (not `save`'s upsert) so a genuine `Id` collision is
    /// detected and retried with a fresh ID, rather than silently
    /// overwriting the other session's row - the gap the default
    /// `create`-via-`save` implementation has (see its own doc comment).
    async fn create(&self, record: &mut Record) -> session_store::Result<()> {
        let insert_sql = match larust_orm::backend() {
            Backend::Sqlite | Backend::MySql => {
                "INSERT INTO sessions (id, data, expiry_at) VALUES (?, ?, ?)"
            }
            Backend::Postgres => "INSERT INTO sessions (id, data, expiry_at) VALUES ($1, $2, $3)",
        };

        for _ in 0..MAX_ID_COLLISION_RETRIES {
            let data = encode(record)?;
            let expiry_at = record.expiry_date.unix_timestamp();
            match sqlx::query(insert_sql)
                .bind(record.id.to_string())
                .bind(data)
                .bind(expiry_at)
                .execute(&self.pool)
                .await
            {
                Ok(_) => return Ok(()),
                Err(source) => {
                    let is_collision = source
                        .as_database_error()
                        .is_some_and(|e| e.is_unique_violation());
                    if !is_collision {
                        return Err(backend_error(source));
                    }
                    record.id = Id::default();
                }
            }
        }

        Err(session_store::Error::Backend(format!(
            "couldn't create a session after {MAX_ID_COLLISION_RETRIES} ID collisions in a row"
        )))
    }

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

fn io_error(source: io::Error) -> session_store::Error {
    session_store::Error::Backend(source.to_string())
}

/// A `tower_sessions::SessionStore` backed by plain files on disk, one per
/// session - see this module's own doc comment for when this is used
/// instead of [`AnySessionStore`]. Each session's `Record` is JSON-encoded
/// (the same [`encode`]/[`decode`] helpers `AnySessionStore` itself uses)
/// into a file named after [`Id`]'s own rendering.
#[derive(Clone, Debug)]
pub struct FileSessionStore {
    dir: PathBuf,
}

impl FileSessionStore {
    /// Public so `larust_testing::TestClient::acting_as` can build one
    /// directly over the same directory the router's own session layer
    /// uses, matching `AnySessionStore::new`'s identical role for the
    /// database-backed store.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Called once from [`session_layer`], before the layer is ever
    /// handed a request - the file-store equivalent of `AnySessionStore::
    /// migrate`'s idempotent `CREATE TABLE IF NOT EXISTS`.
    async fn ensure_dir(&self) -> io::Result<()> {
        tokio::fs::create_dir_all(&self.dir).await
    }

    /// `Id`'s own `Display` always renders a fixed 22-character URL-safe-
    /// base64 string (`A-Za-z0-9-_` only, confirmed by reading `tower-
    /// sessions-core`'s own `Id` impl - the identical fact `AnySessionStore`'s
    /// SQL binding already relies on) - safe as a bare filename on every
    /// platform this framework targets, with no path-traversal risk, since
    /// it's never derived from anything a client actually controls. Never
    /// has a file extension, deliberately - see [`is_session_file`] below.
    fn path_for(&self, id: &Id) -> PathBuf {
        self.dir.join(id.to_string())
    }

    /// Writes `data` to `path` without ever letting a concurrent reader see
    /// a half-written file: write to a sibling temp file first, then
    /// `rename` it into place. Atomic and overwrite-safe on every platform
    /// this framework targets - confirmed directly against the real
    /// standard library source, not assumed: `std::fs::rename` on Windows
    /// calls `MoveFileExW(..., MOVEFILE_REPLACE_EXISTING)`
    /// (`library/std/src/sys/fs/windows.rs`), and a plain `rename(2)` on
    /// Unix is already atomic and overwrite-safe by POSIX definition - so
    /// any concurrent reader always sees either the complete old content or
    /// the complete new content, never a torn write.
    ///
    /// The temp filename includes this process's own PID plus a monotonic
    /// per-process counter, not a fixed suffix - two callers racing to save
    /// the *same* session concurrently (a double-submitted form, or two
    /// Larust generations briefly coexisting during a zero-downtime restart
    /// handoff, both sharing this same directory on disk) must never share
    /// one temp path, or one's write could land in the middle of the
    /// other's in-flight rename.
    async fn write_atomically(path: &Path, data: &[u8]) -> io::Result<()> {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp_path = path.with_extension(format!("tmp.{}.{n}", std::process::id()));
        tokio::fs::write(&tmp_path, data).await?;
        tokio::fs::rename(&tmp_path, path).await
    }
}

/// A real session file, named after `Id`'s own rendering, never has a file
/// extension - `write_atomically`'s own temp files always do
/// (`<id>.tmp.<pid>.<n>`). `delete_expired`'s directory scan uses this to
/// skip anything left behind by a process that crashed mid-write, rather
/// than trying (and failing) to decode it as a session record.
fn is_session_file(path: &Path) -> bool {
    path.extension().is_none()
}

#[async_trait]
impl SessionStore for FileSessionStore {
    /// A real "fail if it already exists" create (not `save`'s upsert), the
    /// file-store equivalent of `AnySessionStore::create`'s real `INSERT` -
    /// see that method's own doc comment for why detecting a genuine `Id`
    /// collision, rather than silently overwriting the other session, is
    /// the whole reason `create` exists separately from `save` at all.
    /// `create_new(true)` atomically fails with `AlreadyExists` if the
    /// path is already taken, with no separate existence check to race
    /// against. No temp-file dance needed here, unlike `save`/
    /// `write_atomically`: a brand-new file has no previous version a
    /// concurrent reader could see a torn mix of.
    async fn create(&self, record: &mut Record) -> session_store::Result<()> {
        use tokio::io::AsyncWriteExt;

        for _ in 0..MAX_ID_COLLISION_RETRIES {
            let path = self.path_for(&record.id);
            let data = encode(record)?;
            match tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .await
            {
                Ok(mut file) => {
                    file.write_all(data.as_bytes()).await.map_err(io_error)?;
                    return Ok(());
                }
                Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
                    record.id = Id::default();
                }
                Err(source) => return Err(io_error(source)),
            }
        }

        Err(session_store::Error::Backend(format!(
            "couldn't create a session after {MAX_ID_COLLISION_RETRIES} ID collisions in a row"
        )))
    }

    async fn save(&self, record: &Record) -> session_store::Result<()> {
        let path = self.path_for(&record.id);
        let data = encode(record)?;
        Self::write_atomically(&path, data.as_bytes())
            .await
            .map_err(io_error)
    }

    async fn load(&self, session_id: &Id) -> session_store::Result<Option<Record>> {
        match tokio::fs::read_to_string(self.path_for(session_id)).await {
            Ok(data) => {
                let record = decode(&data)?;
                // Matches `AnySessionStore::load`'s own `WHERE expiry_at >
                // now` filter exactly: an expired session is treated as
                // absent on read, but its file is left on disk for
                // `delete_expired`'s periodic sweep to remove later, not
                // deleted eagerly here.
                if record.expiry_date.unix_timestamp() > now_unix_secs() {
                    Ok(Some(record))
                } else {
                    Ok(None)
                }
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(io_error(source)),
        }
    }

    async fn delete(&self, session_id: &Id) -> session_store::Result<()> {
        match tokio::fs::remove_file(self.path_for(session_id)).await {
            Ok(()) => Ok(()),
            // Matches `AnySessionStore::delete`'s own idempotent shape - a
            // `DELETE FROM sessions WHERE id = ?` that matched no row is
            // just as much a "success" as one that did.
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(io_error(source)),
        }
    }
}

#[async_trait]
impl ExpiredDeletion for FileSessionStore {
    async fn delete_expired(&self) -> session_store::Result<()> {
        let mut entries = match tokio::fs::read_dir(&self.dir).await {
            Ok(entries) => entries,
            // Nothing has ever been saved yet - nothing to clean up, not
            // an error.
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(source) => return Err(io_error(source)),
        };

        let now = now_unix_secs();
        while let Some(entry) = entries.next_entry().await.map_err(io_error)? {
            let path = entry.path();
            if !is_session_file(&path) {
                continue;
            }
            // Best-effort, matching `RotatingFile::write`'s own "never
            // fail the caller over a housekeeping concern" precedent: a
            // file that fails to read or decode (a concurrent `delete`
            // already removed it between `read_dir` listing it and this
            // loop reaching it, or a stray non-session file) is skipped
            // rather than aborting the rest of the sweep.
            let Ok(data) = tokio::fs::read_to_string(&path).await else {
                continue;
            };
            let Ok(record) = decode(&data) else {
                continue;
            };
            if record.expiry_date.unix_timestamp() <= now {
                let _ = tokio::fs::remove_file(&path).await;
            }
        }
        Ok(())
    }
}

/// Wraps whichever of [`AnySessionStore`]/[`FileSessionStore`] `Config::
/// session_driver` selected, so [`session_layer`] always returns the same
/// concrete `SessionManagerLayer<SessionBackend>` type regardless of which
/// one is active - `tower_sessions::SessionManagerLayer<S>` is generic over
/// one concrete `S`, so the alternative to an enum here would be trait-
/// object erasure (`Arc<dyn SessionStore>`), more machinery for no real
/// benefit over a two-variant `match` in each trait method.
#[derive(Clone, Debug)]
pub enum SessionBackend {
    Database(AnySessionStore),
    File(FileSessionStore),
}

#[async_trait]
impl SessionStore for SessionBackend {
    async fn create(&self, record: &mut Record) -> session_store::Result<()> {
        match self {
            Self::Database(store) => store.create(record).await,
            Self::File(store) => store.create(record).await,
        }
    }

    async fn save(&self, record: &Record) -> session_store::Result<()> {
        match self {
            Self::Database(store) => store.save(record).await,
            Self::File(store) => store.save(record).await,
        }
    }

    async fn load(&self, session_id: &Id) -> session_store::Result<Option<Record>> {
        match self {
            Self::Database(store) => store.load(session_id).await,
            Self::File(store) => store.load(session_id).await,
        }
    }

    async fn delete(&self, session_id: &Id) -> session_store::Result<()> {
        match self {
            Self::Database(store) => store.delete(session_id).await,
            Self::File(store) => store.delete(session_id).await,
        }
    }
}

#[async_trait]
impl ExpiredDeletion for SessionBackend {
    async fn delete_expired(&self) -> session_store::Result<()> {
        match self {
            Self::Database(store) => store.delete_expired().await,
            Self::File(store) => store.delete_expired().await,
        }
    }
}

/// Resolves `Config::session_driver` into the matching, constructed (but
/// not yet prepared - see [`session_layer`]'s own `ensure_dir`/`migrate`
/// call for that) [`SessionBackend`] variant. Split out from `session_layer`
/// itself specifically so `larust_testing::TestClient` can build a handle
/// over the *same* backend a router's own session layer resolved to,
/// without duplicating the driver-selection logic (and risking it drifting
/// out of sync) - `TestClient::acting_as` forges a session by writing
/// directly through this handle, bypassing a real `/login` round trip, so
/// it has to agree with the router on where sessions actually live.
/// Doesn't call `ensure_dir`/`migrate` itself: a `TestClient` is always
/// built over a `router` whose own `.with_sessions(...)` call already ran
/// them, the same "someone else already prepared this" assumption the
/// database branch has always relied on (`TestClient::new` never called
/// `AnySessionStore::migrate` either, even before `FileSessionStore`
/// existed).
///
/// Reads `Config::session_driver`/`AppPaths` through `larust_core::
/// try_config()`/`try_paths()`, not `config()`/`paths()` - the same
/// non-panicking fallback [`cookie_name`] already uses, and for the
/// identical reason: a handful of narrow test harnesses build a session-
/// bearing router directly off a bare pool, with no `Application::new()`
/// call anywhere in the test, so nothing has ever published either
/// singleton. Both fall back to `"database"` behavior when unset - not
/// `"file"` - so calling this the way every existing test in this codebase
/// already does keeps behaving identically whether or not an `Application`
/// happened to be constructed first.
pub fn resolve_backend(pool: &AnyPool) -> SessionBackend {
    let driver =
        larust_core::try_config().map_or("database", |config| config.session_driver.as_str());

    match driver {
        "file" => {
            let dir = larust_core::try_paths()
                .map(|paths| paths.sessions())
                .unwrap_or_else(|| PathBuf::from("storage/sessions"));
            SessionBackend::File(FileSessionStore::new(dir))
        }
        // "database", or anything unrecognized - `Application::new()`'s
        // own `warn_if_session_driver_is_unsupported` already warns about
        // the latter case; degrading to the always-safe default here
        // rather than treating a typo'd `SESSION_DRIVER` as fatal matches
        // `logging::init`'s identical "unrecognized `LOG_CHANNEL` degrades
        // to stdout" precedent.
        _ => SessionBackend::Database(AnySessionStore::new(pool.clone())),
    }
}

/// Builds the session layer for a Larust app, over `pool` (typically
/// `larust_support::orm::pool()`) - or, when `Config::session_driver` is
/// `"file"`, over a directory instead, `pool` going unused for this call
/// entirely. Dispatches to whichever of [`AnySessionStore`]/
/// [`FileSessionStore`] is active (see this module's own doc comment),
/// preparing it before returning: the database store's idempotent
/// `CREATE TABLE IF NOT EXISTS`, or the file store's `create_dir_all` -
/// no separate migration file needed in any app's `database/migrations/`
/// either way.
///
/// Reads `Config::session_driver`/`AppPaths` through `larust_core::
/// try_config()`/`try_paths()`, not `config()`/`paths()` - the same
/// non-panicking fallback [`cookie_name`] already uses, and for the
/// identical reason: a handful of narrow test harnesses (see that
/// function's own doc comment) build a session-bearing router directly off
/// a bare pool, with no `Application::new()` call anywhere in the test, so
/// nothing has ever published either singleton. Both fall back to
/// `"database"` behavior when unset - not `"file"` - so calling this the
/// way every existing test in this codebase already does (`session_layer(
/// &pool, secure)`, database-backed) keeps behaving identically whether or
/// not an `Application` happened to be constructed first.
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
/// Also spawns a background task that periodically deletes expired
/// sessions (every `EXPIRED_SESSION_CLEANUP_INTERVAL`) - a persistent
/// store means expired sessions actually accumulate (rows or files) over
/// time, unlike an in-memory store where every session vanishes on its own
/// at the next restart regardless. This doesn't affect *expiry* itself
/// (`tower-sessions` already treats an expired session as logged-out the
/// moment it's read, with or without this task) - only how long stale
/// rows/files linger.
pub async fn session_layer(
    pool: &AnyPool,
    secure: bool,
) -> Result<SessionManagerLayer<SessionBackend>, AppError> {
    let store = resolve_backend(pool);
    match &store {
        SessionBackend::File(file_store) => file_store
            .ensure_dir()
            .await
            .map_err(|source| AppError::Internal(Box::new(source)))?,
        SessionBackend::Database(db_store) => db_store
            .migrate()
            .await
            .map_err(|source| AppError::Internal(Box::new(source)))?,
    }

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

    /// All scenarios share one test function, not several:
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

        // `create` on a genuinely fresh ID just inserts, same as `save`.
        let mut fresh = Record {
            id: Id::default(),
            data: Default::default(),
            expiry_date: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
        };
        fresh
            .data
            .insert("greeting".to_string(), serde_json::json!("fresh"));
        let fresh_id = fresh.id;
        store.create(&mut fresh).await.unwrap();
        assert_eq!(
            fresh.id, fresh_id,
            "a non-colliding create must not change the id"
        );
        let loaded = store.load(&fresh_id).await.unwrap().unwrap();
        assert_eq!(loaded.data["greeting"], serde_json::json!("fresh"));

        // `create` on an *already-taken* id is the real regression guard
        // for this method's whole reason to exist: unlike `save` (an
        // upsert, which would silently overwrite the row above), `create`
        // must detect the collision and regenerate the id rather than
        // clobbering the existing session.
        let mut colliding = Record {
            id: fresh_id,
            data: Default::default(),
            expiry_date: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
        };
        colliding
            .data
            .insert("greeting".to_string(), serde_json::json!("colliding"));
        store.create(&mut colliding).await.unwrap();
        assert_ne!(
            colliding.id, fresh_id,
            "create must regenerate the id on a real collision, not overwrite the existing row"
        );
        // The original session survives untouched.
        let original = store.load(&fresh_id).await.unwrap().unwrap();
        assert_eq!(original.data["greeting"], serde_json::json!("fresh"));
        // The regenerated session was actually created under its new id.
        let new_one = store.load(&colliding.id).await.unwrap().unwrap();
        assert_eq!(new_one.data["greeting"], serde_json::json!("colliding"));

        // `take`'s actual regression guard: a bare `session.remove()` marks
        // the session modified even when the key was never present
        // (confirmed directly against `tower-sessions-core`'s own
        // `remove_value`, not just assumed) - avoiding exactly that
        // phantom-dirty write on a flash-message check with nothing
        // actually flashed is `take`'s whole reason to exist.
        let session = Session::new(None, std::sync::Arc::new(store.clone()), None);
        let absent: Option<String> = take(&session, "success").await.unwrap();
        assert_eq!(absent, None);
        assert!(
            !session.is_modified(),
            "checking for an absent flash key must not mark the session dirty"
        );

        // The companion case: when the key *is* present, `take` must still
        // actually remove it (a flash message read once, not re-shown on
        // the next page) and report the session as modified, same as a
        // bare `remove()` would.
        session.insert("success", "saved!").await.unwrap();
        let value: Option<String> = take(&session, "success").await.unwrap();
        assert_eq!(value.as_deref(), Some("saved!"));
        assert!(session.is_modified());
        let gone: Option<String> = session.get("success").await.unwrap();
        assert_eq!(gone, None);

        // A basic sanity check for the production incident this area got
        // hardened for (see `docs/GOTCHAS.md`): concurrent session writes
        // against SQLite must succeed, not error with `(code: 5) database
        // is locked`. This doesn't reproduce the original failure by
        // itself - on fast local storage with a generous `busy_timeout`,
        // even the old `max_connections(10)` absorbs a burst this size
        // without erroring (confirmed directly: reverting `larust_orm::
        // pool::connect`'s SQLite cap back to 10 and running this same
        // scenario, even at much higher concurrency, still passed here).
        // The original incident needed sustained real traffic, and very
        // likely a slower disk and/or a zero-downtime restart handoff
        // briefly running two processes' pools against the same file at
        // once - none of which a single in-process test can cheaply
        // reproduce. What this test *does* guard is a plain, real
        // regression: that funneling every SQLite access through one
        // connection (`max_connections(1)`, the actual fix) doesn't
        // itself introduce a deadlock or a dropped write under concurrent
        // callers - a legitimate risk worth a real test, even though the
        // specific "thundering herd under load" failure mode itself has to
        // stay a documented-but-unreproduced-in-CI scenario.
        let concurrent_saves: Vec<_> = (0..32)
            .map(|i| {
                let store = store.clone();
                tokio::spawn(async move {
                    let mut record = Record {
                        id: Id::default(),
                        data: Default::default(),
                        expiry_date: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
                    };
                    record.data.insert("i".to_string(), serde_json::json!(i));
                    store.save(&record).await
                })
            })
            .collect();
        for task in concurrent_saves {
            task.await
                .expect("task panicked")
                .expect("concurrent session save must not fail with a locked database");
        }
    }

    /// The file-store equivalent of `any_session_store_behaves_correctly_
    /// across_every_scenario` above - same scenarios, same assertions,
    /// proving `FileSessionStore` honors the identical `SessionStore`
    /// contract `AnySessionStore` does (round-trip, expiry filtering on
    /// read, idempotent delete, and `create`'s real collision detection).
    /// One function, not several, purely for symmetry with its database
    /// counterpart - `FileSessionStore` has no shared-singleton
    /// constraint forcing that shape the way `AnySessionStore`'s tests do,
    /// but splitting it here while the sibling test stays merged would
    /// make the two harder to compare side by side.
    #[tokio::test]
    async fn file_session_store_behaves_correctly_across_every_scenario() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSessionStore::new(dir.path());

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

        // An expired session is not loaded, but its file is left on disk
        // for `delete_expired` to find - not deleted eagerly on read,
        // matching `AnySessionStore::load`'s identical behavior.
        let expired = Record {
            id: Id::default(),
            data: Default::default(),
            expiry_date: time::OffsetDateTime::now_utc() - time::Duration::hours(1),
        };
        store.save(&expired).await.unwrap();
        assert!(store.load(&expired.id).await.unwrap().is_none());
        assert!(
            dir.path().join(expired.id.to_string()).exists(),
            "an expired session's file must still exist until delete_expired sweeps it"
        );

        // delete removes a saved session, and is idempotent - deleting an
        // already-absent session is still `Ok`, matching `AnySessionStore::
        // delete`'s own `DELETE ... WHERE id = ?` (a no-op match is still a
        // successful statement).
        let doomed = Record {
            id: Id::default(),
            data: Default::default(),
            expiry_date: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
        };
        store.save(&doomed).await.unwrap();
        store.delete(&doomed.id).await.unwrap();
        assert!(store.load(&doomed.id).await.unwrap().is_none());
        store.delete(&doomed.id).await.unwrap();

        // `create` on a genuinely fresh ID just inserts, same as `save`.
        let mut fresh = Record {
            id: Id::default(),
            data: Default::default(),
            expiry_date: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
        };
        fresh
            .data
            .insert("greeting".to_string(), serde_json::json!("fresh"));
        let fresh_id = fresh.id;
        store.create(&mut fresh).await.unwrap();
        assert_eq!(
            fresh.id, fresh_id,
            "a non-colliding create must not change the id"
        );
        let loaded = store.load(&fresh_id).await.unwrap().unwrap();
        assert_eq!(loaded.data["greeting"], serde_json::json!("fresh"));

        // `create` on an *already-taken* id is the real regression guard
        // for this method's whole reason to exist - unlike `save` (an
        // upsert, which would silently overwrite the file above), `create`
        // must detect the collision (`create_new(true)`'s `AlreadyExists`)
        // and regenerate the id rather than clobbering the existing
        // session.
        let mut colliding = Record {
            id: fresh_id,
            data: Default::default(),
            expiry_date: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
        };
        colliding
            .data
            .insert("greeting".to_string(), serde_json::json!("colliding"));
        store.create(&mut colliding).await.unwrap();
        assert_ne!(
            colliding.id, fresh_id,
            "create must regenerate the id on a real collision, not overwrite the existing file"
        );
        let original = store.load(&fresh_id).await.unwrap().unwrap();
        assert_eq!(original.data["greeting"], serde_json::json!("fresh"));
        let new_one = store.load(&colliding.id).await.unwrap().unwrap();
        assert_eq!(new_one.data["greeting"], serde_json::json!("colliding"));

        // `delete_expired` removes only the expired file, leaving every
        // still-valid one (and any stray temp file - see below) alone.
        store.delete_expired().await.unwrap();
        assert!(!dir.path().join(expired.id.to_string()).exists());
        assert!(dir.path().join(fresh_id.to_string()).exists());
        assert!(dir.path().join(colliding.id.to_string()).exists());

        // A leftover temp file (the shape a crash mid-`write_atomically`
        // would leave behind) must not crash `delete_expired`'s scan, and
        // must not be mistaken for a session to delete - `is_session_file`'s
        // whole reason to exist.
        tokio::fs::write(dir.path().join("orphaned.tmp.999.1"), b"not json")
            .await
            .unwrap();
        store.delete_expired().await.unwrap();
        assert!(dir.path().join("orphaned.tmp.999.1").exists());

        // Concurrent creates (a burst of genuinely new sessions, e.g. many
        // simultaneous first-time visitors) must all succeed - proving the
        // real, final file's own atomic `create_new(true)` open doesn't
        // trip over the others, the file-store analogue of
        // `AnySessionStore`'s own concurrent-`save` regression guard above.
        let concurrent_creates: Vec<_> = (0..32)
            .map(|i| {
                let store = store.clone();
                tokio::spawn(async move {
                    let mut record = Record {
                        id: Id::default(),
                        data: Default::default(),
                        expiry_date: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
                    };
                    record.data.insert("i".to_string(), serde_json::json!(i));
                    store.create(&mut record).await
                })
            })
            .collect();
        for task in concurrent_creates {
            task.await
                .expect("task panicked")
                .expect("concurrent session create must not fail");
        }
    }
}
