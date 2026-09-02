//! Builds and runs parameterized `INSERT`/`UPDATE`/`DELETE` against an
//! already-introspected table, plus the deliberately-unrestricted raw-SQL
//! path the dashboard's own "Run SQL" page uses.
//!
//! **Two different security postures, on purpose.** [`insert_row`]/
//! [`update_row`]/[`delete_row`] only ever interpolate a table or column
//! name into SQL text after the caller has validated it against that
//! table's own freshly-introspected column list (`introspect::
//! table_columns`) — every *value* is always a bound parameter via
//! [`codec::bind_any`], never interpolated as text, which is the actual
//! injection guard. [`run_raw`] is the opposite on purpose: it executes
//! whatever SQL text it's given, unrestricted, the same way phpMyAdmin's
//! own SQL tab is — its safety is the dashboard's existing double gate
//! (`DB_DASHBOARD_PASSWORD` + `APP_DEBUG`-gated registration), not query
//! validation, because restricting it would defeat the entire point of a
//! "run SQL" feature.

use crate::sql::codec::{bind_any, json_to_any_value, row_to_json};
use crate::sql::introspect::ColumnInfo;
use larust_core::AppError;
use serde_json::Value as Json;

fn internal<E: std::error::Error + Send + Sync + 'static>(error: E) -> AppError {
    AppError::Internal(Box::new(error))
}

