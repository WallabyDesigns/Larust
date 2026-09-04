//! Schema introspection - `sqlx::Any` has no portable "list tables"/
//! "describe table" API of its own (confirmed by reading its source: the
//! only "meta" operations it exposes are database-level create/drop, for
//! `sqlx::migrate`, nothing table/column-level). Every function here
//! hand-writes the per-backend catalog query - `sqlite_master`/`PRAGMA
//! table_info` for SQLite, the standard SQL-92 `information_schema` views
//! for MySQL/Postgres - and decodes the result through the exact same
//! generic row-reading path (`sqlx::Row`) everything else in this crate's
//! `sql` module uses.

use larust_core::AppError;
use larust_orm::Backend;
use sqlx::any::AnyTypeInfoKind;
use sqlx::Row;

/// One column's shape - enough to render an edit input and bind a
/// submitted value back to it correctly (see `codec::json_to_any_value`).
/// A `Blob`-kind column is a known, stated v1 gap, not silently mishandled:
/// the browse view renders its value as `<blob, N bytes>` (never the raw
/// bytes), and a submitted value for one is stored as plain text rather
/// than real binary - `dashboard::sql_views`'s edit/insert forms don't yet
/// special-case it with a distinct (disabled, or upload-shaped) input, so
/// editing one through the dashboard silently overwrites it with UTF-8
/// text. No table in `demo` or the framework's own schema has one today.
#[derive(Debug, Clone)]
pub struct ColumnInfo {
    pub name: String,
    pub not_null: bool,
    pub kind: AnyTypeInfoKind,
}

fn internal<E: std::error::Error + Send + Sync + 'static>(error: E) -> AppError {
    AppError::Internal(Box::new(error))
}

/// Every real table in the connected database, alphabetical - delegates to
/// `larust_orm::table_names()`, which hand-writes this same per-backend
/// catalog query for its own [`larust_orm::migrate::fresh`]; kept as one
/// query with two callers rather than duplicated here.
pub async fn list_tables() -> Result<Vec<String>, AppError> {
    larust_orm::table_names().await
}

/// `table`'s indexes, one row per index (shape varies by backend - see
/// each arm below). Rendered through the same generic result-table
/// machinery as the raw `/sql` page's output (`codec::row_to_json` handles
/// any `AnyRow`, `PRAGMA`/`SHOW` output included), not a bespoke parsed
/// struct - this is a read-only viewer, the raw columns are the point.
/// Same "caller already validated `table`" contract as [`table_columns`].
pub async fn list_indexes(table: &str) -> Result<Vec<serde_json::Value>, AppError> {
    let pool = larust_orm::pool()?;
    let rows = match larust_orm::backend() {
        Backend::Sqlite => {
            let sql = format!("PRAGMA index_list(\"{table}\")");
            sqlx::query(&sql).fetch_all(pool).await.map_err(internal)?
        }
        Backend::MySql => {
            // ANSI_QUOTES is set for the session (see `larust_orm::connect`'s
            // `after_connect` hook), so a double-quoted identifier works
            // here exactly like it does in ordinary SELECTs.
            let sql = format!("SHOW INDEX FROM \"{table}\"");
            sqlx::query(&sql).fetch_all(pool).await.map_err(internal)?
        }
        backend @ Backend::Postgres => {
            let sql = format!(
                "SELECT indexname, indexdef FROM pg_indexes WHERE tablename = {}",
                larust_orm::placeholder(backend, 1)
            );
            sqlx::query(&sql)
                .bind(table)
                .fetch_all(pool)
                .await
                .map_err(internal)?
        }
    };
    Ok(rows.iter().map(crate::sql::codec::row_to_json).collect())
}

