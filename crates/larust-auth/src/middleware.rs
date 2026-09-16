//! Route middleware (Laravel's `auth`/`guest` middleware aliases).

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use larust_http::session::Session;

use crate::guard;
use crate::Authenticatable;

/// Redirects a guest to `"login"` (falling back to a hardcoded `/login`,
/// with a warning, if that route name isn't registered - the same degrade
/// pattern `larust_support::redirect()->route()` already uses) rather than
/// running the handler. Pair with routes that require a logged-in user
/// (Laravel's `auth` middleware); use the [`crate::Auth`] extractor instead
/// on routes that should fail with a 401 rather than redirect.
///
/// Checks the default (`"web"`) guard; for a named second guard, use
/// [`require_auth_for`] instead.
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

/// [`require_auth`]'s named-guard counterpart (Laravel's
/// `Auth::guard('admin')` protecting a route group) - redirects unless
/// `U::GUARD` specifically is logged in, regardless of whether some other
/// guard is. Used as `require_auth_for::<Admin>`, e.g.
/// `r.middleware(axum::middleware::from_fn(require_auth_for::<Admin>))`.
///
/// Redirects to a route named `"{guard}.login"` (e.g. `"admin.login"` for
/// `Admin::GUARD == "admin"`) rather than the plain `"login"` [`require_auth`]
/// uses - a named second guard almost always has its own, separate login
/// page. Falls back to plain `"login"`, then to a hardcoded `/login`, with
/// a warning at each step, if the more specific name isn't registered -
/// the same degrade pattern [`require_auth`] already uses.
pub async fn require_auth_for<U: Authenticatable>(
    session: Session,
    request: Request,
    next: Next,
) -> Response {
    match guard::check_for::<U>(&session).await {
        Ok(true) => next.run(request).await,
        Ok(false) => Redirect::to(&guard_login_path(U::GUARD)).into_response(),
        Err(error) => {
            tracing::warn!(%error, "require_auth_for: failed to read session; denying access");
            Redirect::to(&guard_login_path(U::GUARD)).into_response()
        }
    }
}

/// The inverse of [`require_auth`]: bounces an already-logged-in user away
/// from guest-only routes (Laravel's `guest` middleware - typically wrapped
/// around `/login`/`/register`) to `"/"`, rather than running the handler.
///
/// Checks the default (`"web"`) guard; for a named second guard, use
/// [`redirect_authenticated_for`] instead.
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

/// [`redirect_authenticated`]'s named-guard counterpart - bounces a user
/// already logged in as `U::GUARD` away from that guard's own guest-only
/// routes (e.g. `/admin/login`), regardless of whether some other guard is
/// also logged in.
pub async fn redirect_authenticated_for<U: Authenticatable>(
    session: Session,
    request: Request,
    next: Next,
) -> Response {
    match guard::check_for::<U>(&session).await {
        Ok(true) => Redirect::to("/").into_response(),
        Ok(false) => next.run(request).await,
        Err(error) => {
            tracing::warn!(%error, "redirect_authenticated_for: failed to read session; allowing access");
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

/// [`login_path`]'s named-guard counterpart: tries `"{guard}.login"` first
/// (e.g. `"admin.login"`), then falls back to plain `"login"`, then to a
/// hardcoded `/login` - each fallback step logs a warning naming which
/// route it expected.
fn guard_login_path(guard: &str) -> String {
    let guard_specific_name = format!("{guard}.login");
    larust_http::resolve_route_name(&guard_specific_name).unwrap_or_else(|| {
        tracing::warn!(
            "require_auth_for::<{guard}>: no route named `{guard_specific_name}` is \
             registered; falling back to the default `login` route"
        );
        login_path()
    })
}
