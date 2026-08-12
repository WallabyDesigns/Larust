use crate::guard;
use crate::Authenticatable;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::StatusCode;
use larust_core::AppError;
use larust_http::session::Session;

/// Extracts the currently authenticated user, or rejects with 401 if there
/// isn't one — for handlers that require a logged-in user and want the
/// `User` itself, not just a yes/no check. Pair with the `require_auth`
/// middleware (which redirects a guest to the login page) on routes meant
/// for browsers; use `Auth<U>` alone on routes meant to fail loudly instead
/// (e.g. an API endpoint).
///
/// For an optional current user (Laravel's nullable `Auth::user()`), call
/// [`crate::user`] directly instead of using this extractor.
pub struct Auth<U>(pub U);

// GOTCHAS.md: axum-core declares `FromRequestParts` via `#[async_trait]`,
// not native async-fn-in-traits — an impl written as a plain `async fn`
// fails with a confusing E0195 lifetime error instead of a clear message
// about the mismatch.
#[axum::async_trait]
impl<S, U> FromRequestParts<S> for Auth<U>
where
    S: Send + Sync,
    U: Authenticatable,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        // `Session`'s own `Rejection` is `(StatusCode, &'static str)`, not a
        // real `Error` impl (it can't go directly into `AppError::Internal`'s
        // `Box<dyn Error + Send + Sync>`) — it only ever fires if
        // `SessionManagerLayer` isn't installed on this router, i.e. a
        // developer misconfiguration, so it's always an internal error here.
        let session = Session::from_request_parts(parts, state)
            .await
            .map_err(|(_, message)| AppError::Internal(Box::new(std::io::Error::other(message))))?;

        guard::user::<U>(&session)
            .await?
            .map(Auth)
            .ok_or_else(|| AppError::Http {
                status: StatusCode::UNAUTHORIZED,
                message: "Unauthenticated.".to_string(),
            })
    }
}
