//! `/{DB_DASHBOARD_PATH}/*` (default `/xr-db`) - Larust's own database
//! admin dashboard: a small, auth-gated, server-rendered tool for
//! browsing and editing the app's *actual* SQL database (whatever
//! `DB_CONNECTION` it's configured for - SQLite/MySQL/Postgres), plus a
//! secondary view onto the embedded KV store this crate also provides.
//! The SQL side is the reason this exists - `users`, `posts`, every real
//! model an app has lives there, not in the KV store, which is a
//! deliberately separate, additive facade for app-local data that never
//! needed relations (see this crate's own top-level doc comment for that
//! design). This module is the framework's answer to "I want to poke at
//! my database during development without leaving Larust or installing a
//! separate SQL client" - the same job phpMyAdmin/Adminer do for PHP.
//!
//! **Carries the Larust brand, not the host app's.** This is a tool the
//! *framework* ships (the same posture Laravel's own Telescope/Horizon
//! dashboards take - their own fixed identity, regardless of whatever the
//! host app looks like), so its colors and mark (`>_` + "larust", [`STYLE`]
//! below) are the same ones `demo/resources/views/layouts/app.blade.xr` and
//! `demo/public/styles/style.css` use for the framework's own reference
//! app. `--brand`'s hex values are duplicated here rather than shared,
//! since this crate has no dependency on `demo` or any CSS build step of
//! its own - if that palette ever changes there, it should change here too.
//!
//! **Three sections, one login.** `/` (Database - table list/browse/edit),
//! `/sql` (raw SQL), `/kv` (the embedded KV store) all share the same
//! session, password, and mount-path gating - see [`sql_views`]/
//! [`kv_views`] for each section's own handlers, and this module's own
//! `STYLE`/`page_shell`/`nav_html` for the chrome all three render inside.
//!
//! **Mount path.** Configurable via `DB_DASHBOARD_PATH` (default `xr-db`) -
//! deliberately *not* under the `/__larust_*` internal-route convention
//! `wire`/`push`/`spa`/`reverb` use, since those are machine-only asset/
//! API endpoints nobody types into a browser, while this is a real page a
//! developer navigates to. Changing the path is obscurity, not security -
//! it's not a substitute for the two real gates below, only an extra one
//! on top for a team that wants a less-guessable URL.
//!
//! **`DB_DASHBOARD_PASSWORD`** (never the app's own `APP_KEY` or user
//! auth - this has nothing to do with app users) - the dashboard refuses
//! to serve at all if it's unset, fail closed rather than a default
//! password. Hashed once via the *existing* `larust_auth::
//! {hash_password, verify_password}` (argon2, no new crypto). Login sets
//! a dedicated session flag, mirroring `larust_auth::guard::login`/
//! `check`'s exact shape one-for-one but with its own key - not a second
//! auth mechanism, the same primitive reused. Separately, whether this
//! crate's `DbPlugin` is even registered at all is the generated app's
//! own choice (an `APP_DEBUG` gate baked into `routes/web.rs` at `xr new`
//! time - see `docs/ARCHITECTURE.md`) - this module doesn't assume that
//! and enforces its own password gate regardless.
//!
//! **Two different security postures for two different features.**
//! Structured browse/edit/insert/delete (`sql_views`) only ever
//! interpolates a table/column name into SQL after validating it against
//! that table's own freshly-introspected schema - every *value* is always
//! a bound parameter (`sql::codec::bind_any`), never interpolated as
//! text. The raw "Run SQL" page is deliberately the opposite: unrestricted
//! by design, the same way phpMyAdmin's own SQL tab is - its safety is the
//! password/`APP_DEBUG` double-gate above, not query validation, since
//! restricting it would defeat the entire point of a "run SQL" feature.
//!
//! CSRF is not handled here: every state-changing form embeds the
//! session's token (`larust_http::csrf::token`/`csrf::FIELD_NAME`), and
//! verification happens the same way it does for every other POST route in
//! this framework - the app's own top-level `.middleware(csrf::verify)`,
//! which (since the `Router::plugin` CSRF fix) already covers every
//! plugin-contributed route, this one included.

