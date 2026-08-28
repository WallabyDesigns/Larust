//! `QueryBuilder` + a process-wide connection pool over `sqlx` (SQLite or
//! MySQL, chosen at runtime by `DATABASE_URL` — see `pool::Backend`), plus
//! a minimal migration runner. `#[derive(Model)]` (in `larust-macros`)
//! generates the CRUD methods that use these.

mod config;
mod migrate;
mod pool;
mod query_builder;
mod repository;

pub use config::{ConnectionConfig, DatabaseConnections, Driver};
pub use migrate::run as migrate;
pub use pool::{
    backend, connect, ensure_backend, normalize_sqlite_url, placeholder, pool, with_pool_override,
    Backend,
};
pub use query_builder::{BindValue, QueryBuilder};
pub use repository::AnyRepository;

pub use sqlx;
