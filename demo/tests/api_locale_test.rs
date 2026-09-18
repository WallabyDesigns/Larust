//! End-to-end proof that `routes/api.rs`'s `locale::negotiate_from_header`
//! middleware actually makes a bearer-token API response locale-aware -
//! `ApiTokenController::store`'s credentials-mismatch message used to
//! always render in `Config::app_locale` (no session, nowhere else to read
//! a per-request locale from), which is exactly the limitation that
//! middleware exists to close: a caller now gets a translated error back
//! just by sending its own `Accept-Language` header, no session/cookie
//! involved at all.
//!
//! Driven via `tower::ServiceExt::oneshot` directly (not `TestClient`,
//! which has no way to attach an arbitrary header to a single request) -
//! same technique `theme_persist_test.rs` uses for the same reason.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use demo::controllers::ApiTokenController;
use larust_http::Route;
use larust_support::serde_json::{self, json};
use tower::ServiceExt;

fn build_router() -> axum::Router {
    Route::post("/tokens", ApiTokenController::store)
        .middleware(larust_http::axum::middleware::from_fn(
            larust_http::locale::negotiate_from_header,
        ))
        .into_axum_router()
}

#[tokio::test]
async fn bad_credentials_with_a_spanish_accept_language_header_get_a_spanish_error() {
    larust_core::Application::new(demo::config::app::config).unwrap();

    let migrations_dir = std::path::Path::new("database/migrations");
    larust_testing::test_transaction(migrations_dir, |_pool| async move {
        let router = build_router();

        let body = serde_json::to_vec(&json!({
            "email": "nobody@example.com",
            "password": "wrong-password",
            "device_name": "test-suite",
        }))
        .unwrap();
        let request = Request::post("/tokens")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT_LANGUAGE, "es-ES,es;q=0.9,en;q=0.8")
            .body(Body::from(body))
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            body.contains("no coinciden con nuestros registros"),
            "expected the Spanish `flash.invalid_credentials` translation in the response \
             body: {body}"
        );
    })
    .await;
}

#[tokio::test]
async fn bad_credentials_with_no_accept_language_header_fall_back_to_the_app_locale() {
    larust_core::Application::new(demo::config::app::config).unwrap();

    let migrations_dir = std::path::Path::new("database/migrations");
    larust_testing::test_transaction(migrations_dir, |_pool| async move {
        let router = build_router();

        let body = serde_json::to_vec(&json!({
            "email": "nobody@example.com",
            "password": "wrong-password",
            "device_name": "test-suite",
        }))
        .unwrap();
        let request = Request::post("/tokens")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            body.contains("don't match our records"),
            "expected the default English `flash.invalid_credentials` translation in the \
             response body: {body}"
        );
    })
    .await;
}
