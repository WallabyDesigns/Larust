use crate::{debug, error_pages};
use axum::http::header::CONTENT_TYPE;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use thiserror::Error;

/// The framework's primary error type.
///
/// HTTP responses for `Config`/`Internal` only ever expose a generic
/// message - *unless* `APP_DEBUG=true` (see the `debug` module), in which
/// case the full message and source chain are rendered as an HTML page
/// instead. The wrapped source error (with full detail) is always logged
/// via `tracing` regardless of debug mode. Never enable `APP_DEBUG` outside
/// local development - see `docs/GOTCHAS.md`.
#[derive(Debug, Error)]
pub enum AppError {
    #[error("configuration error: {0}")]
    Config(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("not found")]
    NotFound,

    #[error("internal server error: {0}")]
    Internal(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// A specific HTTP status with a message safe to show clients (Laravel's
    /// `abort()`). Unlike `Config`/`Internal`, this message is sent as-is -
    /// callers are responsible for not putting sensitive detail in it.
    #[error("{message}")]
    Http { status: StatusCode, message: String },
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        // Computed once, up front, from thiserror's own derived `Display`
        // (`#[error("configuration error: {0}")]` / `#[error("internal
        // server error: {0}")]`) - the single source of truth for the
        // top-level message, used both for the log line below and (for
        // `Config`/`Internal`) as the debug-page detail's first line, so
        // the two can never silently drift apart the way two separately
        // hand-written literals could.
        let message = self.to_string();
        if matches!(self, AppError::Config(_) | AppError::Internal(_)) {
            tracing::error!(error = %message, "unhandled application error");
        }

        match self {
            AppError::NotFound => {
                if debug::is_enabled() {
                    debug_page(
                        StatusCode::NOT_FOUND,
                        "Not Found",
                        "No route matched this request.".to_string(),
                    )
                } else {
                    html_response(StatusCode::NOT_FOUND, error_pages::not_found_html())
                }
            }
            AppError::Http { status, message } => (status, message).into_response(),
            AppError::Config(source) | AppError::Internal(source) => {
                internal_response(&message, source.as_ref())
            }
        }
    }
}

/// Shared by both `AppError` variants that carry a boxed source error -
/// walks the full `source()` chain (each wrapped error, one level at a
/// time) so a debug-mode page shows e.g. the actual SQL driver error, not
/// just "internal server error". Capped so a pathological (e.g. cyclic)
/// third-party `source()` implementation can't hang the request or grow
/// the page unbounded - every error source in this codebase today
/// terminates in a handful of levels, so the cap is generous, not tight.
const MAX_SOURCE_CHAIN_DEPTH: u8 = 20;

fn internal_response(top_message: &str, source: &(dyn std::error::Error + 'static)) -> Response {
    if debug::is_enabled() {
        let mut detail = top_message.to_string();
        let mut cause = source.source();
        let mut depth = 0;
        while let Some(err) = cause {
            depth += 1;
            if depth > MAX_SOURCE_CHAIN_DEPTH {
                detail.push_str("\n\n... source chain truncated ...");
                break;
            }
            detail.push_str("\n\nCaused by:\n  ");
            detail.push_str(&err.to_string());
            cause = err.source();
        }
        debug_page(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error",
            detail,
        )
    } else {
        html_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            error_pages::internal_html(),
        )
    }
}

/// Same debug/production branching `AppError::Internal` uses, for a panic
/// caught by `Application::serve()`'s `CatchPanicLayer`. There's no
/// `AppError`/`std::error::Error` value for a panic - just its payload's
/// message - so this is a distinct entry point rather than routed through
/// `internal_response`, with no synthetic `source()` chain to walk.
pub(crate) fn render_panic(message: &str) -> Response {
    tracing::error!(error = %message, "panic in request handler");
    if debug::is_enabled() {
        debug_page(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error (panic)",
            message.to_string(),
        )
    } else {
        html_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            error_pages::internal_html(),
        )
    }
}

/// Shared by every production-mode branch above - builds the same
/// `text/html; charset=utf-8` response shape `debug_page` uses, just for
/// an already-fully-rendered page instead of one this module formats
/// itself.
fn html_response(status: StatusCode, html: String) -> Response {
    (status, [(CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
}

/// Self-contained (no external CSS/JS, no build step) - this has to render
/// standalone even as the very first response a broken app ever produces.
fn debug_page(status: StatusCode, title: &str, detail: String) -> Response {
    let html = format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>{status} {title}</title>
<style>
  body {{ font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; background: #1e1e1e; color: #d4d4d4; margin: 0; padding: 2rem; }}
  h1 {{ color: #f14c4c; font-size: 1.25rem; margin: 0 0 1rem; }}
  pre {{ white-space: pre-wrap; word-break: break-word; background: #252526; border: 1px solid #3c3c3c; border-radius: 6px; padding: 1rem; line-height: 1.5; }}
  p {{ color: #808080; font-size: 0.85rem; }}
</style>
</head>
<body>
<h1>{status} &mdash; {title}</h1>
<pre>{detail}</pre>
<p>Shown because <code>APP_DEBUG=true</code>. Never enable this outside local development.</p>
</body>
</html>"#,
        status = status.as_u16(),
        title = escape_html(title),
        detail = escape_html(&detail),
    );

    (status, [(CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}
