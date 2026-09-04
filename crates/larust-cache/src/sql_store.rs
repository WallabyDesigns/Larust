//! The `"database"` (default) `cache_driver` implementation - SQL-family
//! storage over `larust_orm`'s `AnyPool`, unchanged from before Redis
//! support existed. `store.rs` dispatches to this module or
//! [`crate::redis_store`] based on `Config::cache_driver`; nothing here
//! knows Redis exists.

use larust_core::AppError;
use larust_orm::Backend;
use serde::de::DeserializeOwned;
use serde::Serialize;
use sqlx::AnyPool;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::OnceCell;

/// Set once per process, the first time any cache function runs (same
/// `OnceCell::const_new()` + `get_or_try_init` idiom as
/// `larust_testing::db::TEST_DB`) - no separate `xr` migration file needed.
/// A step further than either existing self-bootstrapping table in this
/// codebase: `larust_orm::migrate::run`'s own `CREATE TABLE IF NOT EXISTS
/// _migrations` and `larust_http::session::AnySessionStore::migrate()` are
/// both unconditional *once invoked*, but each still needs one explicit
/// call at startup/wiring time (`main.rs`'s `migrate` subcommand;
/// `Router::with_sessions()`). This table has no such call anywhere -
/// bootstrap happens lazily, inside every public function here, memoized
/// by this `OnceCell` after the first hit.
static TABLE_READY: OnceCell<()> = OnceCell::const_new();
static LAST_EXPIRY_SWEEP: AtomicI64 = AtomicI64::new(0);
const EXPIRY_SWEEP_INTERVAL_SECS: i64 = 300;

