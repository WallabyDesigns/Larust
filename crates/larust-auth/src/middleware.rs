//! Route middleware (Laravel's `auth`/`guest` middleware aliases).

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use larust_http::session::Session;

use crate::guard;

/// Redirects a guest to `"login"` (falling back to a hardcoded `/login`,
/// with a warning, if that route name isn't registered — the same degrade
/// pattern `larust_support::redirect()->route()` already uses) rather than
/// running the handler. Pair with routes that require a logged-in user
/// (Laravel's `auth` middleware); use the [`crate::Auth`] extractor instead
/// on routes that should fail with a 401 rather than redirect.
pub async fn require_auth(session: Session, request: Request, next: Next) -> Response {
    match guard::check(&session).await {
        Ok(true) => next.run(request).await,
        Ok(false) => Redirect::to(&login_path()).into_response(),
        // Fail closed: a session-store error is treated the same as "not
        // authenticated" rather than letting the request through.
        Err(error) => {
            tracing::warn!(%error, "require_auth: failed to read session; denying access");
            Redirect::to(&login_path()).into_response()
        }
    }
}

/// The inverse of [`require_auth`]: bounces an already-logged-in user away
/// from guest-only routes (Laravel's `guest` middleware — typically wrapped
/// around `/login`/`/register`) to `"/"`, rather than running the handler.
pub async fn redirect_authenticated(session: Session, request: Request, next: Next) -> Response {
    match guard::check(&session).await {
        Ok(true) => Redirect::to("/").into_response(),
        Ok(false) => next.run(request).await,
        // If we can't tell whether the session is authenticated, don't
        // assume it is: err toward letting the guest-only page render
        // rather than silently bouncing to "/".
        Err(error) => {
            tracing::warn!(%error, "redirect_authenticated: failed to read session; allowing access");
            next.run(request).await
        }
    }
}

fn login_path() -> String {
    larust_http::resolve_route_name("login").unwrap_or_else(|| {
        tracing::warn!(
            "require_auth: no route named `login` is registered; falling back to /login"
        );
        "/login".to_string()
    })
}
