//! End-to-end proof of `@wire('post-form')`/`wire:submit="post"` - the
//! reactive replacement for the plain `<form method="POST" action="/posts">`
//! on `posts/create.blade.xr`. Drives the same `POST /__larust_wire/{id}`
//! JSON endpoint the vendored client runtime uses, mirroring
//! `wire_post_list_test.rs`'s pattern.
//!
//! Uses `test_transaction` (a fresh, isolated database per call), not
//! `test_db` (one shared database across every `#[tokio::test]` fn in this
//! binary) - this file has two tests, and `cargo test` runs them
//! concurrently by default, so a shared database would let one test's
//! `INSERT INTO posts` be visible to the other's `SELECT COUNT(*) FROM
//! posts` assertion, exactly the flaky cross-test interference `test_db`'s
//! own docs warn about.

use demo::controllers::{AuthController, PostController};
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
    // `posts.index` is never visited - only here because
    // `AuthController::register`'s success path redirects to it by name
    // (same gotcha `posts_policy_test.rs`'s own `build_router` documents).
    Route::get("/posts", PostController::index)
        .name("posts.index")
        .get("/posts/create", PostController::create)
        .get("/posts/{post}/edit", PostController::edit)
        .name("posts.edit")
        .get("/register", AuthController::show_register)
        .name("register")
        .post("/register", AuthController::register)
        .name("register.store")
        .get(
            "/__larust_wire/runtime.js",
            larust_support::wire::runtime_js,
        )
        .post(
            "/__larust_wire/{component_id}",
            larust_support::wire::update,
        )
        .middleware(larust_http::axum::middleware::from_fn(
            larust_http::csrf::verify,
        ))
        .with_sessions(pool, false)
        .await
        .unwrap()
        .into_axum_router()
}

fn extract_wire_id(html: &str) -> String {
    let needle = "data-wire-id=\"";
    let start = html.find(needle).expect("missing data-wire-id") + needle.len();
    let end = html[start..].find('"').unwrap() + start;
    html[start..end].to_string()
}