mod kv_views;
mod sql_views;

use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Redirect, Response};
use larust_core::AppError;
use larust_http::session::Session;
use serde::Deserialize;
use std::sync::OnceLock;

const SESSION_KEY: &str = "_larust_db_dashboard_authed";
const DEFAULT_DASHBOARD_PATH: &str = "xr-db";

static DASHBOARD_PATH: OnceLock<String> = OnceLock::new();

/// The dashboard's mount path segment (no leading/trailing slash),
/// computed once on first access from `DB_DASHBOARD_PATH` - falls back to
/// [`DEFAULT_DASHBOARD_PATH`] when unset, empty, or all slashes. Leading/
/// trailing slashes in the configured value are trimmed so
/// `DB_DASHBOARD_PATH=xr-db`, `=/xr-db`, and `=/xr-db/` all behave
/// identically.
pub(crate) fn dashboard_path() -> &'static str {
    DASHBOARD_PATH.get_or_init(|| {
        std::env::var("DB_DASHBOARD_PATH")
            .ok()
            .map(|raw| raw.trim_matches('/').to_string())
            .filter(|path| !path.is_empty())
            .unwrap_or_else(|| DEFAULT_DASHBOARD_PATH.to_string())
    })
}

/// The routes this crate needs, for [`larust_http::Router::plugin`]. Only
/// `/login` is reachable without a valid dashboard session - everything
/// else sits behind [`require_db_login`] via a nested `.group("", ...)`,
/// the same shape `ROUTES_WEB_HEADER_WITH_AUTH`'s own `require_auth` group
/// already uses. axum matches `/{base}` and `/{base}/` (and `/login` vs
/// `/login/`) as distinct routes - both registered so a typed or
/// bookmarked trailing slash doesn't 404.
pub struct DbPlugin;

impl larust_http::Plugin for DbPlugin {
    fn routes(&self) -> larust_http::Router {
        let base = dashboard_path();
        larust_http::Router::new()
            .get(&format!("/{base}/login"), login_form)
            .get(&format!("/{base}/login/"), login_form)
            .post(&format!("/{base}/login"), login)
            .group("", |r| {
                r.middleware(axum::middleware::from_fn(require_db_login))
                    .get(&format!("/{base}"), sql_views::table_list)
                    .get(&format!("/{base}/"), sql_views::table_list)
                    .get(&format!("/{base}/sql"), sql_views::sql_form)
                    .post(&format!("/{base}/sql"), sql_views::sql_run)
                    .get(&format!("/{base}/import"), sql_views::import_form)
                    .post(&format!("/{base}/import"), sql_views::import_run)
                    .post(&format!("/{base}/migrate/fresh"), sql_views::migrate_fresh)
                    .get(&format!("/{base}/t/{{table}}"), sql_views::browse)
                    .get(
                        &format!("/{base}/t/{{table}}/structure"),
                        sql_views::structure,
                    )
                    .get(&format!("/{base}/t/{{table}}/new"), sql_views::new_form)
                    .post(&format!("/{base}/t/{{table}}/insert"), sql_views::insert)
                    .get(&format!("/{base}/t/{{table}}/edit"), sql_views::edit_form)
                    .post(&format!("/{base}/t/{{table}}/update"), sql_views::update)
                    .post(&format!("/{base}/t/{{table}}/delete"), sql_views::delete)
                    .get(&format!("/{base}/kv"), kv_views::index)
                    .post(&format!("/{base}/kv/set"), kv_views::set)
                    .post(&format!("/{base}/kv/{{key}}/delete"), kv_views::delete)
                    .post(&format!("/{base}/logout"), logout)
            })
    }
}

static PASSWORD_HASH: OnceLock<Option<String>> = OnceLock::new();

