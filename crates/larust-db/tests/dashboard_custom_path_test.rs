//! Proves `DB_DASHBOARD_PATH` actually changes where `DbPlugin` mounts —
//! a separate test binary from `dashboard_test.rs`/`dashboard_disabled_test.rs`
//! (see the former's own doc comment) since `dashboard_path()`'s
//! `OnceLock` caches whatever it reads on first access for the rest of
//! this process, same reasoning as `configured_password_hash()`.

use axum::body::Body;
use axum::extract::Request;
use axum::http::StatusCode;
use larust_db::DbPlugin;
use tower::ServiceExt;

#[tokio::test]
async fn dashboard_mounts_at_the_configured_path_instead_of_the_default() {
    // Safety: must be set before the very first request touches
    // `dashboard_path()`, for the same reason `DB_DASHBOARD_PASSWORD` must
    // be set early in `dashboard_test.rs`.
    std::env::set_var("DB_DASHBOARD_PATH", "/admin/embedded-store/");
    std::env::set_var("DB_DASHBOARD_PASSWORD", "s3cret");

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

    // Leading/trailing slashes in the configured value are trimmed, so
    // the route actually lives at exactly `/admin/embedded-store`.
    let response = router
        .clone()
        .oneshot(
            Request::get("/admin/embedded-store")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(response.status().is_redirection());

    // The default path is NOT also reachable — routing happens exclusively
    // at the configured path, this isn't an additive alias.
    let response = router
        .oneshot(Request::get("/xr-db").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
