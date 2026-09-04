//! Proves `DbPlugin` fails closed when `DB_DASHBOARD_PASSWORD` was never
//! set - a separate test binary from `dashboard_test.rs` (see that file's
//! own doc comment) since `configured_password_hash()`'s `OnceLock` caches
//! "unset" permanently on first access within a process, and this process
//! must never touch that env var at all for this scenario to mean anything.

use axum::body::Body;
use axum::extract::Request;
use axum::http::StatusCode;
use larust_db::DbPlugin;
use tower::ServiceExt;

#[tokio::test]
async fn dashboard_refuses_to_serve_without_a_configured_password() {
    let dir = tempfile::tempdir().unwrap().keep();
    let database_url = format!("sqlite://{}/test.sqlite", dir.display());
    larust_orm::connect(&database_url).await.unwrap();
    let pool = larust_orm::pool().unwrap().clone();
    let router = larust_http::Router::new()
        .plugin(DbPlugin)
        .with_sessions(&pool, false)
        .await
        .unwrap()
        .into_axum_router();

    let db_dir = tempfile::tempdir().unwrap();
    larust_db::connect(db_dir.path().join("test.redb"))
        .await
        .unwrap();

    let response = router
        .clone()
        .oneshot(Request::get("/xr-db").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

    // The login form itself refuses too - nothing under `/xr-db/*`
    // is reachable without a configured password, not just the gated
    // group.
    let response = router
        .oneshot(Request::get("/xr-db/login").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}
