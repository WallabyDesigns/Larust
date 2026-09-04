use crate::pool::{backend, pool, Backend};
use larust_core::AppError;
use sqlx::Row;

/// Every real table in the connected database, alphabetical - the one
/// portable piece of schema introspection this crate needs (`sqlx::Any` has
/// no catalog API of its own; see `larust-db`'s own `sql::introspect`
/// module for the fuller browse/edit-oriented introspection built on top of
/// this same query). SQLite's internal `sqlite_%` tables are excluded;
/// MySQL/Postgres's `information_schema.tables` query is already scoped to
/// just the connected database/schema, so nothing internal leaks through.
///
/// This includes `_migrations` itself - correct for [`crate::migrate::fresh`],
/// which wants that table dropped and recreated along with everything else.
pub async fn table_names() -> Result<Vec<String>, AppError> {
    let pool = pool()?;
    let sql = match backend() {
        Backend::Sqlite => {
            "SELECT name FROM sqlite_master WHERE type = 'table' \
             AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name"
        }
        Backend::MySql => {
            "SELECT table_name FROM information_schema.tables \
             WHERE table_schema = DATABASE() ORDER BY table_name"
        }
        Backend::Postgres => {
            "SELECT table_name FROM information_schema.tables \
             WHERE table_schema = 'public' ORDER BY table_name"
        }
    };
    let rows = sqlx::query(sql)
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Internal(Box::new(e)))?;
    Ok(rows.iter().map(|row| row.get::<String, _>(0)).collect())
}