/// `table`'s foreign keys, one row per referencing column. Same shape/
/// rendering rationale as [`list_indexes`].
pub async fn list_foreign_keys(table: &str) -> Result<Vec<serde_json::Value>, AppError> {
    let pool = larust_orm::pool()?;
    let rows = match larust_orm::backend() {
        Backend::Sqlite => {
            let sql = format!("PRAGMA foreign_key_list(\"{table}\")");
            sqlx::query(&sql).fetch_all(pool).await.map_err(internal)?
        }
        Backend::MySql => {
            let sql = "SELECT column_name, referenced_table_name, referenced_column_name, \
                        constraint_name FROM information_schema.key_column_usage \
                        WHERE table_schema = DATABASE() AND table_name = ? \
                        AND referenced_table_name IS NOT NULL";
            sqlx::query(sql)
                .bind(table)
                .fetch_all(pool)
                .await
                .map_err(internal)?
        }
        Backend::Postgres => {
            let sql = "SELECT kcu.column_name, ccu.table_name AS referenced_table_name, \
                        ccu.column_name AS referenced_column_name, tc.constraint_name \
                        FROM information_schema.table_constraints tc \
                        JOIN information_schema.key_column_usage kcu \
                          ON tc.constraint_name = kcu.constraint_name \
                          AND tc.table_schema = kcu.table_schema \
                        JOIN information_schema.constraint_column_usage ccu \
                          ON tc.constraint_name = ccu.constraint_name \
                          AND tc.table_schema = ccu.table_schema \
                        WHERE tc.constraint_type = 'FOREIGN KEY' AND tc.table_name = $1";
            sqlx::query(sql)
                .bind(table)
                .fetch_all(pool)
                .await
                .map_err(internal)?
        }
    };
    Ok(rows.iter().map(crate::sql::codec::row_to_json).collect())
}

/// `table`'s columns, in declaration order. Callers MUST have already
/// validated `table` against [`list_tables`] - SQLite's `PRAGMA` can't
/// take a bind parameter for the table name, so it's interpolated
/// directly, safe only because it's guaranteed to be a real, existing
/// table name by the time this runs, never raw user input.
pub async fn table_columns(table: &str) -> Result<Vec<ColumnInfo>, AppError> {
    let pool = larust_orm::pool()?;
    match larust_orm::backend() {
        Backend::Sqlite => {
            let sql = format!("PRAGMA table_info(\"{table}\")");
            let rows = sqlx::query(&sql).fetch_all(pool).await.map_err(internal)?;
            Ok(rows
                .iter()
                .map(|row| {
                    let name: String = row.get("name");
                    let type_text: String = row.get("type");
                    let notnull: i64 = row.get("notnull");
                    ColumnInfo {
                        name,
                        not_null: notnull != 0,
                        kind: infer_kind(&type_text),
                    }
                })
                .collect())
        }
        backend @ (Backend::MySql | Backend::Postgres) => {
            let sql = format!(
                "SELECT column_name, data_type, is_nullable FROM information_schema.columns \
                 WHERE table_name = {} ORDER BY ordinal_position",
                larust_orm::placeholder(backend, 1)
            );
            let rows = sqlx::query(&sql)
                .bind(table)
                .fetch_all(pool)
                .await
                .map_err(internal)?;
            Ok(rows
                .iter()
                .map(|row| {
                    let name: String = row.get("column_name");
                    let type_text: String = row.get("data_type");
                    let is_nullable: String = row.get("is_nullable");
                    ColumnInfo {
                        name,
                        not_null: is_nullable != "YES",
                        kind: infer_kind(&type_text),
                    }
                })
                .collect())
        }
    }
}

