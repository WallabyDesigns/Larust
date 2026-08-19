//! Regression test for M32's `Mail::fake()`/`assertSent()` — the first
//! test anywhere in this codebase that actually asserts on a sent email's
//! *content*, not just that the log driver printed something.
//!
//! Deliberately a single `#[tokio::test]` fn — matching
//! `larust-testing/tests/db_test.rs`'s/`crates/larust-mail/src/fake.rs`'s
//! own established reasoning: the fake recorder is one process-wide list,
//! never reset, so a second independent scenario in the same binary could
//! see the first scenario's recorded mail.

use demo::controllers::{AuthController, PostController};
use demo::mail::WelcomeMail;
use larust_http::Route;
use larust_support::axum::http::StatusCode;
use larust_testing::TestClient;

async fn build_router(pool: &sqlx::SqlitePool) -> larust_support::axum::Router {
    // `posts.index` is never actually visited by this test — only here
    // because `AuthController::register`'s success path redirects to it
    // by name (same gotcha `posts_policy_test.rs`'s own `build_router`
    // comment documents).
    Route::get("/posts", PostController::index)
        .name("posts.index")
        .get("/register", AuthController::show_register)
        .name("register")
        .post("/register", AuthController::register)
        .name("register.store")
        .middleware(larust_http::axum::middleware::from_fn(
            larust_http::csrf::verify,
        ))
        .with_sessions(pool, false)
        .await
        .unwrap()
        .into_axum_router()
}

#[tokio::test]
async fn registering_actually_sends_a_welcome_mail_to_the_new_user() {
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
            "/register",
            &[
                ("_csrf_token", &csrf_token),
                ("name", "Alice"),
                ("email", "alice@example.com"),
                ("password", "password123"),
                ("password_confirmation", "password123"),
            ],
        )
        .await
        .assert_status(StatusCode::SEE_OTHER);

    // The real proof: a `WelcomeMail`, addressed to the right person, with
    // a subject containing their name — not just that *something* was
    // logged.
    larust_testing::assert_sent::<WelcomeMail<'_>>(|sent| {
        sent.to == vec!["alice@example.com".to_string()] && sent.subject.contains("Alice")
    });

    // Nobody else's welcome mail went out.
    larust_testing::assert_not_sent::<WelcomeMail<'_>>(|sent| {
        sent.to != vec!["alice@example.com".to_string()]
    });
}
