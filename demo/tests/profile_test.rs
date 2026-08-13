//! End-to-end proof of the profile page: viewing/updating name+email, and
//! changing the password (including rejecting a wrong current password).
//! Registers a real user via `/register` (the same login-by-registering
//! shortcut `posts_policy_test.rs` uses) rather than hand-inserting a row,
//! so the password hash is a real one `verify_password` can check against.

use demo::controllers::{AuthController, PostController, ProfileController};
use demo::wire_components::PostForm;
use larust_http::Route;
use larust_support::axum::http::StatusCode;
use larust_testing::TestClient;
use std::sync::Once;

static REGISTER_ONCE: Once = Once::new();

fn ensure_registered() {
    REGISTER_ONCE.call_once(|| {
        larust_support::wire::components()
            .register::<PostForm>()
            .publish();
    });
}

async fn build_router(pool: &sqlx::SqlitePool) -> larust_support::axum::Router {
    ensure_registered();
    // `posts.index` is never visited directly — only registered because
    // `AuthController::register`'s success path redirects to it by name
    // (same gotcha `posts_policy_test.rs`'s own `build_router` documents).
    Route::get("/posts", PostController::index)
        .name("posts.index")
        .get("/posts/create", PostController::create)
        .get("/register", AuthController::show_register)
        .name("register")
        .post("/register", AuthController::register)
        .name("register.store")
        .get("/profile", ProfileController::show)
        .name("profile")
        .post("/profile", ProfileController::update)
        .name("profile.update")
        .post("/profile/password", ProfileController::update_password)
        .name("profile.password")
        .middleware(larust_http::axum::middleware::from_fn(
            larust_http::csrf::verify,
        ))
        .with_sessions(pool, false)
        .await
        .unwrap()
        .into_axum_router()
}

async fn register(
    router: &larust_support::axum::Router,
    pool: &sqlx::SqlitePool,
    name: &str,
    email: &str,
) -> TestClient {
    let mut client = TestClient::new(router.clone(), pool);
    let csrf_token = client
        .get("/posts/create")
        .await
        .csrf_token()
        .expect("create page should render a CSRF token");

    client
        .post_form(
            "/register",
            &[
                ("_csrf_token", &csrf_token),
                ("name", name),
                ("email", email),
                ("password", "password123"),
                ("password_confirmation", "password123"),
            ],
        )
        .await
        .assert_status(StatusCode::SEE_OTHER);

    client
}

async fn csrf_token_for(client: &mut TestClient, path: &str) -> String {
    client
        .get(path)
        .await
        .csrf_token()
        .expect("page should render a CSRF token")
}

#[tokio::test]
async fn viewing_and_updating_the_profile_persists_the_new_name_and_email() {
    larust_core::Application::new().unwrap();

    let pool = larust_testing::test_db(std::path::Path::new("database/migrations"))
        .await
        .unwrap();
    let router = build_router(&pool).await;
    let mut client = register(&router, &pool, "Dana", "dana@example.com").await;

    let show = client.get("/profile").await;
    show.assert_status(StatusCode::OK);
    assert!(show.body().contains("value=\"Dana\""));
    assert!(show.body().contains("value=\"dana@example.com\""));

    let csrf = csrf_token_for(&mut client, "/profile").await;
    client
        .post_form(
            "/profile",
            &[
                ("_csrf_token", &csrf),
                ("name", "Dana Updated"),
                ("email", "dana-updated@example.com"),
            ],
        )
        .await
        .assert_status(StatusCode::SEE_OTHER);

    let (name, email): (String, String) =
        sqlx::query_as("SELECT name, email FROM users WHERE email = ?")
            .bind("dana-updated@example.com")
            .fetch_one(&pool)
            .await
            .expect("the update should have applied");
    assert_eq!(name, "Dana Updated");
    assert_eq!(email, "dana-updated@example.com");

    let show_again = client.get("/profile").await;
    assert!(show_again.body().contains("value=\"Dana Updated\""));
}

#[tokio::test]
async fn changing_password_requires_the_correct_current_password() {
    larust_core::Application::new().unwrap();

    let pool = larust_testing::test_db(std::path::Path::new("database/migrations"))
        .await
        .unwrap();
    let router = build_router(&pool).await;
    let mut client = register(&router, &pool, "Eve", "eve@example.com").await;

    let (original_hash,): (String,) =
        sqlx::query_as("SELECT password_hash FROM users WHERE email = ?")
            .bind("eve@example.com")
            .fetch_one(&pool)
            .await
            .unwrap();

    // Wrong current password: rejected, hash unchanged.
    let csrf = csrf_token_for(&mut client, "/profile").await;
    client
        .post_form(
            "/profile/password",
            &[
                ("_csrf_token", &csrf),
                ("current_password", "not-the-real-password"),
                ("password", "a-brand-new-password"),
                ("password_confirmation", "a-brand-new-password"),
            ],
        )
        .await
        .assert_status(StatusCode::SEE_OTHER);

    let (hash_after_wrong_attempt,): (String,) =
        sqlx::query_as("SELECT password_hash FROM users WHERE email = ?")
            .bind("eve@example.com")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        original_hash, hash_after_wrong_attempt,
        "a wrong current password must not change anything"
    );

    // Correct current password: applied.
    let csrf = csrf_token_for(&mut client, "/profile").await;
    client
        .post_form(
            "/profile/password",
            &[
                ("_csrf_token", &csrf),
                ("current_password", "password123"),
                ("password", "a-brand-new-password"),
                ("password_confirmation", "a-brand-new-password"),
            ],
        )
        .await
        .assert_status(StatusCode::SEE_OTHER);

    let (hash_after_real_change,): (String,) =
        sqlx::query_as("SELECT password_hash FROM users WHERE email = ?")
            .bind("eve@example.com")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_ne!(
        original_hash, hash_after_real_change,
        "the correct current password should let the change through"
    );
}
