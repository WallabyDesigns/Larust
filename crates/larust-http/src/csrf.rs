use crate::random::random_hex;
use crate::session::Session;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

const SESSION_KEY: &str = "_csrf_token";
/// The form field / query param name a template's `@csrf` directive emits
/// and this middleware checks — matches Laravel's own `_token` convention
/// closely enough to be recognizable, kept distinct (`_csrf_token`) so it
/// doesn't collide with an app's own `token` field.
pub const FIELD_NAME: &str = "_csrf_token";
/// The header a JS-driven request (`fetch`/`XMLHttpRequest`) can send the
/// token in instead of a form field — matches Laravel's own `X-CSRF-TOKEN`
/// convention, typically sourced from a `<meta name="csrf-token">` tag.
/// `HeaderMap::get` is case-insensitive, so the exact casing here doesn't
/// matter for matching, only for what a client sees if it inspects the name.
pub const HEADER_NAME: &str = "X-CSRF-TOKEN";

/// Returns the current session's CSRF token, generating and storing one on
/// first use. Read this to embed the token in a form (the `@csrf`
/// directive does this automatically for templates that pass `csrf_token`
/// in their `view!` context).
pub async fn token(session: &Session) -> String {
    if let Ok(Some(existing)) = session.get::<String>(SESSION_KEY).await {
        return existing;
    }

    let generated = generate_token();
    // Best-effort: if the store write fails, the token is still returned
    // and usable for this response, it just won't be persisted for the
    // next request's verification — the next request will get a fresh
    // token and any in-flight form submission naturally fails CSRF
    // verification rather than silently succeeding.
    if let Err(error) = session.insert(SESSION_KEY, generated.clone()).await {
        tracing::warn!(%error, "failed to persist CSRF token to session store");
    }
    generated
}

fn generate_token() -> String {
    random_hex(32)
}

/// Verifies the CSRF token on state-changing requests (POST/PUT/PATCH/
/// DELETE) against the value stored in the session, rejecting with 419
/// (Laravel's own status code for this) on mismatch.
///
/// Checks the `X-CSRF-TOKEN` header *first*, before touching the body at
/// all — matching Laravel's own `VerifyCsrfToken` middleware, which checks
/// this header ahead of a form field for exactly the same reason: a
/// JS-driven request (`fetch`/`XMLHttpRequest`, not a submitted `<form>`)
/// has nowhere else to put the token, and reading it needs no assumption
/// about the body's content type at all. This is also what makes uploads
/// through this middleware possible in the first place — a header-present
/// request never reaches the body-read/2MB-cap path below, so a multipart
/// file (routinely well over 2MB, and never form-urlencoded) isn't capped
/// or misparsed here; the actual body is handled downstream by whatever
/// extractor the route uses. If the header is present, it's the *only*
/// source checked (present-but-wrong rejects immediately — this is
/// deliberately not "check header, then also check body if that fails",
/// which would make the two sources ambiguous about which is authoritative).
///
/// Falls back to reading the whole body and checking the submitted
/// `_csrf_token` form field only when the header is absent — unchanged
/// from before, still assumes `application/x-www-form-urlencoded` for that
/// path (matching plain `<form>` submissions, the only thing that ever
/// relied on it) — then reconstructs the request with the same bytes so
/// downstream extractors (`FormRequest`) can still parse it normally.
pub async fn verify(session: Session, request: Request, next: Next) -> Response {
    let is_state_changing = matches!(
        *request.method(),
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    );

    if !is_state_changing {
        return next.run(request).await;
    }

    let expected = token(&session).await;

    if let Some(header_value) = request
        .headers()
        .get(HEADER_NAME)
        .and_then(|value| value.to_str().ok())
    {
        return if header_value == expected {
            next.run(request).await
        } else {
            reject()
        };
    }

    let (parts, body) = request.into_parts();
    let bytes = match axum::body::to_bytes(body, 2 * 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid request body").into_response(),
    };

    let submitted = form_urlencoded::parse(&bytes)
        .find(|(key, _)| key == FIELD_NAME)
        .map(|(_, value)| value.into_owned());

    if submitted.as_deref() != Some(expected.as_str()) {
        return reject();
    }

    let request = Request::from_parts(parts, Body::from(bytes));
    next.run(request).await
}

fn reject() -> Response {
    (
        StatusCode::from_u16(419).expect("419 is a valid HTTP status code"),
        "CSRF token mismatch",
    )
        .into_response()
}