/// The configured dashboard password's hash, computed once on first access
/// from `DB_DASHBOARD_PASSWORD` - `None` if that env var is unset or
/// empty, which is what makes every handler below fail closed.
fn configured_password_hash() -> Option<&'static str> {
    PASSWORD_HASH
        .get_or_init(|| {
            std::env::var("DB_DASHBOARD_PASSWORD")
                .ok()
                .filter(|password| !password.is_empty())
                .map(|password| {
                    larust_auth::hash_password(&password)
                        .expect("hashing the configured DB_DASHBOARD_PASSWORD")
                })
        })
        .as_deref()
}

fn dashboard_disabled() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "The embedded DB dashboard is disabled - set DB_DASHBOARD_PASSWORD in .env to enable it.",
    )
        .into_response()
}

async fn is_authed(session: &Session) -> Result<bool, AppError> {
    Ok(session
        .get::<bool>(SESSION_KEY)
        .await
        .map_err(internal)?
        .unwrap_or(false))
}

/// Gates every route except `/login` behind a valid dashboard session -
/// mirrors `larust_auth::middleware::require_auth`'s exact shape (fail
/// closed on a session-store error rather than letting the request
/// through).
pub async fn require_db_login(session: Session, request: Request, next: Next) -> Response {
    if configured_password_hash().is_none() {
        return dashboard_disabled();
    }
    match is_authed(&session).await {
        Ok(true) => next.run(request).await,
        Ok(false) => Redirect::to(&format!("/{}/login", dashboard_path())).into_response(),
        Err(error) => {
            tracing::warn!(%error, "db dashboard: failed to read session; denying access");
            Redirect::to(&format!("/{}/login", dashboard_path())).into_response()
        }
    }
}

pub async fn login_form(session: Session) -> Response {
    if configured_password_hash().is_none() {
        return dashboard_disabled();
    }
    let csrf = larust_http::csrf::token(&session).await;
    Html(render_login_page(&csrf, None)).into_response()
}

#[derive(Deserialize)]
pub struct LoginForm {
    password: String,
}

pub async fn login(session: Session, axum::Form(form): axum::Form<LoginForm>) -> Response {
    let Some(hash) = configured_password_hash() else {
        return dashboard_disabled();
    };
    let ok = larust_auth::verify_password(hash, &form.password).unwrap_or(false);
    if !ok {
        let csrf = larust_http::csrf::token(&session).await;
        return Html(render_login_page(&csrf, Some("Incorrect password."))).into_response();
    }
    if let Err(error) = session.insert(SESSION_KEY, true).await {
        tracing::warn!(%error, "db dashboard: failed to persist login session flag");
    }
    Redirect::to(&format!("/{}", dashboard_path())).into_response()
}

pub async fn logout(session: Session) -> Result<Redirect, AppError> {
    session
        .remove::<bool>(SESSION_KEY)
        .await
        .map_err(internal)?;
    Ok(Redirect::to(&format!("/{}/login", dashboard_path())))
}

/// `>_` + "larust" - the same mark and wordmark
/// `demo/resources/views/layouts/app.blade.xr` uses in its own header (see
/// this module's own doc comment for why this crate carries that identity
/// rather than a generic one).
fn brand_html(size_class: &str) -> String {
    format!(
        r#"<div class="brand {size_class}">
    <span class="brand-mark">&gt;_</span>
    <span class="brand-name">larust <strong>db</strong></span>
</div>"#
    )
}

/// Which of the three sections a page belongs to, for [`sidebar_html`]'s
/// active-tab highlighting.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    Database,
    Sql,
    Kv,
}

