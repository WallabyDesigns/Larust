//! Demonstrates `larust_testing::test_transaction` end to end through a
//! real router/`TestClient` — specifically the case that broke the first,
//! abandoned "real `BEGIN`/`ROLLBACK`" design (see `transaction.rs`'s own
//! doc comment): a session-backed, CSRF-protected route. Registers the
//! same email address in two independent `test_transaction` calls —
//! `users.email` is `UNIQUE`, so this only succeeds twice if each call
//! genuinely got its own isolated database; if they shared one (or if
//! sessions/CSRF broke the way they did under the abandoned design),
//! either the second registration would fail outright or the whole flow
//! would 500 before ever reaching it.

use demo::controllers::{AuthController, PostController};
use larust_http::Route;
use larust_support::axum::http::StatusCode;
use larust_testing::TestClient;

async fn build_router(pool: &sqlx::AnyPool) -> larust_support::axum::Router {
    // `posts.index` is never actually visited by this test — it's only
    // here because `AuthController::register`'s success path redirects to
    // it by name (same gotcha `posts_policy_test.rs`'s own `build_router`
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

async fn register_and_count_users(pool: sqlx::AnyPool) -> i64 {
    larust_core::Application::new(demo::config::app::config).unwrap();
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
                ("name", "Transaction Tester"),
                ("email", "transaction-tester@example.com"),
                ("password", "password123"),
                ("password_confirmation", "password123"),
            ],
        )
        .await
        .assert_status(StatusCode::SEE_OTHER);

    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
        .fetch_one(&pool)
        .await
        .unwrap();
    count
}

#[tokio::test]
async fn registering_the_same_email_succeeds_in_two_independent_test_transaction_calls() {
    let migrations_dir = std::path::Path::new("database/migrations");

    // If this call's database weren't genuinely isolated, or if the
    // session/CSRF flow this exercises were broken the way it was under
    // this feature's first, abandoned design, this would either 500 or
    // never even reach the count below.
    let first = larust_testing::test_transaction(migrations_dir, |pool| async move {
        register_and_count_users(pool).await
    })
    .await;
    assert_eq!(first, 1);

    // A second, independent call registering the exact same (`UNIQUE`)
    // email — if it shared the first call's database, this registration
    // would fail outright instead of succeeding a second time.
    let second = larust_testing::test_transaction(migrations_dir, |pool| async move {
        register_and_count_users(pool).await
    })
    .await;
    assert_eq!(second, 1);
}
