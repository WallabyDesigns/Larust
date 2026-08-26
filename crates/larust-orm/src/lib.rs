//! `QueryBuilder` + a process-wide connection pool over `sqlx` (SQLite or
//! MySQL, chosen at runtime by `DATABASE_URL` — see `pool::Backend`), plus
//! a minimal migration runner. `#[derive(Model)]` (in `larust-macros`)
//! generates the CRUD methods that use these.

mod migrate;
mod pool;
mod query_builder;

pub use migrate::run as migrate;
pub use pool::{
    backend, connect, ensure_backend, normalize_sqlite_url, pool, with_pool_override, Backend,
};
pub use query_builder::{BindValue, QueryBuilder};

pub use sqlx;
