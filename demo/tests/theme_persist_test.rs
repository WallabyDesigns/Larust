//! End-to-end proof of `@globals`' `persist` mechanism (see
//! `docs/MACROS.md`'s `persist` section): a request carrying the
//! `larust_pref_theme` cookie renders the server-computed `data-theme`
//! attribute directly from that cookie's value, and a request with no
//! such cookie falls back to `layouts/app.blade.xr`'s own
//! `persist theme = "dark"` default — proving the whole pipeline (parser
//! → resolve → `larust-macros` codegen → `larust_http::preferences::get`)
//! works together, not just each layer in isolation.
//!
//! Driven via `tower::ServiceExt::oneshot` directly (not `TestClient`,
//! which has no way to attach an arbitrary `Cookie` header to a single
//! request) against a minimal router exposing just `/` — the same
//! `index`/`welcome` handler `routes/web.rs` itself uses.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use larust_http::session::Session;
use larust_http::Route;
use larust_support::preferences::CookieJar;
use tower::ServiceExt;

async fn index(
    session: Session,
    cookies: CookieJar,
) -> Result<impl larust_support::axum::response::IntoResponse, larust_core::AppError> {
    let csrf_token = larust_http::csrf::token(&session).await;
    let is_authenticated = larust_support::auth::check(&session).await?;
    let unread_count = demo::controllers::unread_count_for(&session).await?;
    let nav_active = "home";
    let count = demo::models::Post::all().await?.len() as i64;
    Ok(
        larust_support::view!("welcome", { cookies: &cookies, csrf_token, is_authenticated, unread_count, nav_active, count }),
    )
}

async fn build_router(pool: &sqlx::AnyPool) -> axum::Router {
    Route::get("/", index)
        .with_sessions(pool, false)
        .await
        .unwrap()
        .into_axum_router()
}

#[tokio::test]
async fn a_request_with_the_theme_cookie_renders_that_theme() {
    larust_core::Application::new(demo::config::app::config).unwrap();

    let migrations_dir = std::path::Path::new("database/migrations");
    larust_testing::test_transaction(migrations_dir, |pool| async move {
        let router = build_router(&pool).await;

        let request = Request::get("/")
            .header(header::COOKIE, "larust_pref_theme=light")
            .body(Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            body.contains(r#"data-theme="light""#),
            "expected data-theme=\"light\" in body: {body}"
        );
    })
    .await;
}

#[tokio::test]
async fn a_request_with_no_theme_cookie_falls_back_to_the_layouts_own_default() {
    larust_core::Application::new(demo::config::app::config).unwrap();

    let migrations_dir = std::path::Path::new("database/migrations");
    larust_testing::test_transaction(migrations_dir, |pool| async move {
        let router = build_router(&pool).await;

        let request = Request::get("/").body(Body::empty()).unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            body.contains(r#"data-theme="dark""#),
            "expected the layout's own persist fallback data-theme=\"dark\" in body: {body}"
        );
    })
    .await;
}
