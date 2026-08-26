//! Converts the manual curl verification the `Policy<User> for Post`
//! authorization framework was checked with (two real users, one editing
//! the other's post) into a permanent regression test: a non-owner is
//! forbidden from editing/updating/deleting a post; the real owner
//! succeeds at all three.

use demo::controllers::{AuthController, PostController};
use demo::wire_components::PostForm;
use larust_http::Route;
use larust_support::axum::http::StatusCode;
use larust_testing::TestClient;
use std::sync::Once;

// `/posts/create` (visited below only to fetch a CSRF token) renders
// `posts.create`, which mounts `@wire('post-form')` — every component a
// visited route's template mounts must be registered in this file's own
// process-wide registry, or `mount()` 500s with "no component registered".
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
    // Includes every named route the handlers under test redirect
    // through (`posts.index`, `register`) — `larust_support::redirect()
    // .route(name)` resolves against this router's own name registry, so
    // a handler whose success path redirects to a name this test router
    // never declared fails with a 500, not the response the handler
    // itself is trying to produce.
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
        .post("/posts/{post}/update", PostController::update)
        .name("posts.update")
        .post("/posts/{post}/delete", PostController::destroy)
        .name("posts.destroy")
        .middleware(larust_http::axum::middleware::from_fn(
            larust_http::csrf::verify,
        ))
        .with_sessions(pool, false)
        .await
        .unwrap()
        .into_axum_router()
}

/// Registers a new user (which also logs them in — Larust's `register()`
/// mirrors Laravel's own behavior here) and returns the client, ready to
/// act as that user for the rest of the test.
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
async fn only_the_owner_may_edit_update_or_delete_their_post() {
    // `Application::new()` populates `larust_core::config()` — required
    // by `AuthController::register`'s welcome-mail send, which reads
    // `Config::mail_driver`. Deliberately not `.serve()`/`.router()`;
    // this test drives its own minimal router directly. Safe to call
    // here even though nothing else in this codebase's test suite does —
    // `Application::new()` is explicitly idempotent (`try_init` for
    // logging, an idempotent config publish), by design, for this exact
    // situation.
    larust_core::Application::new(demo::config::app::config).unwrap();

    let pool = larust_testing::test_db(std::path::Path::new("database/migrations"))
        .await
        .unwrap();
    let router = build_router(&pool).await;

    let mut alice = register(&router, &pool, "Alice", "alice@example.com").await;
    let mut bob = register(&router, &pool, "Bob", "bob@example.com").await;

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

    // Bob (not the owner) is forbidden from every write action.
    bob.get(&format!("/posts/{post_id}/edit"))
        .await
        .assert_status(StatusCode::FORBIDDEN);

    let bob_csrf = csrf_token_for(&mut bob).await;
    bob.post_form(
        &format!("/posts/{post_id}/update"),
        &[
            ("_csrf_token", &bob_csrf),
            ("title", "Hijacked"),
            ("content", "<p>hijacked</p>"),
            ("tags", ""),
        ],
    )
    .await
    .assert_status(StatusCode::FORBIDDEN);
    bob.post_form(
        &format!("/posts/{post_id}/delete"),
        &[("_csrf_token", &bob_csrf)],
    )
    .await
    .assert_status(StatusCode::FORBIDDEN);

    let (title,): (String,) = sqlx::query_as("SELECT title FROM posts WHERE id = ?")
        .bind(post_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        title, "Alice's Post",
        "Bob's forbidden update must not apply"
    );

    // Alice (the real owner) can edit, update, and delete it.
    alice
        .get(&format!("/posts/{post_id}/edit"))
        .await
        .assert_status(StatusCode::OK);

    let alice_csrf = csrf_token_for(&mut alice).await;
    alice
        .post_form(
            &format!("/posts/{post_id}/update"),
            &[
                ("_csrf_token", &alice_csrf),
                ("title", "Alice's Updated Post"),
                ("content", "<p>Updated by Alice</p>"),
                ("tags", ""),
            ],
        )
        .await
        .assert_status(StatusCode::SEE_OTHER);

    let (title,): (String,) = sqlx::query_as("SELECT title FROM posts WHERE id = ?")
        .bind(post_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(title, "Alice's Updated Post");

    let alice_csrf = csrf_token_for(&mut alice).await;
    alice
        .post_form(
            &format!("/posts/{post_id}/delete"),
            &[("_csrf_token", &alice_csrf)],
        )
        .await
        .assert_status(StatusCode::SEE_OTHER);

    let remaining: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM posts WHERE id = ?")
        .bind(post_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        remaining.0, 0,
        "Alice's own delete should have removed the post"
    );
}