/// `table`'s primary key column(s), in key order - composite-safe. Same
/// "caller already validated `table`" contract as [`table_columns`].
pub async fn primary_key_columns(table: &str) -> Result<Vec<String>, AppError> {
    let pool = larust_orm::pool()?;
    match larust_orm::backend() {
        Backend::Sqlite => {
            // PRAGMA table_info's own `pk` column is the 1-based ordinal
            // within the primary key (0 = not part of it) - ordering by
            // it directly reconstructs composite-key column order.
            let sql = format!("PRAGMA table_info(\"{table}\")");
            let rows = sqlx::query(&sql).fetch_all(pool).await.map_err(internal)?;
            let mut pk: Vec<(i64, String)> = rows
                .iter()
                .filter_map(|row| {
                    let ordinal: i64 = row.get("pk");
                    if ordinal == 0 {
                        return None;
                    }
                    let name: String = row.get("name");
                    Some((ordinal, name))
                })
                .collect();
            pk.sort_by_key(|(ordinal, _)| *ordinal);
            Ok(pk.into_iter().map(|(_, name)| name).collect())
        }
        backend @ (Backend::MySql | Backend::Postgres) => {
            let sql = format!(
                "SELECT kcu.column_name FROM information_schema.key_column_usage kcu \
                 JOIN information_schema.table_constraints tc \
                   ON kcu.constraint_name = tc.constraint_name \
                   AND kcu.table_schema = tc.table_schema \
                 WHERE tc.constraint_type = 'PRIMARY KEY' AND kcu.table_name = {} \
                 ORDER BY kcu.ordinal_position",
                larust_orm::placeholder(backend, 1)
            );
            let rows = sqlx::query(&sql)
                .bind(table)
                .fetch_all(pool)
                .await
                .map_err(internal)?;
            Ok(rows
                .iter()
                .map(|row| row.get::<String, _>("column_name"))
                .collect())
        }
    }
}

/// Maps a backend's own declared-type text (SQLite's free-form type
/// affinity string, or `information_schema.columns.data_type`) onto the
/// 9 kinds `sqlx::any` normalizes every value into. Deliberately
/// pattern-based, not exhaustive - this schema (like most SQLite-first
/// apps) has no table anywhere using a genuine `BOOLEAN`/`TIMESTAMP` SQL
/// type (booleans and timestamps are plain `INTEGER`), so `Text` is the
/// correct, safe fallback for anything not recognized rather than a
/// guess that could silently misdecode.
fn infer_kind(declared_type: &str) -> AnyTypeInfoKind {
    let upper = declared_type.to_ascii_uppercase();
    if upper.contains("BOOL") {
        AnyTypeInfoKind::Bool
    } else if upper.contains("BLOB") || upper.contains("BINARY") || upper.contains("BYTEA") {
        AnyTypeInfoKind::Blob
    } else if upper.contains("DOUBLE")
        || upper.contains("REAL")
        || upper.contains("FLOA")
        || upper.contains("DEC")
        || upper.contains("NUMERIC")
    {
        AnyTypeInfoKind::Double
    } else if upper.contains("INT") {
        AnyTypeInfoKind::BigInt
    } else {
        AnyTypeInfoKind::Text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_kind_recognizes_common_sqlite_and_information_schema_type_names() {
        assert_eq!(infer_kind("INTEGER"), AnyTypeInfoKind::BigInt);
        assert_eq!(infer_kind("integer"), AnyTypeInfoKind::BigInt);
        assert_eq!(infer_kind("BIGINT"), AnyTypeInfoKind::BigInt);
        assert_eq!(infer_kind("TEXT"), AnyTypeInfoKind::Text);
        assert_eq!(infer_kind("character varying"), AnyTypeInfoKind::Text);
        assert_eq!(infer_kind("VARCHAR(255)"), AnyTypeInfoKind::Text);
        assert_eq!(infer_kind("REAL"), AnyTypeInfoKind::Double);
        assert_eq!(infer_kind("DOUBLE PRECISION"), AnyTypeInfoKind::Double);
        assert_eq!(infer_kind("NUMERIC(10,2)"), AnyTypeInfoKind::Double);
        assert_eq!(infer_kind("BOOLEAN"), AnyTypeInfoKind::Bool);
        assert_eq!(infer_kind("BLOB"), AnyTypeInfoKind::Blob);
        assert_eq!(infer_kind("bytea"), AnyTypeInfoKind::Blob);
        assert_eq!(infer_kind("some unknown type"), AnyTypeInfoKind::Text);
    }
}