/// Percent-encodes a single URL *path* segment (RFC 3986 unreserved
/// characters pass through, everything else becomes `%XX`) - deliberately
/// not `form_urlencoded` (that crate targets
/// `application/x-www-form-urlencoded` query-string/form-body encoding,
/// where a space becomes `+`; a path segment has different escaping rules
/// entirely, and axum's own `Path` extractor percent-decodes, not
/// plus-decodes, so using the wrong encoder here would round-trip a table
/// name with a space incorrectly). Shared by `sql_views` (route-building)
/// and this module's own [`sidebar_html`] (the table nav list).
pub(crate) fn path_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The same mark `demo/public/images/favicon.svg` ships, inlined as a data
/// URI rather than served from a path - this crate has no dependency on
/// `demo`'s static files (or any given host app's), and a generated app
/// isn't guaranteed to keep that exact file in place, so embedding it here
/// is the only way every `/{base}` page reliably gets the Larust favicon
/// regardless of the host app's own asset layout.
const FAVICON_DATA_URI: &str = "data:image/svg+xml;base64,PD94bWwgdmVyc2lvbj0iMS4wIiBlbmNvZGluZz0iVVRGLTgiPz4KPHN2ZyBpZD0iTGF5ZXJfMSIgZGF0YS1uYW1lPSJMYXllciAxIiB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCA0OCA0OCI+CiAgPGRlZnM+CiAgICA8c3R5bGU+CiAgICAgIC5jbHMtMSB7CiAgICAgICAgZmlsbDogI2ZmZjsKICAgICAgfQoKICAgICAgLmNscy0yIHsKICAgICAgICBmaWxsOiAjZmY3MzVmOwogICAgICB9CiAgICA8L3N0eWxlPgogIDwvZGVmcz4KICA8cGF0aCBpZD0iUmVjdGFuZ2xlXzEiIGRhdGEtbmFtZT0iUmVjdGFuZ2xlIDEiIGNsYXNzPSJjbHMtMiIgZD0iTTEyLDBoMjRjNi42MywwLDEyLDUuMzcsMTIsMTJ2MjRjMCw2LjYzLTUuMzcsMTItMTIsMTJIMFYxMkMwLDUuMzcsNS4zNywwLDEyLDBaIi8+CiAgPGcgaWQ9IlBhdGhfMSIgZGF0YS1uYW1lPSJQYXRoIDEiPgogICAgPHBhdGggY2xhc3M9ImNscy0xIiBkPSJNMTMuMjUsMzAuNTljLS4yMywwLS40Ni0uMDgtLjY0LS4yMy0uNDItLjM2LS40OC0uOTktLjEyLTEuNDFsNC43LTUuNTktNC42OS01LjQyYy0uMzYtLjQyLS4zMi0xLjA1LjEtMS40MS40Mi0uMzYsMS4wNS0uMzIsMS40MS4xbDUuMjUsNi4wN2MuMzIuMzcuMzMuOTIsMCwxLjNsLTUuMjUsNi4yNGMtLjIuMjQtLjQ4LjM2LS43Ny4zNloiLz4KICA8L2c+CiAgPGcgaWQ9IkxpbmVfMSIgZGF0YS1uYW1lPSJMaW5lIDEiPgogICAgPHBhdGggY2xhc3M9ImNscy0xIiBkPSJNMzIuNzUsMzQuNzNoLTEyYy0uNTUsMC0xLS40NS0xLTFzLjQ1LTEsMS0xaDEyYy41NSwwLDEsLjQ1LDEsMXMtLjQ1LDEtMSwxWiIvPgogIDwvZz4KPC9zdmc+";

/// Wraps `body_html` in the shared page shell (doctype/head/[`STYLE`]) -
/// every full page in this module renders through this, so there's one
/// place that owns the `<html>`/`<head>` boilerplate.
pub(crate) fn page_shell(body_html: &str) -> String {
    format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>larust db</title><link rel="icon" href="{FAVICON_DATA_URI}" type="image/svg+xml">{STYLE}</head>
<body>
{body_html}
</body></html>
"#
    )
}

