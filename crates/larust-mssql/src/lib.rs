//! SQL Server support via a hand-written [`larust_repository::Repository`]
//! implementation, one per model - **not** a `larust_orm::Backend`
//! variant. `sqlx` has no Microsoft SQL Server driver at all (confirmed by
//! reading its own source during this crate's design: `sqlx-core`'s only
//! MSSQL-shaped code is permanently-dead, pre-0.8 scaffolding, and no
//! `mssql` Cargo feature has ever existed to enable it), so SQL Server
//! structurally cannot join the `sqlx::Any`/`Backend`/`QueryBuilder`
//! architecture the way SQLite/MySQL/Postgres do. This crate connects
//! through [`tiberius`] - a separate, pure-Rust TDS-protocol client with
//! its own connection type, its own `@P1`/`@P2` parameter syntax, and no
//! shared code with `larust-orm` at all.
//!
//! **Explicit ceiling, stated plainly, not oversold: CRUD only.** No
//! `#[derive(Model)]` codegen, no relations, no `xr migrate`, no
//! `QueryBuilder`-style chaining - the same trade-off
//! `larust_repository::Repository`'s own doc comment already documents
//! for "a backend `sqlx::Any` structurally cannot reach." This crate owns
//! only the connection plumbing below; writing an actual `Repository<T>`
//! implementation for your own model is real, per-model code you write by
//! hand - there is no generic blanket `impl<T> Repository<T> for
//! AnyRepository<T>` the way `larust-orm` has for SQL-family backends,
//! for the same reason that type's own doc comment gives: building
//! `INSERT`/`UPDATE` text (or here, `tiberius` parameter bindings) from a
//! struct's fields needs to know those fields, which a function generic
//! over `T` never does - only code written *for* that specific struct
//! can. See `tests/widget_repository.rs` for a complete worked example
//! against a real server, mirroring `larust-repository`'s own
//! `InMemoryRepository` test in shape, just against `tiberius` instead of
//! a `HashMap`.
//!
//! **The connection is a single, mutex-serialized [`tiberius::Client`],
//! not a real pool** - `tiberius` has no built-in pooling. This
//! serializes all SQL Server traffic in a process through one TDS
//! session, a real and deliberately undisguised limitation for this first
//! cut, not a glossed-over one. A proper pool (`bb8`/`deadpool` plus a
//! hand-written `tiberius` adapter) is the natural follow-up if concurrent
//! load against SQL Server ever becomes a real requirement - explicitly
//! out of scope here.

use larust_core::AppError;
use tokio::net::TcpStream;
use tokio::sync::{Mutex, MutexGuard, OnceCell};
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

/// Re-exported so a hand-written `Repository<T>` implementation (see this
/// crate's own doc comment) can build query parameters / read `Row`
/// values without needing `tiberius` as a second, separately-versioned
/// direct dependency of its own.
pub use tiberius;

/// The concrete client type [`client`] hands back, locked. `Compat<
/// TcpStream>` is `tiberius`'s own transport-agnostic `Client` running
/// over a plain Tokio TCP stream via the `tokio-util` compatibility shim
/// (`tiberius` is built against the `futures` `AsyncRead`/`AsyncWrite`
/// traits, not Tokio's own).
pub type MssqlClient = tiberius::Client<Compat<TcpStream>>;

static CLIENT: OnceCell<Mutex<MssqlClient>> = OnceCell::const_new();

/// SQL Server connection settings - the same host/port/database/username/
/// password shape `larust_orm::config::ConnectionConfig` uses for the
/// SQL-family backends, but deliberately not that type itself: `tiberius`
/// doesn't take a connection URL the way `sqlx` does, it takes its own
/// `tiberius::Config` builder, so there's no shared `to_url()`-style
/// assembly to reuse here.
#[derive(Debug, Clone)]
pub struct MssqlConfig {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    pub password: String,
}

/// Connects and stores the client process-wide - same `OnceCell`
/// singleton idiom `larust_orm::pool()`'s own `OnceLock<AnyPool>` uses.
/// Call once at startup, before any `Repository<T>` implementation built
/// on [`client`] runs.
///
/// `trust_cert()` is set unconditionally - this targets a local/dev SQL
/// Server instance the same way this framework's SQLite/MySQL/Postgres
/// support already does (none of them has a TLS-certificate-verification
/// story either); a hardened production deployment would need real
/// certificate handling this first cut doesn't attempt.
pub async fn connect(config: &MssqlConfig) -> Result<(), AppError> {
    let mut tiberius_config = tiberius::Config::new();
    tiberius_config.host(&config.host);
    tiberius_config.port(config.port);
    tiberius_config.database(&config.database);
    tiberius_config.authentication(tiberius::AuthMethod::sql_server(
        &config.username,
        &config.password,
    ));
    tiberius_config.trust_cert();

    let tcp = TcpStream::connect(tiberius_config.get_addr())
        .await
        .map_err(|e| AppError::Internal(Box::new(e)))?;
    tcp.set_nodelay(true)
        .map_err(|e| AppError::Internal(Box::new(e)))?;

    let client = tiberius::Client::connect(tiberius_config, tcp.compat_write())
        .await
        .map_err(|e| AppError::Internal(Box::new(e)))?;

    CLIENT.set(Mutex::new(client)).map_err(|_| {
        AppError::Internal(Box::new(std::io::Error::other(
            "larust_mssql::connect() called more than once",
        )))
    })?;
    Ok(())
}

/// Returns the process-wide client, locked for exclusive use - released
/// automatically when the returned guard drops. See this crate's own doc
/// comment for why this is one mutex-serialized connection, not a pool.
/// Errors (rather than panics) if [`connect`] hasn't run yet - the same
/// "a misconfigured startup order is a real possibility" reasoning
/// `larust_orm::pool()` already documents for its own identical case.
pub async fn client() -> Result<MutexGuard<'static, MssqlClient>, AppError> {
    let mutex = CLIENT.get().ok_or_else(|| {
        AppError::Internal(Box::new(std::io::Error::other(
            "SQL Server not connected; call larust_mssql::connect() at startup \
             before any Repository<T> implementation runs",
        )))
    })?;
    Ok(mutex.lock().await)
}
