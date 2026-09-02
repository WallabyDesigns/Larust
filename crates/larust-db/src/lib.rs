//! Larust's own database admin dashboard (`/xr-db`, `dashboard/` module) —
//! a phpMyAdmin/Adminer-style tool built into the framework itself: browse
//! and edit the app's *actual* SQL database (whatever `DB_CONNECTION` is
//! configured — SQLite/MySQL/Postgres via `larust_orm::AnyPool`; see
//! `sql/` for the schema-introspection/generic-row engine behind it), run
//! raw SQL, all from a browser during development. This is the primary
//! reason this crate exists.
//!
//! **Also an embedded, pure-Rust key-value store** (this module — wraps
//! [`redb`], single-file, MVCC, zero C dependencies), reachable from the
//! same dashboard's secondary "Key-Value" section. Named `db` (not `kv`)
//! for the `xr new` wizard/CLI/feature surface deliberately — a developer
//! scanning feature names recognizes "db" instantly. **Additive, not a
//! second SQL backend**: `#[derive(Model)]` generates literal SQL and
//! requires `sqlx::FromRow` — a KV store has no columns to decode, so it
//! structurally cannot plug into that macro, `larust_repository`'s
//! relations, or `QueryBuilder`. Every real model (`users`, `posts`, ...)
//! lives in the SQL database, which the dashboard's primary section
//! browses directly — the KV store is only ever for app-local data that
//! never needed relations in the first place (feature flags, small local
//! caches, offline queues, embedded config); the same posture
//! `larust_cache` already has for its own SQLite-backed store.
//!
//! **The KV store has no network port, no server process, ever.** `redb`
//! is an in-process embedded library, the same way `rusqlite`/
//! `sqlx-sqlite` are for SQLite — `connect()` just opens a plain file on
//! disk (`database/db.redb` by default) directly inside the app's own
//! process. There is nothing to start, stop, or point a connection
//! string's host/port at. The dashboard itself is likewise just a route
//! on the app's *own* existing HTTP server, not a second listener — it
//! shares whatever port `APP_PORT` already binds, for both its SQL and KV
//! sections alike; only the SQL side additionally talks to whatever real
//! database server the app itself is already configured to use.
//!
//! Gated behind `larust-support`'s `db` feature — re-exported through
//! `larust_support::db`, never depended on directly by a generated app —
//! selectable via `xr new`'s wizard. See `docs/ARCHITECTURE.md`'s
//! "Embedded key-value store" section for the full design, and
//! `dashboard/mod.rs`'s own doc comment for the dashboard itself.

mod dashboard;
pub mod sql;

pub use dashboard::DbPlugin;

use larust_core::AppError;
use redb::{Database, ReadableTable, TableDefinition};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::path::Path;
use std::sync::{Arc, OnceLock};

const TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("kv");

static DATABASE: OnceLock<Arc<Database>> = OnceLock::new();

/// Opens (creating if missing) the embedded store at `path` and stores it
/// process-wide — same `OnceLock` singleton discipline as
/// `larust_orm::connect()` ([`crates/larust-orm/src/pool.rs`]), for the
/// same reason: an embedded engine's own file lock means only one
/// `Database` handle per process per file. Call once at startup, before
/// any other function in this module.
pub async fn connect(path: impl AsRef<Path>) -> Result<(), AppError> {
    let path = path.as_ref().to_path_buf();
    let db = tokio::task::spawn_blocking(move || -> Result<Database, AppError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(internal)?;
            }
        }
        let db = Database::create(&path).map_err(internal)?;
        // Opening a table on a write transaction creates it if it doesn't
        // exist yet — done once here so every later `get`/`keys` (which
        // only ever open a *read* transaction, where opening a
        // still-missing table is an error, not an empty result) can rely
        // on the table always already existing.
        let txn = db.begin_write().map_err(internal)?;
        {
            txn.open_table(TABLE).map_err(internal)?;
        }
        txn.commit().map_err(internal)?;
        Ok(db)
    })
    .await
    .map_err(internal)??;

    DATABASE.set(Arc::new(db)).map_err(|_| {
        AppError::Internal(Box::new(std::io::Error::other(
            "connect() called more than once",
        )))
    })
}

fn store() -> Result<Arc<Database>, AppError> {
    DATABASE.get().cloned().ok_or_else(|| {
        AppError::Internal(Box::new(std::io::Error::other(
            "embedded db not connected; call larust_db::connect() (via \
             larust_support::db::connect) at startup before using it",
        )))
    })
}

/// Stores `value` (JSON-serialized) under `key`, overwriting any existing
/// value.
pub async fn put<T: Serialize>(key: &str, value: &T) -> Result<(), AppError> {
    let value = serde_json::to_value(value).map_err(internal)?;
    put_raw(key, value).await
}

/// Reads the value stored under `key`, if any, deserialized as `T`.
pub async fn get<T: DeserializeOwned>(key: &str) -> Result<Option<T>, AppError> {
    match get_raw(key).await? {
        Some(value) => Ok(Some(serde_json::from_value(value).map_err(internal)?)),
        None => Ok(None),
    }
}

/// Removes `key`, if present. A no-op (not an error) if it isn't.
pub async fn forget(key: &str) -> Result<(), AppError> {
    let db = store()?;
    let key = key.to_string();
    tokio::task::spawn_blocking(move || {
        let txn = db.begin_write().map_err(internal)?;
        {
            let mut table = txn.open_table(TABLE).map_err(internal)?;
            table.remove(key.as_str()).map_err(internal)?;
        }
        txn.commit().map_err(internal)?;
        Ok(())
    })
    .await
    .map_err(internal)?
}