fn column_kind<'a>(columns: &'a [ColumnInfo], name: &str) -> sqlx::any::AnyTypeInfoKind {
    columns
        .iter()
        .find(|c| c.name == name)
        .map(|c: &'a ColumnInfo| c.kind)
        .unwrap_or(sqlx::any::AnyTypeInfoKind::Text)
}

/// Inserts one row. `values` keys not found in `columns` are silently
/// ignored (a form field the caller shouldn't have sent) rather than
/// erroring — the caller (the HTTP handler) only ever builds `values` from
/// `columns` in the first place, so this is a defense-in-depth backstop,
/// not the primary guard.
pub async fn insert_row(
    table: &str,
    columns: &[ColumnInfo],
    values: &[(String, Json)],
) -> Result<(), AppError> {
    let backend = larust_orm::backend();
    let present: Vec<&(String, Json)> = values
        .iter()
        .filter(|(name, _)| columns.iter().any(|c| &c.name == name))
        .collect();
    if present.is_empty() {
        return Ok(());
    }

    let column_list = present
        .iter()
        .map(|(name, _)| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let placeholders = (1..=present.len())
        .map(|n| larust_orm::placeholder(backend, n))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("INSERT INTO \"{table}\" ({column_list}) VALUES ({placeholders})");

    let pool = larust_orm::pool()?;
    let mut query = sqlx::query(&sql);
    for (name, value) in &present {
        query = bind_any(query, json_to_any_value(value, column_kind(columns, name)));
    }
    query.execute(pool).await.map_err(internal)?;
    Ok(())
}

/// Updates one row identified by `pk` (composite-safe — pass more than
/// one entry for a composite key). Any entry in `values` whose column is
/// also part of `pk` is skipped for the `SET` clause (the primary key
/// itself is never rewritten by an edit form).
pub async fn update_row(
    table: &str,
    columns: &[ColumnInfo],
    pk: &[(String, Json)],
    values: &[(String, Json)],
) -> Result<(), AppError> {
    let backend = larust_orm::backend();
    let settable: Vec<&(String, Json)> = values
        .iter()
        .filter(|(name, _)| {
            columns.iter().any(|c| &c.name == name)
                && !pk.iter().any(|(pk_name, _)| pk_name == name)
        })
        .collect();
    if settable.is_empty() {
        return Ok(());
    }

    let mut n = 0usize;
    let set_clause = settable
        .iter()
        .map(|(name, _)| {
            n += 1;
            format!("\"{name}\" = {}", larust_orm::placeholder(backend, n))
        })
        .collect::<Vec<_>>()
        .join(", ");
    let where_clause = pk
        .iter()
        .map(|(name, _)| {
            n += 1;
            format!("\"{name}\" = {}", larust_orm::placeholder(backend, n))
        })
        .collect::<Vec<_>>()
        .join(" AND ");
    let sql = format!("UPDATE \"{table}\" SET {set_clause} WHERE {where_clause}");

    let pool = larust_orm::pool()?;
    let mut query = sqlx::query(&sql);
    for (name, value) in &settable {
        query = bind_any(query, json_to_any_value(value, column_kind(columns, name)));
    }
    for (name, value) in pk {
        query = bind_any(query, json_to_any_value(value, column_kind(columns, name)));
    }
    query.execute(pool).await.map_err(internal)?;
    Ok(())
}

/// Deletes one row identified by `pk` (composite-safe, same shape as
/// [`update_row`]).
pub async fn delete_row(
    table: &str,
    columns: &[ColumnInfo],
    pk: &[(String, Json)],
) -> Result<(), AppError> {
    let backend = larust_orm::backend();
    let mut n = 0usize;
    let where_clause = pk
        .iter()
        .map(|(name, _)| {
            n += 1;
            format!("\"{name}\" = {}", larust_orm::placeholder(backend, n))
        })
        .collect::<Vec<_>>()
        .join(" AND ");
    let sql = format!("DELETE FROM \"{table}\" WHERE {where_clause}");

    let pool = larust_orm::pool()?;
    let mut query = sqlx::query(&sql);
    for (name, value) in pk {
        query = bind_any(query, json_to_any_value(value, column_kind(columns, name)));
    }
    query.execute(pool).await.map_err(internal)?;
    Ok(())
}

/// Fetches one row identified by `pk`, if it still exists — used to
/// prefill an edit form. Bound parameters, not string interpolation
/// (unlike [`run_raw`]): unlike that function's deliberately-unrestricted
/// user-typed SQL, `pk` here originates from a query string a user can
/// tamper with, so it gets the same real parameter binding every other
/// structured operation in this module uses.
pub async fn fetch_row(
    table: &str,
    columns: &[ColumnInfo],
    pk: &[(String, Json)],
) -> Result<Option<Json>, AppError> {
    let backend = larust_orm::backend();
    let mut n = 0usize;
    let where_clause = pk
        .iter()
        .map(|(name, _)| {
            n += 1;
            format!("\"{name}\" = {}", larust_orm::placeholder(backend, n))
        })
        .collect::<Vec<_>>()
        .join(" AND ");
    let sql = format!("SELECT * FROM \"{table}\" WHERE {where_clause}");

    let pool = larust_orm::pool()?;
    let mut query = sqlx::query(&sql);
    for (name, value) in pk {
        query = bind_any(query, json_to_any_value(value, column_kind(columns, name)));
    }
    let row = query.fetch_optional(pool).await.map_err(internal)?;
    Ok(row.as_ref().map(row_to_json))
}

/// Runs arbitrary, user-typed SQL and renders whatever comes back — see
/// this module's own doc comment for why this one is deliberately
/// unrestricted. `fetch_all` works uniformly for both queries and
/// statements: a `SELECT` returns real rows; `INSERT`/`UPDATE`/`DELETE`/
/// DDL return zero rows on success, so the same call reports "0 rows"
/// rather than needing to sniff the statement's kind from its text first.
pub async fn run_raw(sql: &str) -> Result<Vec<Json>, AppError> {
    let pool = larust_orm::pool()?;
    let rows = sqlx::query(sql).fetch_all(pool).await.map_err(internal)?;
    Ok(rows.iter().map(row_to_json).collect())
}

/// Runs an uploaded `.sql` file's full contents (the SQL Import feature) —
/// unlike [`run_raw`], this doesn't return or render rows, because an
/// imported file is typically many statements, not one query to display.
/// Uses `sqlx::raw_sql` rather than `fetch_all`, the same primitive
/// `larust_orm::migrate::run` already relies on for exactly this reason:
/// it delegates statement parsing to the backend itself, so multi-statement
/// files, comments, and string literals containing semicolons all work
/// correctly (splitting on `;` by hand does not — a real bug this
/// framework already hit once in the migration runner). Same deliberately-
/// unrestricted security posture as [`run_raw`].
pub async fn run_script(sql: &str) -> Result<(), AppError> {
    let pool = larust_orm::pool()?;
    sqlx::raw_sql(sql).execute(pool).await.map_err(internal)?;
    Ok(())
}
