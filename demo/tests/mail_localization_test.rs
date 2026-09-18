//! The welcome mail's subject/body used to be hardcoded English literals
//! with no `t()` call at all - this fires the registration flow with the
//! session already switched to Spanish first, to prove the email itself
//! (not just the app's own pages) now reads through the current locale.
//! `WelcomeMail::html_body` renders synchronously inside `AuthController
//! ::register`'s own request, so it inherits whatever locale `locale
//! ::negotiate` already established for this same request from the
//! session - no separate per-user stored locale needed for this to work
//! correctly.
//!
//! A separate file (not another `#[tokio::test]` fn in `mail_test.rs`) for
//! the same reason that file's own module doc comment gives: the fake
//! mail recorder is one process-wide list, never reset, so a second
//! scenario in the *same* binary would see the first scenario's recorded
//! mail too - a separate integration test file is a separate process.

use demo::controllers::{AuthController, LanguageController, PostController};
use demo::mail::WelcomeMail;
use larust_http::Route;
use larust_support::axum::http::StatusCode;
use larust_testing::TestClient;

async fn build_router(pool: &sqlx::AnyPool) -> larust_support::axum::Router {
    Route::get("/posts", PostController::index)
        .name("posts.index")
        .get("/register", AuthController::show_register)
        .name("register")
        .post("/register", AuthController::register)
        .name("register.store")
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
async fn registering_while_the_session_locale_is_spanish_sends_a_spanish_welcome_mail() {
    larust_core::Application::new(demo::config::app::config).unwrap();
    larust_testing::fake();

    let pool = larust_testing::test_db(std::path::Path::new("database/migrations"))
        .await
        .unwrap();
    let router = build_router(&pool).await;
    let mut client = TestClient::new(router, &pool);

    let csrf_token = client
        .get("/register")
        .await
        .csrf_token()
        .expect("register page should render a CSRF token");
    client
        .post_form(
            "/language",
            &[("_csrf_token", &csrf_token), ("locale", "es")],
        )
        .await
        .assert_status(StatusCode::SEE_OTHER);

    let csrf_token = client
        .get("/register")
        .await
        .csrf_token()
        .expect("register page should render a CSRF token");
    client
        .post_form(
            "/register",
            &[
                ("_csrf_token", &csrf_token),
                ("name", "Beatriz"),
                ("email", "beatriz@example.com"),
                ("password", "password123"),
                ("password_confirmation", "password123"),
            ],
        )
        .await
        .assert_status(StatusCode::SEE_OTHER);

    larust_testing::assert_sent::<WelcomeMail<'_>>(|sent| {
        sent.to == vec!["beatriz@example.com".to_string()]
            && sent.subject.contains("Beatriz")
            && sent.html_body.contains("Gracias por registrarte")
    });
}