/// Every key currently stored, in no particular order. A dev/inspection
/// tool (the CLI browser, the dashboard) — not meant for a hot request
/// path in a store with a large key count.
pub async fn keys() -> Result<Vec<String>, AppError> {
    let db = store()?;
    tokio::task::spawn_blocking(move || {
        let txn = db.begin_read().map_err(internal)?;
        let table = txn.open_table(TABLE).map_err(internal)?;
        let mut out = Vec::new();
        for entry in table.iter().map_err(internal)? {
            let (k, _v) = entry.map_err(internal)?;
            out.push(k.value().to_string());
        }
        Ok(out)
    })
    .await
    .map_err(internal)?
}

/// Untyped read — what the CLI browser and dashboard use, since they don't
/// know any app's Rust types at compile time. [`get`] is a thin
/// `serde_json::from_value` wrapper around this.
pub async fn get_raw(key: &str) -> Result<Option<serde_json::Value>, AppError> {
    let db = store()?;
    let key = key.to_string();
    tokio::task::spawn_blocking(move || {
        let txn = db.begin_read().map_err(internal)?;
        let table = txn.open_table(TABLE).map_err(internal)?;
        match table.get(key.as_str()).map_err(internal)? {
            Some(value) => Ok(Some(
                serde_json::from_slice(value.value()).map_err(internal)?,
            )),
            None => Ok(None),
        }
    })
    .await
    .map_err(internal)?
}

/// Untyped write — what the CLI browser and dashboard use. [`put`] is a
/// thin `serde_json::to_value` wrapper around this.
pub async fn put_raw(key: &str, value: serde_json::Value) -> Result<(), AppError> {
    let db = store()?;
    let key = key.to_string();
    tokio::task::spawn_blocking(move || {
        let bytes = serde_json::to_vec(&value).map_err(internal)?;
        let txn = db.begin_write().map_err(internal)?;
        {
            let mut table = txn.open_table(TABLE).map_err(internal)?;
            table
                .insert(key.as_str(), bytes.as_slice())
                .map_err(internal)?;
        }
        txn.commit().map_err(internal)?;
        Ok(())
    })
    .await
    .map_err(internal)?
}

/// Parses a CLI argument into a JSON value for [`put_raw`] — tries real
/// JSON first (`42` -> a number, `"Alice"` -> a string, `true` -> a bool),
/// falling back to storing the raw text as a JSON string so `xr db:put
/// name Alice` works without shell-quoting a JSON string.
pub fn parse_cli_value(raw: &str) -> serde_json::Value {
    serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.to_string()))
}

fn internal<E: std::error::Error + Send + Sync + 'static>(error: E) -> AppError {
    AppError::Internal(Box::new(error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Item {
        name: String,
        count: u32,
    }

    // `DATABASE` is a process-wide `OnceLock` (mirroring
    // `larust_orm::connect()`'s own singleton discipline — see this
    // module's doc comment) — a second `connect()` call in the same test
    // binary errors "connect() called more than once", and `cargo test`
    // runs every `#[tokio::test]` in one process. So every scenario below
    // runs from this one test function, the same convention every
    // DB-touching test file in this codebase already follows
    // (`examples/repository_bench`, `larust-orm`'s own tests, ...).
    #[tokio::test]
    async fn store_behaves_correctly_across_every_scenario() {
        let dir = tempfile::tempdir().unwrap();
        connect(dir.path().join("test.redb")).await.unwrap();

        // put/get/forget/keys round trip.
        assert_eq!(get::<Item>("missing").await.unwrap(), None);
        assert_eq!(keys().await.unwrap(), Vec::<String>::new());

        let item = Item {
            name: "widget".to_string(),
            count: 3,
        };
        put("item", &item).await.unwrap();
        assert_eq!(get::<Item>("item").await.unwrap(), Some(item));
        assert_eq!(keys().await.unwrap(), vec!["item".to_string()]);

        forget("item").await.unwrap();
        assert_eq!(get::<Item>("item").await.unwrap(), None);
        assert_eq!(keys().await.unwrap(), Vec::<String>::new());

        // Forgetting a missing key is not an error.
        forget("nope").await.unwrap();

        // put overwrites an existing value.
        put("count", &1).await.unwrap();
        put("count", &2).await.unwrap();
        assert_eq!(get::<i64>("count").await.unwrap(), Some(2));

        // Raw variants round-trip arbitrary JSON.
        put_raw("raw", serde_json::json!({"a": 1, "b": [true, null]}))
            .await
            .unwrap();
        assert_eq!(
            get_raw("raw").await.unwrap(),
            Some(serde_json::json!({"a": 1, "b": [true, null]}))
        );

        // A second connect() call is rejected, not silently accepted.
        let second = tempfile::tempdir().unwrap();
        let err = connect(second.path().join("other.redb")).await.unwrap_err();
        assert!(err.to_string().contains("more than once"));
    }

    #[test]
    fn parse_cli_value_parses_real_json() {
        assert_eq!(parse_cli_value("42"), serde_json::json!(42));
        assert_eq!(parse_cli_value("true"), serde_json::json!(true));
        assert_eq!(parse_cli_value("\"Alice\""), serde_json::json!("Alice"));
    }

    #[test]
    fn parse_cli_value_falls_back_to_a_plain_string() {
        assert_eq!(parse_cli_value("Alice"), serde_json::json!("Alice"));
    }
}
