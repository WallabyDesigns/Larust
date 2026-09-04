//! The embedded KV store section (`/{base}/kv`) - the same `Set`/browse/
//! delete flow this dashboard has always had, moved here unchanged in
//! behavior once the Database section (`sql_views`) became the primary
//! reason this dashboard exists. See `dashboard/mod.rs`'s own doc comment
//! for the three-section framing.

use super::{dashboard_path, html_escape, page_frame, page_shell, Section};
use axum::extract::{Form, Path};
use axum::response::{Html, Redirect};
use larust_core::AppError;
use larust_http::session::Session;
use serde::Deserialize;

pub async fn index(session: Session) -> Result<Html<String>, AppError> {
    let base = dashboard_path();
    let csrf = larust_http::csrf::token(&session).await;
    let mut keys = crate::keys().await?;
    keys.sort_unstable();

    let mut rows = String::new();
    for key in &keys {
        let value = crate::get_raw(key)
            .await?
            .unwrap_or(serde_json::Value::Null);
        let pretty = serde_json::to_string_pretty(&value).unwrap_or_default();
        rows.push_str(&format!(
            r#"<tr>
    <td class="key-cell">{key}</td>
    <td><pre>{value}</pre></td>
    <td class="actions-cell">
        <form method="post" action="/{base}/kv/{key}/delete">
            <input type="hidden" name="{field}" value="{csrf}">
            <button type="submit" class="button-ghost">Delete</button>
        </form>
    </td>
</tr>
"#,
            key = html_escape(key),
            value = html_escape(&pretty),
            field = larust_http::csrf::FIELD_NAME,
            csrf = html_escape(&csrf),
        ));
    }
    let table_or_empty = if rows.is_empty() {
        r#"<div class="empty-state">
    <p>No keys yet.</p>
    <p class="empty-hint">Add one above, or from the CLI: <code>xr db:put &lt;key&gt; &lt;value&gt;</code></p>
</div>"#
            .to_string()
    } else {
        format!(
            r#"<table>
<thead><tr><th>Key</th><th>Value</th><th></th></tr></thead>
<tbody>
{rows}
</tbody>
</table>"#
        )
    };

    let main_html = format!(
        r#"<p class="subtitle">{count} {key_word} stored - app-local data with no relations, separate from the real database above</p>

    <div class="card">
        <form method="post" action="/{base}/kv/set" class="set-row">
            <input type="text" name="key" placeholder="key" required>
            <input type="text" name="value" placeholder="value - JSON or plain text">
            <input type="hidden" name="{field}" value="{csrf}">
            <button type="submit" class="button">Set</button>
        </form>

        {table_or_empty}
    </div>"#,
        field = larust_http::csrf::FIELD_NAME,
        csrf = html_escape(&csrf),
        count = keys.len(),
        key_word = if keys.len() == 1 { "key" } else { "keys" },
    );
    Ok(Html(page_shell(&page_frame(
        &csrf,
        Section::Kv,
        &[],
        None,
        &main_html,
    ))))
}

#[derive(Deserialize)]
pub struct SetForm {
    key: String,
    value: String,
}

pub async fn set(Form(form): Form<SetForm>) -> Result<Redirect, AppError> {
    let value = crate::parse_cli_value(&form.value);
    crate::put_raw(&form.key, value).await?;
    Ok(Redirect::to(&format!("/{}/kv", dashboard_path())))
}

pub async fn delete(Path(key): Path<String>) -> Result<Redirect, AppError> {
    crate::forget(&key).await?;
    Ok(Redirect::to(&format!("/{}/kv", dashboard_path())))
}
