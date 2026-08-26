//! Demonstrates `TestClient::acting_as` directly — the complementary
//! usage pattern to `demo`'s full register-flow test: simulate an
//! already-authenticated user without driving a real registration/login
//! form through the router at all.

use blog::controllers::PostController;
use blog::models::{NewUser, User};
use larust_http::{Route, Router};
use larust_support::auth::require_auth;
use larust_testing::TestClient;

async fn build_router(pool: &sqlx::AnyPool) -> larust_support::axum::Router {
    Route::get("/posts", PostController::index)
        .name("posts.index")
        .get("/posts/create", PostController::create)
        .get("/login", || async { "login page" })
        .name("login")
        .group("", |r: Router| {
            r.middleware(larust_support::axum::middleware::from_fn(require_auth))
                .post("/posts", PostController::store)
                .name("posts.store")
        })
        .middleware(larust_support::axum::middleware::from_fn(
            larust_http::csrf::verify,
        ))
        .with_sessions(pool, false)
        .await
        .unwrap()
        .into_axum_router()
}

/// `test_db()` shares one physical database across every `#[tokio::test]`
/// fn in this file — assertions must stay scoped to the specific title a
/// test created (never a broad `SELECT COUNT(*) FROM posts`), so this
/// test stays correct if a sibling test is ever added alongside it.
async fn post_exists(pool: &sqlx::AnyPool, title: &str) -> bool {
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM posts WHERE title = ?")
        .bind(title)
        .fetch_one(pool)
        .await
        .unwrap();
    count > 0
}

#[tokio::test]
async fn only_a_logged_in_user_can_store_a_post() {
    let pool = larust_testing::test_db(std::path::Path::new("database/migrations"))
        .await
        .unwrap();
    let router = build_router(&pool).await;

    // A logged-out client is bounced to `/login` by `require_auth` before
    // it ever reaches `PostController::store`.
    let mut anonymous = TestClient::new(router.clone(), &pool);
    let csrf_token = anonymous
        .get("/posts/create")
        .await
        .csrf_token()
        .expect("create page should render a CSRF token");
    anonymous
        .post_form(
            "/posts",
            &[("_csrf_token", &csrf_token), ("title", "Should not exist")],
        )
        .await
        .assert_redirect_to("/login");

    assert!(
        !post_exists(&pool, "Should not exist").await,
        "the anonymous attempt must not have created a post"
    );

    // `acting_as` authenticates the client against the same session
    // store the router uses, with no `/login` request involved at all.
    let password_hash = larust_support::auth::hash_password("password123").unwrap();
    let user = User::create(NewUser {
        name: "Casey".to_string(),
        email: "casey@example.com".to_string(),
        password_hash,
    })
    .await
    .unwrap();

    let mut client = TestClient::new(router, &pool);
    client.acting_as(&user).await.unwrap();

    let csrf_token = client
        .get("/posts/create")
        .await
        .csrf_token()
        .expect("create page should render a CSRF token");
    client
        .post_form(
            "/posts",
            &[("_csrf_token", &csrf_token), ("title", "A real post")],
        )
        .await
        .assert_redirect_to("/posts");

    assert!(
        post_exists(&pool, "A real post").await,
        "acting_as should have let the post through"
    );
}
