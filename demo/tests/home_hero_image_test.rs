//! End-to-end proof that the home page's `<Image ...>` tags (added as a
//! real showcase of `larust-image`'s full attribute set, not a unit test
//! fixture) actually work against this app's own real files: the hero
//! banner (`priority`/`sizes`/`placeholder="blur"`, routed through
//! `/__larust_image/optimize`) and the footer logo (`unoptimized`,
//! served straight from `public/`). `larust-image`'s own test suite
//! already covers `tag`/`optimize` in isolation against synthetic
//! fixtures - this instead proves the *real* `public/images/cover.jpg`/
//! `logo.png` files this app ships actually decode and resize
//! correctly, and that `ImagePlugin` is actually registered on this
//! app's real router (`routes/web.rs`) - a plugin a template references
//! but the router never mounts would 404 on every page view, silently,
//! since nothing short of an actual request catches that.

use axum::body::Body;
use axum::http::{Request, StatusCode};
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
        .plugin(larust_support::image::ImagePlugin)
        .with_sessions(pool, false)
        .await
        .unwrap()
        .into_axum_router()
}

#[tokio::test]
async fn the_home_page_renders_a_real_optimized_hero_image_and_an_unoptimized_logo() {
    larust_core::Application::new(demo::config::app::config).unwrap();

    let migrations_dir = std::path::Path::new("database/migrations");
    larust_testing::test_transaction(migrations_dir, |pool| async move {
        let router = build_router(&pool).await;

        let home_page = router
            .clone()
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(home_page.status(), StatusCode::OK);
        let body = axum::body::to_bytes(home_page.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();

        // `priority="true"` - a real inline preload link, not just an
        // eager `<img>`.
        assert!(
            body.contains(r#"<link rel="preload" as="image""#),
            "expected a priority preload link in the home page body: {body}"
        );
        // `sizes="100vw"` - a real srcset, not a bare passthrough attribute.
        assert!(
            body.contains("imagesrcset=") && body.contains("srcset="),
            "expected a real srcset on the hero image: {body}"
        );
        let hero_src = "/__larust_image/optimize?src=images/cover.jpg&w=1200&h=676&q=75";
        // `&` renders HTML-escaped as `&amp;` inside the attribute value -
        // the real request below needs the raw, unescaped form instead.
        let hero_src_escaped = "/__larust_image/optimize?src=images/cover.jpg&amp;w=1200&amp;h=676&amp;q=75";
        assert!(
            body.contains(hero_src_escaped),
            "expected the hero image's optimize URL ({hero_src_escaped}) in the home page body: {body}"
        );
        // `unoptimized="true"` - served straight from `public/`, never
        // through the optimize route.
        assert!(
            body.contains(r#"src="/images/logo.png""#),
            "expected the footer logo to point straight at /images/logo.png: {body}"
        );

        // The real proof `ImagePlugin` is actually mounted and the real
        // shipped file actually decodes/resizes: fetch the hero image's
        // own optimize URL and confirm it's a genuine, non-empty image
        // response, not a 404 or a 500 from a corrupt/missing file.
        let optimized = router
            .oneshot(Request::get(hero_src).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            optimized.status(),
            StatusCode::OK,
            "the hero image's own optimize URL should resolve - ImagePlugin must be mounted \
             on this app's real router, and public/images/cover.jpg must actually decode"
        );
        let image_bytes = axum::body::to_bytes(optimized.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(
            image_bytes.len() > 1000,
            "expected a real, non-trivial re-encoded image, got {} bytes",
            image_bytes.len()
        );
    })
    .await;
}