/// Just the logout button now - the brand mark moved into [`sidebar_html`]
/// once the top nav tabs became a left sidebar.
fn topbar_html(csrf: &str) -> String {
    let base = dashboard_path();
    format!(
        r#"<div class="topbar">
    <form method="post" action="/{base}/logout">
        <input type="hidden" name="{field}" value="{csrf}">
        <button type="submit" class="link-button">Log out</button>
    </form>
</div>"#,
        field = larust_http::csrf::FIELD_NAME,
        csrf = html_escape(csrf),
    )
}

/// The persistent left sidebar: brand mark, the 3-section switcher
/// (Database/SQL/Key-Value), and - only for the Database section - the
/// live table list (the currently-browsed table highlighted via
/// `active_table`), an "Import .sql" link, and the destructive "Fresh
/// migrate" action, visually separated as `.sidebar-danger`.
///
/// `tables` is fetched fresh by every Database-section caller (one extra
/// query per page load beyond what that page already needed) - an
/// accepted tradeoff for a dev-only tool, not worth caching machinery.
fn sidebar_html(
    base: &str,
    csrf: &str,
    active: Section,
    tables: &[String],
    active_table: Option<&str>,
) -> String {
    let tab = |section: Section, href: String, label: &str| {
        let class = if section == active {
            "nav-tab-v is-active"
        } else {
            "nav-tab-v"
        };
        format!(r#"<a class="{class}" href="{href}">{label}</a>"#)
    };
    let section_nav = format!(
        r#"<nav class="nav-tabs-vertical">
    {database}
    {sql}
    {kv}
</nav>"#,
        database = tab(Section::Database, format!("/{base}"), "Database"),
        sql = tab(Section::Sql, format!("/{base}/sql"), "SQL"),
        kv = tab(Section::Kv, format!("/{base}/kv"), "Key-Value"),
    );

    let database_extra = if active == Section::Database {
        let table_items: String = tables
            .iter()
            .map(|table| {
                let class = if Some(table.as_str()) == active_table {
                    "table-nav-item is-active"
                } else {
                    "table-nav-item"
                };
                format!(
                    r#"<a class="{class}" href="/{base}/t/{path}">{name}</a>"#,
                    path = path_segment(table),
                    name = html_escape(table),
                )
            })
            .collect();
        format!(
            r#"<div class="sidebar-section">
    <div class="sidebar-label">Tables</div>
    <nav class="table-nav-list">{table_items}</nav>
</div>
<div class="sidebar-section">
    <a class="sidebar-link" href="/{base}/import">Import .sql</a>
</div>
<div class="sidebar-danger">
    <form method="post" action="/{base}/migrate/fresh" onsubmit="return confirm('This drops every table and reapplies migrations from scratch. Continue?')">
        <input type="hidden" name="{field}" value="{csrf}">
        <button type="submit" class="button-ghost">Fresh migrate</button>
    </form>
</div>"#,
            field = larust_http::csrf::FIELD_NAME,
            csrf = html_escape(csrf),
        )
    } else {
        String::new()
    };

    format!(
        r#"{brand}
{section_nav}
{database_extra}"#,
        brand = brand_html("brand-compact"),
    )
}

/// Wraps `main_html` in the topbar + left-sidebar shell every logged-in
/// page under `/{base}` shares - the one place that owns the
/// `.dashboard-layout` structure, so every `sql_views`/`kv_views` handler
/// just supplies its own inner content.
pub(crate) fn page_frame(
    csrf: &str,
    active: Section,
    tables: &[String],
    active_table: Option<&str>,
    main_html: &str,
) -> String {
    format!(
        r#"<div class="wrap">
    {topbar}
    <div class="dashboard-layout">
        <aside class="sidebar">
            {sidebar}
        </aside>
        <main class="main-content">
            {main_html}
        </main>
    </div>
</div>"#,
        topbar = topbar_html(csrf),
        sidebar = sidebar_html(dashboard_path(), csrf, active, tables, active_table),
    )
}

fn render_login_page(csrf: &str, error: Option<&str>) -> String {
    let base = dashboard_path();
    let error_html = error
        .map(|message| format!(r#"<p class="error">{}</p>"#, html_escape(message)))
        .unwrap_or_default();
    page_shell(&format!(
        r#"<div class="wrap wrap-narrow">
    <div class="card card-centered">
        {brand}
        <p class="subtitle">Sign in to browse and edit</p>
        {error_html}
        <form method="post" action="/{base}/login">
            <input type="password" name="password" placeholder="Dashboard password" autofocus class="login-input">
            <input type="hidden" name="{field}" value="{csrf}">
            <button type="submit" class="button button-block">Log in</button>
        </form>
    </div>
</div>"#,
        brand = brand_html("brand-centered"),
        field = larust_http::csrf::FIELD_NAME,
        csrf = html_escape(csrf),
    ))
}

// Larust's own brand - the same colors and `>_`/"larust" mark
// `demo/public/styles/style.css` and its layout template use, kept in
// sync by hand (see this module's own doc comment for why this crate
// can't just depend on that stylesheet). Light values match `:root` there,
// dark values match its `[data-theme="dark"]` block - demo's own dark
// mode is a JS-driven toggle, this page has no JS at all, so it switches
// on `prefers-color-scheme` instead; the two mechanisms differ, the colors
// don't.
const STYLE: &str = r#"<style>
:root {
    --ink: #202124;
    --muted: #6b6d73;
    --paper: #fffdf9;
    --canvas: #f4f0e8;
    --line: #e4ddd2;
    --brand: #f4513d;
    --brand-dark: #cf3628;
    --brand-shadow: color-mix(in srgb, var(--brand) 19%, transparent);
    --danger: #8c3028;
    --danger-bg: #ffebe7;
    --danger-line: #f0c4bc;
    color-scheme: light dark;
}
@media (prefers-color-scheme: dark) {
    :root {
        --ink: #f6f1e8;
        --muted: #b8b1a8;
        --paper: #272522;
        --canvas: #181716;
        --line: #45413b;
        --brand: #ff735f;
        --brand-dark: #ff8a79;
        --danger: #ffb4a8;
        --danger-bg: #3a2320;
        --danger-line: #5c332c;
    }
}
* { box-sizing: border-box; }
body {
    margin: 0;
    min-height: 100vh;
    color: var(--ink);
    background: var(--canvas);
    font-family: Inter, ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif;
    line-height: 1.5;
}
.wrap { width: min(1200px, calc(100% - 40px)); margin: 48px auto; }
.wrap-narrow { width: min(420px, calc(100% - 40px)); margin: 96px auto; }
.dashboard-layout { display: flex; align-items: flex-start; gap: 28px; }
.sidebar { width: 220px; flex-shrink: 0; }
.main-content { flex: 1; min-width: 0; }
.nav-tabs-vertical { display: flex; flex-direction: column; gap: 2px; margin: 20px 0; }
.nav-tab-v {
    padding: 8px 10px;
    border-radius: 8px;
    color: var(--muted);
    font-weight: 650;
    font-size: .9rem;
}
.nav-tab-v:hover { background: var(--canvas); color: var(--ink); }
.nav-tab-v.is-active { color: var(--brand-dark); background: var(--brand-shadow); }
.sidebar-section { margin-bottom: 16px; }
.sidebar-label {
    padding: 0 10px;
    margin-bottom: 6px;
    color: var(--muted);
    font-size: .72rem;
    font-weight: 750;
    text-transform: uppercase;
    letter-spacing: .05em;
}
.table-nav-list { display: flex; flex-direction: column; gap: 1px; max-height: 50vh; overflow-y: auto; }
.table-nav-item {
    padding: 7px 10px;
    border-radius: 8px;
    color: var(--ink);
    font-size: .85rem;
    font-family: ui-monospace, "SF Mono", Consolas, monospace;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}
.table-nav-item:hover { background: var(--canvas); }
.table-nav-item.is-active { background: var(--brand-shadow); color: var(--brand-dark); font-weight: 650; }
.sidebar-link {
    display: block;
    padding: 7px 10px;
    color: var(--muted);
    font-size: .85rem;
    font-weight: 650;
    border-radius: 8px;
}
.sidebar-link:hover { background: var(--canvas); color: var(--ink); }
.sidebar-danger { padding-top: 16px; border-top: 1px solid var(--line); }
.sidebar-danger .button-ghost { width: 100%; color: var(--danger); box-shadow: inset 0 0 0 1px var(--danger-line); }
.sidebar-danger .button-ghost:hover { background: var(--danger-bg); }
.card {
    background: var(--paper);
    border: 1px solid var(--line);
    border-radius: 18px;
    padding: 30px 32px;
}
.card-centered { text-align: center; padding: 40px 36px; }
.card-centered form { text-align: left; }
.brand { display: inline-flex; gap: 12px; align-items: center; }
.brand-centered { display: flex; justify-content: center; gap: 16px; margin-bottom: 22px; }
.brand-mark {
    display: grid;
    place-items: center;
    flex-shrink: 0;
    width: 34px;
    height: 34px;
    color: #fff;
    background: var(--brand);
    border-radius: 10px 10px 10px 3px;
    font-family: ui-monospace, monospace;
    font-size: 1rem;
    font-weight: 700;
}
.brand-centered .brand-mark {
    width: 60px;
    height: 60px;
    border-radius: 18px 18px 18px 6px;
    font-size: 1.6rem;
}
.brand-name { font-size: 1.15rem; font-weight: 800; letter-spacing: -.03em; }
.brand-name strong { color: var(--brand); font-weight: 800; }
.brand-centered .brand-name { font-size: 1.6rem; }
h1 { margin: 0 0 4px; font-size: 1.3rem; letter-spacing: -.01em; }
h2 { margin: 0 0 16px; font-size: 1.05rem; letter-spacing: -.01em; }
.subtitle { margin: 6px 0 0; color: var(--muted); font-size: .88rem; }
.card-centered .subtitle { margin: 0 0 28px; font-size: .95rem; }
.topbar { display: flex; justify-content: space-between; align-items: center; margin-bottom: 18px; gap: 16px; }
.nav-tabs { display: flex; gap: 4px; margin-bottom: 20px; border-bottom: 1px solid var(--line); }
.nav-tab {
    padding: 9px 16px;
    color: var(--muted);
    font-weight: 650;
    font-size: .9rem;
    border-bottom: 2px solid transparent;
    margin-bottom: -1px;
}
.nav-tab:hover { color: var(--ink); }
.nav-tab.is-active { color: var(--brand-dark); border-bottom-color: var(--brand); }
.breadcrumb { margin: 0 0 16px; color: var(--muted); font-size: .85rem; }
.breadcrumb a { color: var(--brand-dark); font-weight: 650; }
.breadcrumb a:hover { text-decoration: underline; }
input[type="text"], input[type="password"], textarea {
    width: 100%;
    padding: 11px 14px;
    border: 1px solid var(--line);
    border-radius: 10px;
    background: var(--paper);
    color: var(--ink);
    font: inherit;
    outline: none;
}
textarea { font-family: ui-monospace, "SF Mono", Consolas, monospace; font-size: .88rem; min-height: 120px; resize: vertical; }
input[type="text"]:focus, input[type="password"]:focus, textarea:focus {
    border-color: var(--brand);
    box-shadow: 0 0 0 4px var(--brand-shadow);
}
input[readonly] { background: var(--canvas); color: var(--muted); }
.login-input { margin-bottom: 16px; }
.button {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border: 0;
    border-radius: 10px;
    padding: 11px 18px;
    background: var(--brand);
    color: #fff;
    font: inherit;
    font-weight: 750;
    cursor: pointer;
    white-space: nowrap;
    box-shadow: 0 6px 14px var(--brand-shadow);
    transition: .15s;
}
.button:hover { background: var(--brand-dark); transform: translateY(-1px); }
.button-block { width: 100%; }
.button-secondary {
    background: var(--paper);
    color: var(--ink);
    box-shadow: inset 0 0 0 1px var(--line);
}
.button-secondary:hover { background: var(--canvas); transform: none; }
.button-ghost {
    border: 0;
    background: transparent;
    color: var(--muted);
    box-shadow: inset 0 0 0 1px var(--line);
    border-radius: 8px;
    padding: 6px 12px;
    font: inherit;
    font-size: .85rem;
    font-weight: 650;
    cursor: pointer;
    transition: .15s;
}
.button-ghost:hover { background: var(--danger-bg); color: var(--danger); box-shadow: inset 0 0 0 1px var(--danger-line); }
.link-button {
    border: 0;
    background: transparent;
    color: var(--muted);
    font: inherit;
    font-size: .85rem;
    font-weight: 650;
    cursor: pointer;
    padding: 2px;
}
.link-button:hover { color: var(--brand-dark); text-decoration: underline; }
.set-row { display: flex; gap: 8px; margin-bottom: 24px; }
.set-row input[type="text"]:first-child { flex: 0 0 200px; }
.set-row input[type="text"]:nth-child(2) { flex: 1; }
table { width: 100%; border-collapse: collapse; }
th {
    text-align: left;
    padding: 8px 10px;
    border-bottom: 1px solid var(--line);
    color: var(--muted);
    font-size: .72rem;
    font-weight: 750;
    text-transform: uppercase;
    letter-spacing: .05em;
    white-space: nowrap;
}
td { padding: 12px 10px; border-bottom: 1px solid var(--line); vertical-align: top; }
tr:last-child td { border-bottom: none; }
tr.is-clickable:hover { background: var(--canvas); }
.key-cell { font-family: ui-monospace, "SF Mono", Consolas, monospace; font-weight: 650; white-space: nowrap; }
.actions-cell { text-align: right; width: 1%; white-space: nowrap; }
.count-cell { color: var(--muted); text-align: right; width: 1%; white-space: nowrap; }
pre { margin: 0; font-family: ui-monospace, "SF Mono", Consolas, monospace; font-size: .82rem; white-space: pre-wrap; word-break: break-word; color: var(--muted); }
.null-value { color: var(--muted); font-style: italic; }
.empty-state { padding: 32px 8px; text-align: center; color: var(--muted); }
.empty-state p { margin: 0 0 6px; }
.empty-hint { font-size: .85rem; }
.empty-hint code { background: var(--canvas); border-radius: 4px; padding: 2px 6px; font-family: ui-monospace, "SF Mono", Consolas, monospace; }
.error {
    margin: 0 0 16px;
    padding: 10px 14px;
    border-radius: 10px;
    color: var(--danger);
    background: var(--danger-bg);
    font-size: .88rem;
    font-weight: 650;
}
.form-field { display: grid; gap: 6px; margin-bottom: 16px; }
.form-field label { font-size: .82rem; font-weight: 700; color: var(--muted); }
.form-actions { display: flex; gap: 10px; margin-top: 20px; }
.pagination { display: flex; justify-content: space-between; align-items: center; margin-top: 20px; font-size: .85rem; color: var(--muted); }
.pagination a { color: var(--brand-dark); font-weight: 650; }
.pagination a:hover { text-decoration: underline; }
.toolbar { display: flex; justify-content: space-between; align-items: center; margin-bottom: 16px; gap: 12px; }
</style>"#;

/// Every value embedded above is either app-controlled data (a KV/SQL
/// value, a table/column name) or another route's own generated token -
/// never assume it's safe to write into HTML verbatim.
pub(crate) fn html_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub(crate) fn internal<E: std::error::Error + Send + Sync + 'static>(error: E) -> AppError {
    AppError::Internal(Box::new(error))
}
