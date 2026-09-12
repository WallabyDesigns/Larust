//! End-to-end proof, against the real `demo` app (real router, real
//! session/CSRF middleware, real SQLite database), that `PostForm`'s
//! `@push('head')` (added to `resources/views/components/post-form.blade.xr`
//! alongside this test) - a dynamic page `<title>` reflecting the post's
//! own `title` field - reaches `layouts/app.blade.xr`'s `@stack('head')`
//! on initial render, and reaches the client as an `x-wire-head-patch`
//! header on a later live re-render. This is the real, shipped case
//! `docs/GOTCHAS.md`'s `@push`/`@stack` entry describes:
//! `posts/create.blade.xr` composes with `layouts/app.blade.xr` via
//! `@extends` (one shared tree), but `<wire:post-form />`'s own render is a
//! genuinely separate `view!(...)` call the compile-time-only mechanism
//! can't see into.
//!
//! Mirrors `wire_post_form_test.rs`'s own router/login helpers (duplicated
//! locally, matching that file's own relationship to `wire_post_list_test.rs`).

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
    Route::get("/posts", PostController::index)
        .name("posts.index")
        .get("/posts/create", PostController::create)
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

fn decode_header(encoded: &str) -> String {
    String::from_utf8(
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded).unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn the_pages_layout_stack_gets_post_forms_initial_dynamic_title_push() {
    larust_core::Application::new(demo::config::app::config).unwrap();
    ensure_registered();

    let migrations_dir = std::path::Path::new("database/migrations");
    larust_testing::test_transaction(migrations_dir, |pool| async move {
        let router = build_router(&pool).await;
        let mut client = TestClient::new(router.clone(), &pool);
        login(&mut client, "Carol", "carol-post-form@example.com").await;

        let page = client.get("/posts/create").await;

        // A fresh create-mode mount has an empty title - the same
        // `@stack('head')` (`layouts/app.blade.xr`) that has no local
        // `@push('head')` of its own now also carries this wire
        // component's contribution, wrapped in its own addressable marker.
        assert!(
            page.body().contains("<title>New Post - Larust</title>"),
            "page was: {}",
            page.body()
        );
        assert!(page.body().contains("<!--wire-head:"));
    })
    .await;
}

#[tokio::test]
async fn a_live_re_render_ships_the_updated_title_as_a_head_patch_header() {
    larust_core::Application::new(demo::config::app::config).unwrap();
    ensure_registered();

    let migrations_dir = std::path::Path::new("database/migrations");
    larust_testing::test_transaction(migrations_dir, |pool| async move {
        let router = build_router(&pool).await;
        let mut client = TestClient::new(router.clone(), &pool);
        login(&mut client, "Dave", "dave-post-form@example.com").await;

        let page = client.get("/posts/create").await;
        let wire_id = extract_wire_id(page.body());
        let csrf_token = page
            .meta_csrf_token()
            .expect("create page should render a csrf-token meta tag");

        // Leaves `content` blank so the submit fails validation and
        // re-renders in place (no redirect) - the exact "stays on the same
        // page" moment a later push needs to reach the client through this
        // header rather than a freshly-rendered `@stack`.
        let response = client
            .post_json(
                &format!("/__larust_wire/{wire_id}"),
                &csrf_token,
                &larust_support::serde_json::json!({
                    "props": { "title": "Draft Idea", "tags": "", "content": "" },
                    "action": { "name": "post", "args": null }
                }),
            )
            .await;

        response.assert_status(StatusCode::OK);
        assert!(
            response.header("x-wire-redirect").is_none(),
            "a validation failure must not redirect"
        );
        assert!(response.body().contains("Content is required."));

        let encoded = response
            .header("x-wire-head-patch")
            .expect("a title-changing re-render should carry a head patch");
        assert_eq!(
            decode_header(encoded).trim(),
            "<title>Draft Idea - Larust</title>"
        );
    })
    .await;
}
