//! The Database and SQL sections (`/{base}`, `/{base}/t/*`,
//! `/{base}/sql`) - table list, row browsing/insert/edit/delete, and the
//! raw SQL box. See `dashboard/mod.rs`'s own doc comment for the security
//! posture split between the two: structured CRUD here always validates
//! `{table}` against a live introspected table list before building any
//! SQL with it, and always binds values as parameters
//! (`sql::codec::bind_any`) rather than interpolating them - the raw SQL
//! box is the one deliberate exception to both, by design.

use super::{dashboard_path, html_escape, page_frame, page_shell, path_segment, Section};
use crate::sql::{introspect, mutate};
use axum::extract::{Form, Multipart, Path, Query};
use axum::http::StatusCode;
use axum::response::Html;
use larust_core::AppError;
use larust_http::session::Session;
use serde::Deserialize;
use serde_json::Value as Json;
use std::collections::HashMap;

const PAGE_SIZE: i64 = 50;

/// `table` must already be one of [`introspect::list_tables`]'s own
/// results before any SQL is built with it - every handler in this file
/// checks that first, turning a mistyped/unknown table into a 404 rather
/// than a SQL error or (worse) a raw identifier straight from user input.
async fn require_known_table(table: &str) -> Result<(), AppError> {
    let tables = introspect::list_tables().await?;
    if tables.iter().any(|t| t == table) {
        Ok(())
    } else {
        Err(AppError::NotFound)
    }
}

