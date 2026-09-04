use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::routing::{get, post};
use axum::Router;
use larust_http::csrf;
use larust_http::session::{session_layer as build_session_layer, Session};
use tower::ServiceExt;

async fn show_token(session: Session) -> String {
    csrf::token(&session).await
}

async fn submit() -> &'static str {
    "ok"
}

/// Every test in this file shares one process-wide pool -
/// `larust_orm::connect()` is a real once-per-process singleton (like
/// every other test suite in this codebase that uses it), so the first
/// call here wins and every later call's "already connected" error is
/// deliberately swallowed. A real temp-file database, not
/// `sqlite::memory:`: a pool can open more than one physical connection,
/// and pooled `:memory:` connections each get their own private, empty
/// database without explicit shared-cache URI mode - the same reasoning
/// `larust_testing::db::test_db`'s own doc comment gives for avoiding it.
/// Harmless for what these tests exercise (CSRF token round-tripping
/// through independent request pairs, never cross-test data isolation).
async fn shared_pool() -> sqlx::AnyPool {
    let dir = tempfile::tempdir().unwrap().keep();
    let database_url = format!("sqlite://{}/test.sqlite", dir.display());
    let _ = larust_orm::connect(&database_url).await;
    larust_orm::pool().unwrap().clone()
}

/// Exercises the same `AnySessionStore`/migration code path production
/// uses, against the shared pool above.
async fn app() -> Router {
    let pool = shared_pool().await;
    let session_layer = build_session_layer(&pool, true).await.unwrap();
    Router::new()
        .route("/token", get(show_token))
        .route("/submit", post(submit))
        .layer(axum::middleware::from_fn(csrf::verify))
        .layer(session_layer)
}

/// Fetches a CSRF token and returns `(token, session_cookie)` from `router`
/// - the SAME router (same backing store) must be reused for the follow-up
/// request, or the session/token won't exist there at all.
async fn fetch_token(router: &Router) -> (String, String) {
    let response = router
        .clone()
        .oneshot(Request::get("/token").body(Body::empty()).unwrap())
        .await
        .unwrap();

    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .expect("session cookie should be set")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let token = String::from_utf8(body.to_vec()).unwrap();

    (token, cookie)
}

fn post_with(cookie: Option<&str>, body: &str) -> Request<Body> {
    let mut builder =
        Request::post("/submit").header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

#[tokio::test]
async fn get_requests_are_not_checked() {
    let response = app()
        .await
        .oneshot(Request::get("/token").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn post_with_correct_token_and_matching_session_succeeds() {
    let router = app().await;
    let (token, cookie) = fetch_token(&router).await;

    let response = router
        .oneshot(post_with(Some(&cookie), &format!("_csrf_token={token}")))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn post_with_no_token_is_rejected() {
    let router = app().await;
    let (_token, cookie) = fetch_token(&router).await;

    let response = router.oneshot(post_with(Some(&cookie), "")).await.unwrap();

    assert_eq!(response.status(), StatusCode::from_u16(419).unwrap());
}

#[tokio::test]
async fn post_with_wrong_token_is_rejected() {
    let router = app().await;
    let (_token, cookie) = fetch_token(&router).await;

    let response = router
        .oneshot(post_with(Some(&cookie), "_csrf_token=not-the-real-token"))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::from_u16(419).unwrap());
}

#[tokio::test]
async fn post_with_correct_token_but_different_session_is_rejected() {
    let router = app().await;
    let (token, _cookie) = fetch_token(&router).await;

    // No cookie replayed: this is a fresh session with its own (different)
    // token, so the "correct" token from a different session must fail.
    let response = router
        .oneshot(post_with(None, &format!("_csrf_token={token}")))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::from_u16(419).unwrap());
}

#[tokio::test]
async fn post_with_correct_token_in_header_succeeds_with_no_body_field_at_all() {
    // No form field, no Content-Type even set - a JS-driven request has
    // nowhere else to put the token, so the header alone must be enough.
    let router = app().await;
    let (token, cookie) = fetch_token(&router).await;

    let response = router
        .oneshot(
            Request::post("/submit")
                .header(header::COOKIE, &cookie)
                .header(csrf::HEADER_NAME, &token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_wrong_header_is_rejected_even_with_a_correct_token_in_the_body() {
    // Proves the header, once present, is the *only* source checked - not
    // "check header, fall back to body if that fails". A wrong header must
    // reject outright, regardless of what the body separately contains.
    let router = app().await;
    let (token, cookie) = fetch_token(&router).await;

    let response = router
        .oneshot(
            Request::post("/submit")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(csrf::HEADER_NAME, "not-the-real-token")
                .body(Body::from(format!("_csrf_token={token}")))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::from_u16(419).unwrap());
}

#[tokio::test]
async fn a_header_authenticated_request_skips_the_bodys_2mb_read_cap() {
    // The form-urlencoded fallback path caps the body read at 2MB (see
    // `csrf::verify`) - a header-present request must never hit that path
    // at all, since a real upload routinely exceeds it. A >2MB body that
    // still succeeds is exactly what proves the body was never read here.
    let router = app().await;
    let (token, cookie) = fetch_token(&router).await;

    let oversized_body = vec![b'a'; 3 * 1024 * 1024];
    let response = router
        .oneshot(
            Request::post("/submit")
                .header(header::COOKIE, &cookie)
                .header(csrf::HEADER_NAME, &token)
                .body(Body::from(oversized_body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}
