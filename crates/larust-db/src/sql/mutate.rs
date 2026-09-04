//! Builds and runs parameterized `INSERT`/`UPDATE`/`DELETE` against an
//! already-introspected table, plus the deliberately-unrestricted raw-SQL
//! path the dashboard's own "Run SQL" page uses.
//!
//! **Two different security postures, on purpose.** The structured
//! functions ([`insert_row`], [`update_row`], [`delete_row`],
//! [`fetch_row`]) only ever interpolate a table or column name into SQL
//! text after validating it against that table's own freshly-introspected
//! column list (`introspect::table_columns`) - every *value* is always a
//! bound parameter via [`codec::bind_any`], never interpolated as text.
//! Column-name validation is enforced *inside* this module, not left to
//! the caller: `values`/`settable` are filtered against `columns` before
//! use, and `pk` is checked by [`require_known_pk_columns`] - the latter
//! closes a real SQL-injection gap found in a security review (`pk`'s
//! column names previously reached a `WHERE` clause unchecked, since they
//! originate from a request's raw field *names*, not values, which
//! nothing upstream ever validated). The raw functions ([`run_raw`],
//! [`run_script`]) are the deliberate opposite: they execute whatever SQL
//! text they're given, unrestricted, the same way phpMyAdmin's own SQL tab
//! and Import feature are - their safety is the dashboard's existing
//! double gate (`DB_DASHBOARD_PASSWORD` + `APP_DEBUG`-gated registration),
//! not query validation, because restricting either would defeat the
//! entire point of those features.

use crate::sql::codec::{bind_any, json_to_any_value, row_to_json};
use crate::sql::introspect::ColumnInfo;
use axum::http::StatusCode;
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

/// `pk` column *names* end up interpolated directly into a `WHERE` clause
/// as SQL identifiers (values are always bound - see this module's own
/// doc comment) - so, unlike `insert_row`'s `values`/`update_row`'s
/// `settable` (both already filtered against `columns` before being used
/// to build SQL text), every `pk` entry MUST be checked against the same
/// introspected column list before it reaches a `format!` call. This was
/// a real, exploitable gap: `pk` originates from `sql_views::extract_pk`,
/// which builds it straight from a request's raw query-string/form-field
/// *keys* (`pk_<anything>`) with no validation at all - an authenticated
/// dashboard user could submit `pk_<injection>` as a field name and inject
/// arbitrary SQL into the WHERE clause of `update`/`delete`/the edit
/// form's row fetch, bypassing the "only real columns, only bound values"
/// guarantee every doc comment in this file already claimed to provide.
fn require_known_pk_columns(columns: &[ColumnInfo], pk: &[(String, Json)]) -> Result<(), AppError> {
    for (name, _) in pk {
        if !columns.iter().any(|c| &c.name == name) {
            return Err(AppError::Http {
                status: StatusCode::BAD_REQUEST,
                message: format!("unknown primary key column: {name}"),
            });
        }
    }
    Ok(())
}

/// Inserts one row. `values` keys not found in `columns` are silently
/// ignored (a form field the caller shouldn't have sent) rather than
/// erroring - the caller (the HTTP handler) only ever builds `values` from
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

/// Updates one row identified by `pk` (composite-safe - pass more than
/// one entry for a composite key). Any entry in `values` whose column is
/// also part of `pk` is skipped for the `SET` clause (the primary key
/// itself is never rewritten by an edit form).
pub async fn update_row(
    table: &str,
    columns: &[ColumnInfo],
    pk: &[(String, Json)],
    values: &[(String, Json)],
) -> Result<(), AppError> {
    require_known_pk_columns(columns, pk)?;
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
    require_known_pk_columns(columns, pk)?;
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

/// Fetches one row identified by `pk`, if it still exists - used to
/// prefill an edit form. `pk` originates from a query string a user can
/// tamper with, so both its column *names* ([`require_known_pk_columns`])
/// and its *values* ([`bind_any`]) are validated/bound before any SQL is
/// built - unlike [`run_raw`]'s deliberately-unrestricted user-typed SQL.
pub async fn fetch_row(
    table: &str,
    columns: &[ColumnInfo],
    pk: &[(String, Json)],
) -> Result<Option<Json>, AppError> {
    require_known_pk_columns(columns, pk)?;
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

/// Runs arbitrary, user-typed SQL and renders whatever comes back - see
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

/// Runs an uploaded `.sql` file's full contents (the SQL Import feature) -
/// unlike [`run_raw`], this doesn't return or render rows, because an
/// imported file is typically many statements, not one query to display.
/// Uses `sqlx::raw_sql` rather than `fetch_all`, the same primitive
/// `larust_orm::migrate::run` already relies on for exactly this reason:
/// it delegates statement parsing to the backend itself, so multi-statement
/// files, comments, and string literals containing semicolons all work
/// correctly (splitting on `;` by hand does not - a real bug this
/// framework already hit once in the migration runner). Same deliberately-
/// unrestricted security posture as [`run_raw`].
pub async fn run_script(sql: &str) -> Result<(), AppError> {
    let pool = larust_orm::pool()?;
    sqlx::raw_sql(sql).execute(pool).await.map_err(internal)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id_column() -> ColumnInfo {
        ColumnInfo {
            name: "id".to_string(),
            not_null: true,
            kind: sqlx::any::AnyTypeInfoKind::BigInt,
        }
    }

    #[test]
    fn require_known_pk_columns_accepts_a_real_column() {
        let columns = [id_column()];
        let pk = [("id".to_string(), Json::from(1))];
        assert!(require_known_pk_columns(&columns, &pk).is_ok());
    }

    #[test]
    fn require_known_pk_columns_rejects_an_unknown_column_name() {
        // The exact shape of the real gap: a `pk_*` field *name* an
        // attacker fully controls, crafted to break out of the `"..."`
        // identifier quoting a naive `format!` would otherwise use.
        let columns = [id_column()];
        let pk = [(r#"id" OR "1"="1"#.to_string(), Json::from(1))];
        let error = require_known_pk_columns(&columns, &pk).unwrap_err();
        assert!(matches!(
            error,
            AppError::Http {
                status: StatusCode::BAD_REQUEST,
                ..
            }
        ));
    }
}