fn json_as_text(value: &Json) -> String {
    match value {
        Json::Null => String::new(),
        Json::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// An empty submitted field is treated as SQL `NULL`, not an empty
/// string - a real, stated v1 simplification (a plain text input can't
/// distinguish the two); the raw SQL page is the escape hatch for a
/// table that genuinely needs an empty string stored.
fn form_value(raw: &str) -> Json {
    if raw.is_empty() {
        Json::Null
    } else {
        Json::String(raw.to_string())
    }
}

fn extract_pk(map: &HashMap<String, String>) -> Vec<(String, Json)> {
    map.iter()
        .filter_map(|(k, v)| {
            k.strip_prefix("pk_")
                .map(|col| (col.to_string(), form_value(v)))
        })
        .collect()
}

fn pk_query_string(pk: &[(String, Json)]) -> String {
    let mut ser = form_urlencoded::Serializer::new(String::new());
    for (name, value) in pk {
        ser.append_pair(&format!("pk_{name}"), &json_as_text(value));
    }
    ser.finish()
}

fn pk_hidden_fields(pk: &[(String, Json)]) -> String {
    pk.iter()
        .map(|(name, value)| {
            format!(
                r#"<input type="hidden" name="pk_{}" value="{}">"#,
                html_escape(name),
                html_escape(&json_as_text(value))
            )
        })
        .collect()
}

fn pk_pairs(pk_columns: &[String], row: &Json) -> Vec<(String, Json)> {
    pk_columns
        .iter()
        .map(|col| (col.clone(), row.get(col).cloned().unwrap_or(Json::Null)))
        .collect()
}

fn render_cell(value: &Json) -> String {
    match value {
        Json::Null => r#"<span class="null-value">NULL</span>"#.to_string(),
        Json::String(s) => html_escape(s),
        other => html_escape(&other.to_string()),
    }
}

pub async fn table_list(session: Session) -> Result<Html<String>, AppError> {
    let base = dashboard_path();
    let csrf = larust_http::csrf::token(&session).await;
    let tables = introspect::list_tables().await?;

    let mut rows = String::new();
    for table in &tables {
        // Best-effort - a single table's count query failing (an odd
        // permission setup, a locked table) shouldn't break the whole
        // list; render it as "?" rather than losing the rest of the page.
        let count = mutate::run_raw(&format!("SELECT COUNT(*) AS cnt FROM \"{table}\""))
            .await
            .ok()
            .and_then(|rows| rows.into_iter().next())
            .and_then(|row| row.get("cnt").cloned())
            .map(|v| json_as_text(&v))
            .unwrap_or_else(|| "?".to_string());
        rows.push_str(&format!(
            r#"<tr>
    <td class="key-cell"><a href="/{base}/t/{table_path}">{table_esc}</a></td>
    <td class="count-cell">{count} rows</td>
</tr>
"#,
            table_path = path_segment(table),
            table_esc = html_escape(table),
        ));
    }
    let table_or_empty = if rows.is_empty() {
        r#"<div class="empty-state"><p>No tables found in the connected database.</p></div>"#
            .to_string()
    } else {
        format!(
            r#"<table><thead><tr><th>Table</th><th></th></tr></thead><tbody>{rows}</tbody></table>"#
        )
    };

    let main_html = format!(
        r#"<p class="subtitle">{n} {table_word} in the connected database</p>
    <div class="card">{table_or_empty}</div>"#,
        n = tables.len(),
        table_word = if tables.len() == 1 { "table" } else { "tables" },
    );
    Ok(Html(page_shell(&page_frame(
        &csrf,
        Section::Database,
        &tables,
        None,
        &main_html,
    ))))
}

#[derive(Deserialize)]
pub struct PageQuery {
    page: Option<i64>,
}

pub async fn browse(
    session: Session,
    Path(table): Path<String>,
    Query(query): Query<PageQuery>,
) -> Result<Html<String>, AppError> {
    require_known_table(&table).await?;
    let base = dashboard_path();
    let csrf = larust_http::csrf::token(&session).await;
    let tables = introspect::list_tables().await?;
    let columns = introspect::table_columns(&table).await?;
    let pk_columns = introspect::primary_key_columns(&table).await?;

    let page = query.page.unwrap_or(1).max(1);
    let offset = (page - 1) * PAGE_SIZE;
    let rows = mutate::run_raw(&format!(
        "SELECT * FROM \"{table}\" LIMIT {PAGE_SIZE} OFFSET {offset}"
    ))
    .await?;

    let thead: String = columns
        .iter()
        .map(|c| format!("<th>{}</th>", html_escape(&c.name)))
        .collect();

    let mut body_rows = String::new();
    for row in &rows {
        let cells: String = columns
            .iter()
            .map(|c| {
                format!(
                    "<td>{}</td>",
                    render_cell(row.get(&c.name).unwrap_or(&Json::Null))
                )
            })
            .collect();
        let actions = if pk_columns.is_empty() {
            // No primary key at all - can't safely target one row for
            // edit/delete (`larust_repository`'s own `Repository<T>`
            // trait requires an `Id` too; a PK-less table is already an
            // edge case elsewhere in this framework, not new here).
            r#"<span class="null-value">no primary key</span>"#.to_string()
        } else {
            let pk = pk_pairs(&pk_columns, row);
            format!(
                r#"<a href="/{base}/t/{table_path}/edit?{edit_query}" class="link-button">Edit</a>
                <form method="post" action="/{base}/t/{table_path}/delete" style="display:inline">
                    {pk_hidden}
                    <input type="hidden" name="{field}" value="{csrf}">
                    <button type="submit" class="button-ghost">Delete</button>
                </form>"#,
                table_path = path_segment(&table),
                edit_query = pk_query_string(&pk),
                pk_hidden = pk_hidden_fields(&pk),
                field = larust_http::csrf::FIELD_NAME,
                csrf = html_escape(&csrf),
            )
        };
        body_rows.push_str(&format!(
            "<tr>{cells}<td class=\"actions-cell\">{actions}</td></tr>\n"
        ));
    }
    let table_or_empty = if rows.is_empty() {
        r#"<div class="empty-state"><p>No rows on this page.</p></div>"#.to_string()
    } else {
        format!(
            r#"<div style="overflow-x:auto"><table><thead><tr>{thead}<th></th></tr></thead><tbody>{body_rows}</tbody></table></div>"#
        )
    };

    let has_more = rows.len() as i64 == PAGE_SIZE;
    let pagination = format!(
        r#"<div class="pagination">
    <span>{prev}</span>
    <span>Page {page}</span>
    <span>{next}</span>
</div>"#,
        prev = if page > 1 {
            format!(
                r#"<a href="/{base}/t/{table_path}?page={prev_page}">&larr; Previous</a>"#,
                table_path = path_segment(&table),
                prev_page = page - 1
            )
        } else {
            String::new()
        },
        next = if has_more {
            format!(
                r#"<a href="/{base}/t/{table_path}?page={next_page}">Next &rarr;</a>"#,
                table_path = path_segment(&table),
                next_page = page + 1
            )
        } else {
            String::new()
        },
    );

    let main_html = format!(
        r#"<p class="breadcrumb"><a href="/{base}">Database</a> / {table_esc}</p>
    <div class="nav-tabs">
        <a class="nav-tab is-active" href="/{base}/t/{table_path}">Browse</a>
        <a class="nav-tab" href="/{base}/t/{table_path}/structure">Structure</a>
    </div>
    <div class="toolbar">
        <h1>{table_esc}</h1>
        <a href="/{base}/t/{table_path}/new" class="button">New row</a>
    </div>
    <div class="card">
        {table_or_empty}
        {pagination}
    </div>"#,
        table_esc = html_escape(&table),
        table_path = path_segment(&table),
    );
    Ok(Html(page_shell(&page_frame(
        &csrf,
        Section::Database,
        &tables,
        Some(&table),
        &main_html,
    ))))
}

/// `readonly_reason`: `None` renders a normal editable input;
/// `Some(reason)` renders a disabled, unnamed input (never submitted) with
/// `reason` shown in the label - used for both a primary-key column (never
/// rewritten by an edit form) and a `Blob`-kind column (see
/// `introspect::ColumnInfo`'s own doc comment for why those aren't
/// editable here).
fn field_input(
    name: &str,
    value: Option<&str>,
    not_null: bool,
    readonly_reason: Option<&str>,
) -> String {
    let value_attr = value.map(html_escape).unwrap_or_default();
    if let Some(reason) = readonly_reason {
        format!(
            r#"<div class="form-field">
    <label>{name} ({reason})</label>
    <input type="text" value="{value_attr}" readonly>
</div>"#,
            name = html_escape(name),
        )
    } else {
        let label_suffix = if not_null { " *" } else { "" };
        let placeholder = if not_null { "" } else { "leave blank for NULL" };
        format!(
            r#"<div class="form-field">
    <label for="f_{name}">{name}{label_suffix}</label>
    <input type="text" id="f_{name}" name="{name}" value="{value_attr}" placeholder="{placeholder}">
</div>"#,
            name = html_escape(name),
        )
    }
}

pub async fn new_form(
    session: Session,
    Path(table): Path<String>,
) -> Result<Html<String>, AppError> {
    require_known_table(&table).await?;
    let base = dashboard_path();
    let csrf = larust_http::csrf::token(&session).await;
    let tables = introspect::list_tables().await?;
    let columns = introspect::table_columns(&table).await?;

    let fields: String = columns
        .iter()
        .map(|c| {
            let readonly_reason = (c.kind == sqlx::any::AnyTypeInfoKind::Blob)
                .then_some("binary data, not editable here");
            field_input(&c.name, None, c.not_null, readonly_reason)
        })
        .collect();

    let main_html = format!(
        r#"<p class="breadcrumb"><a href="/{base}">Database</a> / <a href="/{base}/t/{table_path}">{table_esc}</a> / New row</p>
    <div class="card">
        <h2>New row in {table_esc}</h2>
        <form method="post" action="/{base}/t/{table_path}/insert">
            {fields}
            <input type="hidden" name="{field}" value="{csrf}">
            <div class="form-actions">
                <button type="submit" class="button">Create</button>
                <a href="/{base}/t/{table_path}" class="button button-secondary">Cancel</a>
            </div>
        </form>
    </div>"#,
        table_esc = html_escape(&table),
        table_path = path_segment(&table),
        field = larust_http::csrf::FIELD_NAME,
        csrf = html_escape(&csrf),
    );
    Ok(Html(page_shell(&page_frame(
        &csrf,
        Section::Database,
        &tables,
        Some(&table),
        &main_html,
    ))))
}

/// A column is editable through the structured insert/update forms when
/// it's both a real column of `table` and not `Blob`-kind - see
/// `introspect::ColumnInfo`'s own doc comment for why blobs aren't
/// editable here. Checked again at this layer (not just by the form
/// rendering `readonly`/omitting the input) as the actual guard: a
/// disabled `<input>` never gets submitted by a browser, but nothing stops
/// a direct HTTP client from sending the field name anyway.
fn is_editable(columns: &[introspect::ColumnInfo], name: &str) -> bool {
    columns
        .iter()
        .any(|c| c.name == name && c.kind != sqlx::any::AnyTypeInfoKind::Blob)
}

pub async fn insert(
    Path(table): Path<String>,
    Form(form): Form<HashMap<String, String>>,
) -> Result<axum::response::Redirect, AppError> {
    require_known_table(&table).await?;
    let columns = introspect::table_columns(&table).await?;
    let values: Vec<(String, Json)> = form
        .iter()
        .filter(|(name, _)| is_editable(&columns, name))
        .map(|(name, raw)| (name.clone(), form_value(raw)))
        .collect();
    mutate::insert_row(&table, &columns, &values).await?;
    Ok(axum::response::Redirect::to(&format!(
        "/{}/t/{}",
        dashboard_path(),
        path_segment(&table)
    )))
}

pub async fn edit_form(
    session: Session,
    Path(table): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Html<String>, AppError> {
    require_known_table(&table).await?;
    let base = dashboard_path();
    let csrf = larust_http::csrf::token(&session).await;
    let tables = introspect::list_tables().await?;
    let columns = introspect::table_columns(&table).await?;
    let pk = extract_pk(&query);
    if pk.is_empty() {
        return Err(AppError::Http {
            status: StatusCode::BAD_REQUEST,
            message: "missing primary key in the edit link".to_string(),
        });
    }
    let Some(row) = mutate::fetch_row(&table, &columns, &pk).await? else {
        return Err(AppError::NotFound);
    };
    let pk_names: Vec<&str> = pk.iter().map(|(name, _)| name.as_str()).collect();

    let fields: String = columns
        .iter()
        .map(|c| {
            let value = row.get(&c.name).map(json_as_text);
            let readonly_reason = if pk_names.contains(&c.name.as_str()) {
                Some("primary key")
            } else if c.kind == sqlx::any::AnyTypeInfoKind::Blob {
                Some("binary data, not editable here")
            } else {
                None
            };
            field_input(&c.name, value.as_deref(), c.not_null, readonly_reason)
        })
        .collect();

    let main_html = format!(
        r#"<p class="breadcrumb"><a href="/{base}">Database</a> / <a href="/{base}/t/{table_path}">{table_esc}</a> / Edit row</p>
    <div class="card">
        <h2>Edit row in {table_esc}</h2>
        <form method="post" action="/{base}/t/{table_path}/update">
            {fields}
            {pk_hidden}
            <input type="hidden" name="{field}" value="{csrf}">
            <div class="form-actions">
                <button type="submit" class="button">Save</button>
                <a href="/{base}/t/{table_path}" class="button button-secondary">Cancel</a>
            </div>
        </form>
    </div>"#,
        table_esc = html_escape(&table),
        table_path = path_segment(&table),
        pk_hidden = pk_hidden_fields(&pk),
        field = larust_http::csrf::FIELD_NAME,
        csrf = html_escape(&csrf),
    );
    Ok(Html(page_shell(&page_frame(
        &csrf,
        Section::Database,
        &tables,
        Some(&table),
        &main_html,
    ))))
}

pub async fn update(
    Path(table): Path<String>,
    Form(form): Form<HashMap<String, String>>,
) -> Result<axum::response::Redirect, AppError> {
    require_known_table(&table).await?;
    let columns = introspect::table_columns(&table).await?;
    let pk = extract_pk(&form);
    let values: Vec<(String, Json)> = form
        .iter()
        .filter(|(name, _)| !name.starts_with("pk_") && is_editable(&columns, name))
        .map(|(name, raw)| (name.clone(), form_value(raw)))
        .collect();
    mutate::update_row(&table, &columns, &pk, &values).await?;
    Ok(axum::response::Redirect::to(&format!(
        "/{}/t/{}",
        dashboard_path(),
        path_segment(&table)
    )))
}

pub async fn delete(
    Path(table): Path<String>,
    Form(form): Form<HashMap<String, String>>,
) -> Result<axum::response::Redirect, AppError> {
    require_known_table(&table).await?;
    let columns = introspect::table_columns(&table).await?;
    let pk = extract_pk(&form);
    mutate::delete_row(&table, &columns, &pk).await?;
    Ok(axum::response::Redirect::to(&format!(
        "/{}/t/{}",
        dashboard_path(),
        path_segment(&table)
    )))
}

pub async fn sql_form(session: Session) -> Result<Html<String>, AppError> {
    let base = dashboard_path();
    let csrf = larust_http::csrf::token(&session).await;
    let main_html = format!(
        r#"<div class="card">
        <h2>Run SQL</h2>
        <form method="post" action="/{base}/sql">
            <div class="form-field">
                <textarea name="sql" placeholder="SELECT * FROM users LIMIT 10" required></textarea>
            </div>
            <input type="hidden" name="{field}" value="{csrf}">
            <div class="form-actions">
                <button type="submit" class="button">Run</button>
            </div>
        </form>
    </div>"#,
        field = larust_http::csrf::FIELD_NAME,
        csrf = html_escape(&csrf),
    );
    Ok(Html(page_shell(&page_frame(
        &csrf,
        Section::Sql,
        &[],
        None,
        &main_html,
    ))))
}

#[derive(Deserialize)]
pub struct SqlForm {
    sql: String,
}

pub async fn sql_run(
    session: Session,
    Form(form): Form<SqlForm>,
) -> Result<Html<String>, AppError> {
    let base = dashboard_path();
    let csrf = larust_http::csrf::token(&session).await;

    let (result_html, error_html) = match mutate::run_raw(&form.sql).await {
        Ok(rows) => (
            render_rows_table(&rows, "Query executed - 0 rows returned."),
            String::new(),
        ),
        Err(error) => (
            String::new(),
            format!(
                r#"<p class="error">{}</p>"#,
                html_escape(&error.to_string())
            ),
        ),
    };

    let main_html = format!(
        r#"<div class="card">
        <h2>Run SQL</h2>
        <form method="post" action="/{base}/sql">
            <div class="form-field">
                <textarea name="sql" required>{sql}</textarea>
            </div>
            <input type="hidden" name="{field}" value="{csrf}">
            <div class="form-actions">
                <button type="submit" class="button">Run</button>
            </div>
        </form>
        {error_html}
        {result_html}
    </div>"#,
        sql = html_escape(&form.sql),
        field = larust_http::csrf::FIELD_NAME,
        csrf = html_escape(&csrf),
    );
    Ok(Html(page_shell(&page_frame(
        &csrf,
        Section::Sql,
        &[],
        None,
        &main_html,
    ))))
}

/// Renders a `Vec<serde_json::Value>` of `{column: value}` objects as a
/// generic HTML table - used for the raw `/sql` page's own result set, and
/// (via [`introspect::list_indexes`]/[`introspect::list_foreign_keys`]) for
/// the Structure page's index/foreign-key sections. One rendering path for
/// "some rows of unknown shape came back from the database", regardless of
/// which query produced them.
fn render_rows_table(rows: &[Json], empty_message: &str) -> String {
    if rows.is_empty() {
        return format!(r#"<p class="subtitle">{}</p>"#, html_escape(empty_message));
    }
    let Some(Json::Object(first)) = rows.first() else {
        return String::new();
    };
    let columns: Vec<&String> = first.keys().collect();
    let thead: String = columns
        .iter()
        .map(|c| format!("<th>{}</th>", html_escape(c)))
        .collect();
    let mut body = String::new();
    for row in rows {
        let cells: String = columns
            .iter()
            .map(|c| {
                format!(
                    "<td>{}</td>",
                    render_cell(row.get(c.as_str()).unwrap_or(&Json::Null))
                )
            })
            .collect();
        body.push_str(&format!("<tr>{cells}</tr>\n"));
    }
    format!(
        r#"<p class="subtitle">{n} row(s) returned.</p>
<div style="overflow-x:auto"><table><thead><tr>{thead}</tr></thead><tbody>{body}</tbody></table></div>"#,
        n = rows.len(),
    )
}

pub async fn structure(
    session: Session,
    Path(table): Path<String>,
) -> Result<Html<String>, AppError> {
    require_known_table(&table).await?;
    let base = dashboard_path();
    let csrf = larust_http::csrf::token(&session).await;
    let tables = introspect::list_tables().await?;
    let columns = introspect::table_columns(&table).await?;
    let pk_columns = introspect::primary_key_columns(&table).await?;
    let indexes = introspect::list_indexes(&table).await?;
    let foreign_keys = introspect::list_foreign_keys(&table).await?;

    let column_rows: String = columns
        .iter()
        .map(|c| {
            let pk_badge = if pk_columns.iter().any(|pk| pk == &c.name) {
                r#" <span class="null-value">PK</span>"#
            } else {
                ""
            };
            format!(
                "<tr><td class=\"key-cell\">{name}{pk_badge}</td><td>{kind:?}</td><td>{nullable}</td></tr>\n",
                name = html_escape(&c.name),
                kind = c.kind,
                nullable = if c.not_null { "NOT NULL" } else { "NULL" },
            )
        })
        .collect();

    let main_html = format!(
        r#"<p class="breadcrumb"><a href="/{base}">Database</a> / <a href="/{base}/t/{table_path}">{table_esc}</a> / Structure</p>
    <div class="nav-tabs">
        <a class="nav-tab" href="/{base}/t/{table_path}">Browse</a>
        <a class="nav-tab is-active" href="/{base}/t/{table_path}/structure">Structure</a>
    </div>
    <div class="card">
        <h2>Columns</h2>
        <div style="overflow-x:auto"><table><thead><tr><th>Column</th><th>Type</th><th>Nullable</th></tr></thead><tbody>{column_rows}</tbody></table></div>
    </div>
    <div class="card" style="margin-top:20px">
        <h2>Indexes</h2>
        {indexes_html}
    </div>
    <div class="card" style="margin-top:20px">
        <h2>Foreign keys</h2>
        {fk_html}
    </div>"#,
        table_esc = html_escape(&table),
        table_path = path_segment(&table),
        indexes_html = render_rows_table(&indexes, "No indexes on this table."),
        fk_html = render_rows_table(&foreign_keys, "No foreign keys on this table."),
    );
    Ok(Html(page_shell(&page_frame(
        &csrf,
        Section::Database,
        &tables,
        Some(&table),
        &main_html,
    ))))
}

/// Shared markup for both [`import_form`] (empty `error_html`/`result_html`)
/// and [`import_run`]'s re-render after a submission.
///
/// The CSRF middleware (`larust_http::csrf::verify`) only reads a submitted
/// `_csrf_token` *body field* for `application/x-www-form-urlencoded`
/// requests - its own doc comment states this is deliberate, since a plain
/// `multipart/form-data` upload could otherwise be capped/misparsed by its
/// 2MB body-read path. Its documented escape hatch is exactly what a
/// `multipart` upload needs anyway: send the token via the `X-CSRF-TOKEN`
/// header instead, which the middleware checks *before* touching the body
/// at all. A native `<form>` can't set a custom header, so this is the one
/// other deliberate JS exception in this otherwise JS-free dashboard
/// (alongside the "Fresh migrate" `confirm()`): intercept the submit,
/// re-send as `fetch` with the header, and swap the returned page in.
fn import_form_html(base: &str, csrf: &str, error_html: &str, result_html: &str) -> String {
    format!(
        r#"<div class="card">
        <h2>Import .sql</h2>
        <p class="subtitle">Uploads a <code>.sql</code> file and runs its full contents against the
        connected database - the same unrestricted-by-design execution as the Run SQL page.</p>
        <form method="post" action="/{base}/import" enctype="multipart/form-data" id="import-form">
            <div class="form-field">
                <input type="file" name="file" accept=".sql" required>
            </div>
            <input type="hidden" name="{field}" value="{csrf}">
            <div class="form-actions">
                <button type="submit" class="button">Import</button>
            </div>
        </form>
        {error_html}
        {result_html}
    </div>
    <script>
    document.getElementById('import-form').addEventListener('submit', async function (event) {{
        event.preventDefault();
        var form = event.target;
        var token = form.querySelector('input[name="{field}"]').value;
        var response = await fetch(form.action, {{
            method: 'POST',
            body: new FormData(form),
            headers: {{ '{header}': token }},
        }});
        var html = await response.text();
        document.open();
        document.write(html);
        document.close();
    }});
    </script>"#,
        field = larust_http::csrf::FIELD_NAME,
        csrf = html_escape(csrf),
        header = larust_http::csrf::HEADER_NAME,
    )
}

pub async fn import_form(session: Session) -> Result<Html<String>, AppError> {
    let base = dashboard_path();
    let csrf = larust_http::csrf::token(&session).await;
    let tables = introspect::list_tables().await?;
    let main_html = import_form_html(base, &csrf, "", "");
    Ok(Html(page_shell(&page_frame(
        &csrf,
        Section::Database,
        &tables,
        None,
        &main_html,
    ))))
}

pub async fn import_run(
    session: Session,
    mut multipart: Multipart,
) -> Result<Html<String>, AppError> {
    let base = dashboard_path();
    let csrf = larust_http::csrf::token(&session).await;
    let tables = introspect::list_tables().await?;

    let mut result_html = String::new();
    let mut error_html = String::new();
    let mut found_file = false;
    while let Some(field) = multipart.next_field().await.map_err(super::internal)? {
        if field.name() != Some("file") {
            continue;
        }
        found_file = true;
        let bytes = field.bytes().await.map_err(super::internal)?;
        match std::str::from_utf8(&bytes) {
            Ok(sql) => match mutate::run_script(sql).await {
                Ok(()) => {
                    result_html = r#"<p class="subtitle">Import ran successfully.</p>"#.to_string();
                }
                Err(error) => {
                    error_html = format!(
                        r#"<p class="error">{}</p>"#,
                        html_escape(&error.to_string())
                    );
                }
            },
            Err(_) => {
                error_html =
                    r#"<p class="error">The uploaded file isn't valid UTF-8 text.</p>"#.to_string();
            }
        }
    }
    if !found_file {
        error_html = r#"<p class="error">No file was uploaded.</p>"#.to_string();
    }

    let main_html = import_form_html(base, &csrf, &error_html, &result_html);
    Ok(Html(page_shell(&page_frame(
        &csrf,
        Section::Database,
        &tables,
        None,
        &main_html,
    ))))
}

/// The dashboard's own "Fresh migrate" button. Calling
/// `larust_orm::migrate_fresh` in-process (the original design) turns out
/// to be a dead end: `sqlx::Any`'s trait-object-based executor dispatch
/// makes the resulting future's `Send`-ness unprovable for *any* lifetime -
/// a genuine `rustc`/`sqlx::Any` limitation ("implementation of
/// `Executor`/`Send` is not general enough"), confirmed by isolating it
/// with a manual `Box::pin` + `assert_send::<T: Send>` probe. The identical
/// call compiles fine from a plain, non-generic `async fn main` (every
/// `xr <command>` dispatch already does this successfully) - it's only
/// axum's `Handler` machinery, which needs the future's `Send`-ness proven
/// generically, that trips over it. So this shells out to
/// `cargo run -- migrate:fresh` instead, the exact subprocess `xr
/// migrate:fresh` itself spawns (`crates/larust-cli/src/main.rs`'s
/// `run_app_subcommand`) - inheriting this already-running process's own
/// working directory (the app root, the same convention
/// `AppPaths::default()` relies on elsewhere).
pub async fn migrate_fresh(
    Form(_): Form<HashMap<String, String>>,
) -> Result<axum::response::Redirect, AppError> {
    let status = tokio::process::Command::new("cargo")
        .args(["run", "--quiet", "--", "migrate:fresh"])
        .status()
        .await
        .map_err(super::internal)?;
    if !status.success() {
        return Err(AppError::Internal(Box::new(std::io::Error::other(
            "migrate:fresh exited with a non-zero status",
        ))));
    }
    Ok(axum::response::Redirect::to(&format!(
        "/{}",
        dashboard_path()
    )))
}