async fn login(client: &mut TestClient, name: &str, email: &str) {
    let csrf = client
        .get("/posts/create")
        .await
        .meta_csrf_token()
        .expect("create page should render a csrf-token meta tag");
    client
        .post_form(
            "/register",
            &[
                ("_csrf_token", &csrf),
                ("name", name),
                ("email", email),
                ("password", "password123"),
                ("password_confirmation", "password123"),
            ],
        )
        .await
        .assert_status(StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn wire_submit_creates_a_real_post_and_redirects_to_it() {
    larust_core::Application::new(demo::config::app::config).unwrap();
    ensure_registered();

    let migrations_dir = std::path::Path::new("database/migrations");
    larust_testing::test_transaction(migrations_dir, |pool| async move {
        let router = build_router(&pool).await;

        let mut client = TestClient::new(router.clone(), &pool);
        login(&mut client, "Alice", "alice-post-form@example.com").await;

        let page = client.get("/posts/create").await;
        // Pulled in by the shared layout's `@larustscripts`, not a manual
        // `<script>` tag on this page's own template.
        assert!(page.body().contains("/__larust_wire/runtime.js"));
        let wire_id = extract_wire_id(page.body());
        let csrf_token = page
            .meta_csrf_token()
            .expect("create page should render a csrf-token meta tag");

        let response = client
            .post_json(
                &format!("/__larust_wire/{wire_id}"),
                &csrf_token,
                &larust_support::serde_json::json!({
                    "props": {
                        "title": "My Rust Journey",
                        "tags": "rust, journal",
                        "content": "<p>hello world</p>"
                    },
                    "action": { "name": "post", "args": null }
                }),
            )
            .await;

        response.assert_status(StatusCode::OK);
        let redirect = response
            .header("x-wire-redirect")
            .expect("a successful publish should redirect to the new post");

        let (post_id, title, content): (i64, String, String) =
            sqlx::query_as("SELECT id, title, content FROM posts WHERE title = ?")
                .bind("My Rust Journey")
                .fetch_one(&pool)
                .await
                .expect("the post should have actually been created");
        assert_eq!(redirect, format!("/posts/{post_id}"));
        assert_eq!(title, "My Rust Journey");
        assert!(content.contains("hello world"));

        let (tag_count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM post_tag JOIN tags ON tags.id = post_tag.tag_id \
             WHERE post_tag.post_id = ?",
        )
        .bind(post_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            tag_count, 2,
            "both \"rust\" and \"journal\" should be attached"
        );
    })
    .await;
}

#[tokio::test]
async fn wire_submit_with_a_blank_title_shows_a_validation_error_and_creates_nothing() {
    larust_core::Application::new(demo::config::app::config).unwrap();
    ensure_registered();

    let migrations_dir = std::path::Path::new("database/migrations");
    larust_testing::test_transaction(migrations_dir, |pool| async move {
        let router = build_router(&pool).await;

        let mut client = TestClient::new(router.clone(), &pool);
        login(&mut client, "Bob", "bob-post-form@example.com").await;

        let page = client.get("/posts/create").await;
        let wire_id = extract_wire_id(page.body());
        let csrf_token = page
            .meta_csrf_token()
            .expect("create page should render a csrf-token meta tag");

        let response = client
            .post_json(
                &format!("/__larust_wire/{wire_id}"),
                &csrf_token,
                &larust_support::serde_json::json!({
                    "props": { "title": "", "tags": "", "content": "<p>only content</p>" },
                    "action": { "name": "post", "args": null }
                }),
            )
            .await;

        response.assert_status(StatusCode::OK);
        assert!(
            response.header("x-wire-redirect").is_none(),
            "a validation failure must not redirect"
        );
        assert!(response.body().contains("Title is required."));

        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM posts")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0, "no post should have been created");
    })
    .await;
}

#[tokio::test]
async fn edit_mode_prefills_the_form_and_wire_submit_updates_the_existing_post_in_place() {
    larust_core::Application::new(demo::config::app::config).unwrap();
    ensure_registered();

    let migrations_dir = std::path::Path::new("database/migrations");
    larust_testing::test_transaction(migrations_dir, |pool| async move {
        let router = build_router(&pool).await;

        let mut client = TestClient::new(router.clone(), &pool);
        login(&mut client, "Carol", "carol-post-form@example.com").await;

        // Create the post through the same wire "post" action the create
        // page already uses, then edit it.
        let create_page = client.get("/posts/create").await;
        let create_wire_id = extract_wire_id(create_page.body());
        let create_csrf = create_page
            .meta_csrf_token()
            .expect("create page should render a csrf-token meta tag");
        client
            .post_json(
                &format!("/__larust_wire/{create_wire_id}"),
                &create_csrf,
                &larust_support::serde_json::json!({
                    "props": {
                        "title": "Draft Title",
                        "tags": "draft",
                        "content": "<p>draft content</p>"
                    },
                    "action": { "name": "post", "args": null }
                }),
            )
            .await
            .assert_status(StatusCode::OK);
        let (post_id,): (i64,) = sqlx::query_as("SELECT id FROM posts WHERE title = ?")
            .bind("Draft Title")
            .fetch_one(&pool)
            .await
            .unwrap();

        let edit_page = client.get(&format!("/posts/{post_id}/edit")).await;
        edit_page.assert_status(StatusCode::OK);
        // `mount`'s edit-mode branch prefilled these from the existing row -
        // not the empty create-mode defaults.
        assert!(edit_page.body().contains("value=\"Draft Title\""));
        assert!(edit_page.body().contains("value=\"draft\""));
        assert!(edit_page.body().contains("Save Changes"));

        let edit_wire_id = extract_wire_id(edit_page.body());
        let edit_csrf = edit_page
            .meta_csrf_token()
            .expect("edit page should render a csrf-token meta tag");

        let response = client
            .post_json(
                &format!("/__larust_wire/{edit_wire_id}"),
                &edit_csrf,
                &larust_support::serde_json::json!({
                    "props": {
                        "title": "Published Title",
                        "tags": "published",
                        "content": "<p>final content</p>"
                    },
                    "action": { "name": "post", "args": null }
                }),
            )
            .await;

        response.assert_status(StatusCode::OK);
        let redirect = response
            .header("x-wire-redirect")
            .expect("a successful update should redirect back to the post");
        assert_eq!(redirect, format!("/posts/{post_id}"));

        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM posts")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            count, 1,
            "editing must update in place, not create a second post"
        );

        let (title, content): (String, String) =
            sqlx::query_as("SELECT title, content FROM posts WHERE id = ?")
                .bind(post_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(title, "Published Title");
        assert!(content.contains("final content"));

        let (tag_name,): (String,) = sqlx::query_as(
            "SELECT tags.name FROM post_tag JOIN tags ON tags.id = post_tag.tag_id \
             WHERE post_tag.post_id = ?",
        )
        .bind(post_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(tag_name, "published", "tags should also have been synced");
    })
    .await;
}
