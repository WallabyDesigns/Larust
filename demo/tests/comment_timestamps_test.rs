//! End-to-end proof that `Comment`'s `#[timestamps]` (`app/Models/
//! comment.rs`) is wired into the real, live `CommentController::store`
//! path - not just the isolated `#[derive(Model)]` proof in
//! `crates/larust-macros/tests/model_timestamps.rs`.

use demo::controllers::{AuthController, CommentController, PostController};
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

async fn build_router(pool: &sqlx::AnyPool) -> larust_support::axum::Router {
    ensure_registered();
    Route::get("/posts", PostController::index)
        .name("posts.index")
        .get("/posts/create", PostController::create)
        .get("/register", AuthController::show_register)
        .name("register")
        .post("/register", AuthController::register)
        .name("register.store")
        .post("/posts", PostController::store)
        .name("posts.store")
        .post("/posts/{post}/comments", CommentController::store)
        .name("posts.comments.store")
        .middleware(larust_http::axum::middleware::from_fn(
            larust_http::csrf::verify,
        ))
        .with_sessions(pool, false)
        .await
        .unwrap()
        .into_axum_router()
}

#[tokio::test]
async fn a_comment_posted_through_the_real_controller_gets_real_timestamps() {
    larust_core::Application::new(demo::config::app::config).unwrap();

    let pool = larust_testing::test_db(std::path::Path::new("database/migrations"))
        .await
        .unwrap();
    let router = build_router(&pool).await;

    let mut client = TestClient::new(router.clone(), &pool);
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
                ("name", "Alice"),
                ("email", "alice@example.com"),
                ("password", "password123"),
                ("password_confirmation", "password123"),
            ],
        )
        .await
        .assert_status(StatusCode::SEE_OTHER);

    let csrf_token = client
        .get("/posts/create")
        .await
        .csrf_token()
        .expect("create page should render a CSRF token");
    client
        .post_form(
            "/posts",
            &[
                ("_csrf_token", &csrf_token),
                ("title", "Alice's Post"),
                ("content", "<p>Hello from Alice</p>"),
                ("tags", ""),
            ],
        )
        .await
        .assert_status(StatusCode::SEE_OTHER);
    let (post_id,): (i64,) = sqlx::query_as("SELECT id FROM posts WHERE title = ?")
        .bind("Alice's Post")
        .fetch_one(&pool)
        .await
        .unwrap();

    let before = now_unix_secs();
    let csrf_token = client
        .get("/posts/create")
        .await
        .csrf_token()
        .expect("create page should render a CSRF token");
    client
        .post_form(
            &format!("/posts/{post_id}/comments"),
            &[("_csrf_token", &csrf_token), ("body", "Nice post!")],
        )
        .await
        .assert_status(StatusCode::SEE_OTHER);
    let after = now_unix_secs();

    let (created_at, updated_at): (i64, i64) =
        sqlx::query_as("SELECT created_at, updated_at FROM comments WHERE body = ?")
            .bind("Nice post!")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        created_at >= before && created_at <= after,
        "created_at should be stamped to roughly now, got {created_at}"
    );
    assert_eq!(
        created_at, updated_at,
        "a freshly created comment's created_at and updated_at should match"
    );
}

fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}
