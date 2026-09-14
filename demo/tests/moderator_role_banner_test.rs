//! End-to-end proof that `posts.edit`'s `@role(Role::Moderator)` banner
//! (`app/Http/Controllers/post_controller.rs::edit` /
//! `resources/views/posts/edit.blade.xr`) is driven by the real
//! `larust-permissions` role assignment, not a stand-in bool - the first
//! live usage of `@can`/`@role` in this reference app, see
//! `crates/larust-macros/tests/view_can_role.rs` for the directive's own
//! isolated proof.

use demo::controllers::{AuthController, PostController};
use demo::permissions::{Permission, Role};
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
        .get("/posts/{post}/edit", PostController::edit)
        .name("posts.edit")
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

async fn csrf_token_for(client: &mut TestClient) -> String {
    client
        .get("/posts/create")
        .await
        .csrf_token()
        .expect("create page should render a CSRF token")
}

#[tokio::test]
async fn the_moderator_banner_only_renders_for_a_real_moderator() {
    larust_core::Application::new(demo::config::app::config).unwrap();

    let pool = larust_testing::test_db(std::path::Path::new("database/migrations"))
        .await
        .unwrap();
    let router = build_router(&pool).await;

    larust_support::permission::create_permission(Permission::ManagePosts)
        .await
        .unwrap();
    larust_support::permission::create_role(Role::Moderator)
        .await
        .unwrap();
    larust_support::permission::grant_role_permission(Role::Moderator, Permission::ManagePosts)
        .await
        .unwrap();

    let mut alice = register(&router, &pool, "Alice", "alice@example.com").await;
    let mut carol = register(&router, &pool, "Carol", "carol@example.com").await;

    let (carol_id,): (i64,) = sqlx::query_as("SELECT id FROM users WHERE email = ?")
        .bind("carol@example.com")
        .fetch_one(&pool)
        .await
        .unwrap();

    // Alice creates a post.
    let csrf_token = csrf_token_for(&mut alice).await;
    alice
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

    // Alice, the owner but not a moderator, sees no banner.
    let response = alice.get(&format!("/posts/{post_id}/edit")).await;
    response.assert_status(StatusCode::OK);
    assert!(
        !response.body().contains("Editing as a moderator"),
        "a plain owner must not see the moderator banner"
    );

    // Grant Carol the Moderator role - `Post::can_manage` now lets her
    // through `edit`'s own `authorize()` gate despite not owning the post,
    // and `@role(Role::Moderator)` picks up the same real assignment.
    let carol_user = demo::models::User::find(carol_id)
        .await
        .unwrap()
        .expect("carol must exist");
    larust_support::permission::assign_role(&carol_user, Role::Moderator)
        .await
        .unwrap();

    carol
        .get(&format!("/posts/{post_id}/edit"))
        .await
        .assert_status(StatusCode::OK)
        .assert_body_contains("Editing as a moderator");
}
