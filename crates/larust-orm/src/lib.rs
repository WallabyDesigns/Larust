//! `QueryBuilder` + a process-wide connection pool over `sqlx` (SQLite for
//! now), plus a minimal migration runner. `#[derive(Model)]` (in
//! `larust-macros`) generates the CRUD methods that use these.

mod migrate;
mod pool;
mod query_builder;

pub use migrate::run as migrate;
pub use pool::{connect, pool, with_pool_override};
pub use query_builder::{BindValue, QueryBuilder};

pub use sqlx;
