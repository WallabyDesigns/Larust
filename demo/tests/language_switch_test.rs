//! End-to-end proof that `larust_http::locale::negotiate` + `larust_lang::t`
//! actually change what a real page renders - not just that the crate-level
//! unit tests in `larust-lang`/`larust-http` pass in isolation. Exercises
//! the exact pipeline a real app wires: `POST /language` (`LanguageController
//! ::update`) stores a locale in the session; `locale::negotiate` (attached
//! in `routes/web.rs`, same as CSRF) reads it back on the *next* request and
//! sets the current-request locale before `PostController::index` ever
//! renders `posts/index.blade.xr`'s `{{ t("posts.heading") }}`.

use demo::controllers::{LanguageController, PostController};
use demo::wire_components::PostList;
use larust_http::Route;
use larust_support::axum::http::StatusCode;
use larust_testing::TestClient;
use std::sync::Once;

static REGISTER_ONCE: Once = Once::new();

fn ensure_registered() {
    REGISTER_ONCE.call_once(|| {
        larust_support::wire::components()
            .register::<PostList>()
            .publish();
    });
}

async fn build_router(pool: &sqlx::AnyPool) -> larust_support::axum::Router {
    ensure_registered();
    Route::get("/posts", PostController::index)
        .name("posts.index")
        .post("/language", LanguageController::update)
        .name("language.update")
        .middleware(larust_http::axum::middleware::from_fn(
            larust_http::csrf::verify,
        ))
        .middleware(larust_http::axum::middleware::from_fn(
            larust_http::locale::negotiate,
        ))
        .with_sessions(pool, false)
        .await
        .unwrap()
        .into_axum_router()
}

#[tokio::test]
async fn switching_language_changes_what_the_very_next_page_renders() {
    larust_core::Application::new(demo::config::app::config).unwrap();

    let pool = larust_testing::test_db(std::path::Path::new("database/migrations"))
        .await
        .unwrap();
    let router = build_router(&pool).await;
    let mut client = TestClient::new(router.clone(), &pool);

    // Default locale (APP_LOCALE, "en") - no /language POST has happened
    // yet in this session at all.
    client
        .get("/posts")
        .await
        .assert_status(StatusCode::OK)
        .assert_body_contains("Small notes. Fast ideas.");

    let csrf_token = client
        .get("/posts")
        .await
        .csrf_token()
        .expect("posts index should render a CSRF token");
    client
        .post_form(
            "/language",
            &[("_csrf_token", &csrf_token), ("locale", "es")],
        )
        .await
        .assert_status(StatusCode::SEE_OTHER);

    // Same client (same session cookie), a brand new request - the
    // *session*, not anything request-local, is what carried the change.
    client
        .get("/posts")
        .await
        .assert_status(StatusCode::OK)
        .assert_body_contains("Notas breves. Ideas rápidas.");
}

#[tokio::test]
async fn an_unsupported_locale_is_rejected_and_never_reaches_the_session() {
    larust_core::Application::new(demo::config::app::config).unwrap();

    let pool = larust_testing::test_db(std::path::Path::new("database/migrations"))
        .await
        .unwrap();
    let router = build_router(&pool).await;
    let mut client = TestClient::new(router.clone(), &pool);

    let csrf_token = client
        .get("/posts")
        .await
        .csrf_token()
        .expect("posts index should render a CSRF token");
    client
        .post_form(
            "/language",
            &[("_csrf_token", &csrf_token), ("locale", "xx")],
        )
        .await
        .assert_status(StatusCode::SEE_OTHER);

    // Still English - the rejected locale was never stored.
    client
        .get("/posts")
        .await
        .assert_status(StatusCode::OK)
        .assert_body_contains("Small notes. Fast ideas.");
}