async fn ensure_table(pool: &AnyPool) -> Result<(), AppError> {
    TABLE_READY
        .get_or_try_init(|| async {
            let create_table = match larust_orm::backend() {
                Backend::Sqlite => {
                    "CREATE TABLE IF NOT EXISTS cache_items (\
                        \"key\" TEXT PRIMARY KEY, \
                        value TEXT NOT NULL, \
                        expires_at INTEGER NOT NULL\
                     )"
                }
                // `key`: a `TEXT`/`BLOB` column needs an explicit key
                // length to be usable as a MySQL key at all ("BLOB/TEXT
                // column used in key specification without a key length")
                // - `VARCHAR(255)` is a reasonable cap for a cache key.
                //
                // `value`: `VARCHAR`, not MySQL's own `TEXT` - confirmed
                // empirically (a real, live MySQL server) that `sqlx`'s
                // `Any` driver maps every MySQL `TEXT`-family column to
                // its own generic `Blob` kind unconditionally, and
                // `Decode<Any> for String` only accepts `Text`-kind
                // values - so a `TEXT` column here would fail to decode
                // back as `String` at all ("Rust type `String` is not
                // compatible with SQL type `BLOB`"; only `CHAR`/`VARCHAR`
                // map to `Any`'s `Text` kind). `VARCHAR(4000)` is a real,
                // documented trade-off here specifically (unlike session
                // data, a cached value could legitimately be large) -
                // this crate has no size-cap concept today regardless, so
                // this doesn't newly introduce one so much as make an
                // existing "how big can this get" question concrete for
                // MySQL. A future fix, if this cap proves too small in
                // practice, needs `larust_orm` to expose a way to decode
                // a MySQL `TEXT` column despite the `Any` driver's own
                // gap here (see that crate's own `QueryBuilder` doc
                // comment for the same class of `Any`-driver limitation
                // already documented for `bool`).
                Backend::MySql => {
                    "CREATE TABLE IF NOT EXISTS cache_items (\
                        \"key\" VARCHAR(255) PRIMARY KEY, \
                        value VARCHAR(4000) NOT NULL, \
                        expires_at INTEGER NOT NULL\
                     )"
                }
                // Postgres has native, unbounded `TEXT` and no MySQL-style
                // "BLOB/TEXT column used in key spec" key-length
                // requirement, so this needs neither of MySQL's two
                // workarounds above - same shape as SQLite's own arm.
                Backend::Postgres => {
                    "CREATE TABLE IF NOT EXISTS cache_items (\
                        \"key\" TEXT PRIMARY KEY, \
                        value TEXT NOT NULL, \
                        expires_at INTEGER NOT NULL\
                     )"
                }
            };
            sqlx::query(create_table)
                .execute(pool)
                .await
                .map_err(|source| AppError::Internal(Box::new(source)))?;
            match larust_orm::backend() {
                // Postgres, like SQLite, supports `IF NOT EXISTS` on
                // `CREATE INDEX` (unlike MySQL) - same statement shape.
                Backend::Sqlite | Backend::Postgres => {
                    sqlx::query(
                        "CREATE INDEX IF NOT EXISTS idx_cache_items_expires_at ON cache_items(expires_at)",
                    )
                    .execute(pool)
                    .await
                    .map_err(|source| AppError::Internal(Box::new(source)))?;
                }
                Backend::MySql => {
                    if let Err(error) = sqlx::query(
                        "CREATE INDEX idx_cache_items_expires_at ON cache_items(expires_at)",
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
            Ok(())
        })
        .await?;
    Ok(())
}

fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_secs() as i64
}

/// Bounds expiry cleanup work to once every five minutes across the process,
/// instead of letting expired rows accumulate forever until their exact key is
/// requested again.
async fn sweep_expired_if_due(pool: &AnyPool) {
    let now = now_unix_secs();
    let previous = LAST_EXPIRY_SWEEP.load(Ordering::Relaxed);
    if now - previous < EXPIRY_SWEEP_INTERVAL_SECS
        || LAST_EXPIRY_SWEEP
            .compare_exchange(previous, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
    {
        return;
    }
    let sql = format!(
        "DELETE FROM cache_items WHERE expires_at <= {}",
        larust_orm::placeholder(larust_orm::backend(), 1)
    );
    if let Err(error) = sqlx::query(&sql).bind(now).execute(pool).await {
        tracing::warn!(%error, "failed to sweep expired cache entries");
    }
}

/// Stores `value` under `key`, serialized as JSON, expiring after `ttl`.
/// Overwrites any existing entry under the same key (Laravel's own `put()`
/// semantics - not an error to reuse a key).
pub(crate) async fn put<T: Serialize>(key: &str, value: &T, ttl: Duration) -> Result<(), AppError> {
    let pool = larust_orm::pool()?;
    ensure_table(pool).await?;
    sweep_expired_if_due(pool).await;

    let json =
        serde_json::to_string(value).map_err(|source| AppError::Internal(Box::new(source)))?;
    let expires_at = now_unix_secs() + ttl.as_secs() as i64;

    let upsert_sql = match larust_orm::backend() {
        Backend::Sqlite => {
            "INSERT INTO cache_items (\"key\", value, expires_at) VALUES (?, ?, ?) \
             ON CONFLICT(\"key\") DO UPDATE SET value = excluded.value, expires_at = excluded.expires_at"
        }
        Backend::MySql => {
            "INSERT INTO cache_items (\"key\", value, expires_at) VALUES (?, ?, ?) \
             ON DUPLICATE KEY UPDATE value = VALUES(value), expires_at = VALUES(expires_at)"
        }
        Backend::Postgres => {
            "INSERT INTO cache_items (\"key\", value, expires_at) VALUES ($1, $2, $3) \
             ON CONFLICT(\"key\") DO UPDATE SET value = excluded.value, expires_at = excluded.expires_at"
        }
    };
    sqlx::query(upsert_sql)
        .bind(key)
        .bind(json)
        .bind(expires_at)
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    Ok(())
}

/// Returns `Ok(None)` for a missing or expired key (an ordinary cache
/// miss). A key that exists but whose stored JSON can't be coerced into
/// `T` - e.g. reading a key back with an incompatible type than it was
/// `put` with - is a caller bug, not a miss, so it surfaces as
/// `Err(AppError::Internal)` rather than silently degrading to `None` the
/// way Laravel's own cache would. This detection is best-effort, not a
/// guarantee: a same-shaped-but-different type (e.g. reading an `i64` back
/// as `serde_json::Value`, or as a different numeric type JSON can still
/// coerce) can "succeed" with no error, since there's no stored type tag
/// to check against, only the JSON's own shape.
pub(crate) async fn get<T: DeserializeOwned>(key: &str) -> Result<Option<T>, AppError> {
    let pool = larust_orm::pool()?;
    ensure_table(pool).await?;
    sweep_expired_if_due(pool).await;

    let select_sql = format!(
        "SELECT value, expires_at FROM cache_items WHERE \"key\" = {}",
        larust_orm::placeholder(larust_orm::backend(), 1)
    );
    let row: Option<(String, i64)> = sqlx::query_as(&select_sql)
        .bind(key)
        .fetch_optional(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    let Some((json, expires_at)) = row else {
        return Ok(None);
    };

    if expires_at <= now_unix_secs() {
        // Lazily evict. Either way this call reports a miss, so a failed
        // delete here isn't fatal to it - the next `get`/`put` on this key
        // will just try the same cleanup again.
        let delete_sql = format!(
            "DELETE FROM cache_items WHERE \"key\" = {}",
            larust_orm::placeholder(larust_orm::backend(), 1)
        );
        let _ = sqlx::query(&delete_sql).bind(key).execute(pool).await;
        return Ok(None);
    }

    serde_json::from_str(&json)
        .map(Some)
        .map_err(|source| AppError::Internal(Box::new(source)))
}

/// Removes `key`, if present. Not an error to forget a key that was never
/// set or has already expired.
pub(crate) async fn forget(key: &str) -> Result<(), AppError> {
    let pool = larust_orm::pool()?;
    ensure_table(pool).await?;

    let sql = format!(
        "DELETE FROM cache_items WHERE \"key\" = {}",
        larust_orm::placeholder(larust_orm::backend(), 1)
    );
    sqlx::query(&sql)
        .bind(key)
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    Ok(())
}
