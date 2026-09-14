//! End-to-end proof that `profile.show`'s "You have administrator
//! privileges." badge (`app/Http/Controllers/profile_controller.rs::show`
//! / `resources/views/profile/show.blade.xr`) is driven by the real
//! `larust_support::permission::is_admin` - `app/Permissions/mod.rs`'s
//! `AdminRole for Role { fn admin() -> Self { Role::Moderator } }` - not a
//! stand-in bool.

use demo::controllers::{AuthController, PostController, ProfileController};
use demo::permissions::Role;
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
        .get("/profile", ProfileController::show)
        .name("profile")
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
    pool: &sqlx::AnyPool,
    name: &str,
    email: &str,
) -> TestClient {
    let mut client = TestClient::new(router.clone(), pool);
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

#[tokio::test]
async fn the_admin_badge_only_renders_for_a_real_moderator() {
    larust_core::Application::new(demo::config::app::config).unwrap();

    let pool = larust_testing::test_db(std::path::Path::new("database/migrations"))
        .await
        .unwrap();
    let router = build_router(&pool).await;

    larust_support::permission::create_permission(demo::permissions::Permission::ManagePosts)
        .await
        .unwrap();
    larust_support::permission::create_role(Role::Moderator)
        .await
        .unwrap();

    let mut alice = register(&router, &pool, "Alice", "alice@example.com").await;
    let mut carol = register(&router, &pool, "Carol", "carol@example.com").await;

    // Alice: a plain, unprivileged user - no badge.
    alice
        .get("/profile")
        .await
        .assert_status(StatusCode::OK)
        .assert_body_contains("Your profile.");
    let response = alice.get("/profile").await;
    assert!(
        !response.body().contains("administrator privileges"),
        "a plain user must not see the admin badge"
    );

    // Carol: granted the role `AdminRole::admin()` names for this app
    // (`Role::Moderator`) - the badge now renders.
    let (carol_id,): (i64,) = sqlx::query_as("SELECT id FROM users WHERE email = ?")
        .bind("carol@example.com")
        .fetch_one(&pool)
        .await
        .unwrap();
    let carol_user = demo::models::User::find(carol_id)
        .await
        .unwrap()
        .expect("carol must exist");
    larust_support::permission::assign_role(&carol_user, Role::Moderator)
        .await
        .unwrap();

    carol
        .get("/profile")
        .await
        .assert_status(StatusCode::OK)
        .assert_body_contains("administrator privileges");
}
