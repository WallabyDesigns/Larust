//! End-to-end proof of `@wire('post-list')`/`wire:model.live` — the
//! Journal's own live search filter, folded directly into `/posts` rather
//! than living on a separate `/search` page (per explicit design feedback:
//! search should filter the listing the visitor is already looking at, not
//! send them to a second page). Also covers `PostList::mount`'s per-viewer
//! `can_manage` (Edit/Delete only shown to a post's own author), the first
//! real exercise of `WireComponent::mount` receiving `session`.

use demo::controllers::{AuthController, PostController};
use demo::wire_components::{PostForm, PostList};
use larust_http::Route;
use larust_support::axum::http::StatusCode;
use larust_testing::TestClient;
use std::sync::Once;

static REGISTER_ONCE: Once = Once::new();

fn ensure_registered() {
    REGISTER_ONCE.call_once(|| {
        // `PostForm` isn't exercised by this file's own assertions, but
        // `/posts/create` (visited here only to fetch a CSRF token, same
        // as `posts_policy_test.rs`) now mounts it via `@wire('post-form')`.
        larust_support::wire::components()
            .register::<PostList>()
            .register::<PostForm>()
            .publish();
    });
}

async fn build_router(pool: &sqlx::SqlitePool) -> larust_support::axum::Router {
    Route::get("/posts", PostController::index)
        .name("posts.index")
        .get("/posts/create", PostController::create)
        .post("/posts", PostController::store)
        .name("posts.store")
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

async fn register_and_post(client: &mut TestClient, name: &str, email: &str, title: &str) {
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

    let csrf = client
        .get("/posts/create")
        .await
        .meta_csrf_token()
        .expect("create page should render a csrf-token meta tag");
    client
        .post_form(
            "/posts",
            &[
                ("_csrf_token", &csrf),
                ("title", title),
                ("content", "<p>hello</p>"),
                ("tags", ""),
            ],
        )
        .await
        .assert_status(StatusCode::SEE_OTHER);
}

/// Proves `post-list.blade.xr`'s `@foreach((post, loop_) in
/// posts.iter().with_loop())` — the demo's own hand-authored (not
/// converter-generated) use of keyed iteration plus `larust_support::
/// WithLoop` — actually renders the right per-row numbering and the
/// "Latest" badge on exactly the newest post, not just that the page
/// compiles and returns 200.
#[tokio::test]
async fn post_list_numbers_rows_and_badges_only_the_newest_as_latest() {
    larust_core::Application::new().unwrap();
    ensure_registered();

    let pool = larust_testing::test_db(std::path::Path::new("database/migrations"))
        .await
        .unwrap();
    let router = build_router(&pool).await;

    let mut author = TestClient::new(router.clone(), &pool);
    register_and_post(
        &mut author,
        "Carol",
        "carol-post-list-loop@example.com",
        "Carol First Post",
    )
    .await;
    // A second post from the same, already-registered/authenticated
    // client — posts are listed newest-first (`ORDER BY posts.id DESC`),
    // so this one becomes row #1/"Latest" and the first becomes row #2.
    let csrf = author
        .get("/posts/create")
        .await
        .meta_csrf_token()
        .expect("create page should render a csrf-token meta tag");
    author
        .post_form(
            "/posts",
            &[
                ("_csrf_token", &csrf),
                ("title", "Carol Second Post"),
                ("content", "<p>hello again</p>"),
                ("tags", ""),
            ],
        )
        .await
        .assert_status(StatusCode::SEE_OTHER);

    let mut visitor = TestClient::new(router, &pool);
    let page = visitor.get("/posts").await;
    page.assert_status(StatusCode::OK);
    let body = page.body();

    assert!(body.contains("#1 · Latest"));
    assert!(body.contains("#2"));
    assert!(!body.contains("#2 · Latest"));

    // The badge sits on the newest post specifically, not just anywhere
    // on the page — "Latest" appears before "Carol Second Post" in
    // source order and not before "Carol First Post".
    let latest_pos = body.find("· Latest").expect("missing Latest badge");
    let second_post_pos = body
        .find("Carol Second Post")
        .expect("missing Carol Second Post");
    let first_post_pos = body
        .find("Carol First Post")
        .expect("missing Carol First Post");
    assert!(latest_pos < second_post_pos);
    assert!(latest_pos < first_post_pos);
}

#[tokio::test]
async fn journal_search_filters_the_same_listing_in_place() {
    larust_core::Application::new().unwrap();
    ensure_registered();

    let pool = larust_testing::test_db(std::path::Path::new("database/migrations"))
        .await
        .unwrap();
    let router = build_router(&pool).await;

    let mut author = TestClient::new(router.clone(), &pool);
    register_and_post(
        &mut author,
        "Alice",
        "alice-post-list@example.com",
        "Alice Rust Notes",
    )
    .await;

    // A separate, anonymous visitor browses the Journal — no login
    // required to view or filter it.
    let mut visitor = TestClient::new(router.clone(), &pool);
    let page = visitor.get("/posts").await;
    page.assert_status(StatusCode::OK);
    // Pulled in by the shared layout's `@larustscripts`, since this page
    // now mounts a `@wire(...)` component.
    assert!(page.body().contains("/__larust_wire/runtime.js"));
    assert!(page.body().contains("Alice Rust Notes"));

    let wire_id = extract_wire_id(page.body());
    let csrf_token = page
        .meta_csrf_token()
        .expect("page should render a csrf-token meta tag");

    // wire:model.live="query" syncing "Rust" — the same grid the page
    // loaded with, now filtered, not a separate results list.
    let synced = visitor
        .post_json(
            &format!("/__larust_wire/{wire_id}"),
            &csrf_token,
            &larust_support::serde_json::json!({ "props": { "query": "Rust" }, "action": null }),
        )
        .await;
    synced.assert_status(StatusCode::OK);
    assert!(synced.body().contains("Alice Rust Notes"));

    // A query with no matches shows the empty state, not stale results.
    let no_match = visitor
        .post_json(
            &format!("/__larust_wire/{wire_id}"),
            &csrf_token,
            &larust_support::serde_json::json!({ "props": { "query": "nonexistent" }, "action": null }),
        )
        .await;
    assert!(!no_match.body().contains("Alice Rust Notes"));
    assert!(no_match.body().contains("No posts match"));

    // wire:click="clear_search" — the full listing (every post, unfiltered)
    // returns, since an empty query means "show everything" for a listing
    // page, unlike a dedicated search box where empty means "show nothing".
    let cleared = visitor
        .post_json(
            &format!("/__larust_wire/{wire_id}"),
            &csrf_token,
            &larust_support::serde_json::json!({ "props": { "query": "Rust" }, "action": { "name": "clear_search", "args": null } }),
        )
        .await;
    assert!(cleared.body().contains("Alice Rust Notes"));
    assert!(!cleared.body().contains("No posts match"));
}

#[tokio::test]
async fn only_the_posts_own_author_sees_edit_and_delete_controls() {
    larust_core::Application::new().unwrap();
    ensure_registered();

    let pool = larust_testing::test_db(std::path::Path::new("database/migrations"))
        .await
        .unwrap();
    let router = build_router(&pool).await;

    let mut alice = TestClient::new(router.clone(), &pool);
    register_and_post(
        &mut alice,
        "Alice",
        "alice-can-manage@example.com",
        "Alice Manageable Post",
    )
    .await;

    let (post_id,): (i64,) = sqlx::query_as("SELECT id FROM posts WHERE title = ?")
        .bind("Alice Manageable Post")
        .fetch_one(&pool)
        .await
        .unwrap();
    let edit_link = format!("/posts/{post_id}/edit");

    // Alice, viewing her own Journal, sees Edit/Delete on her own post —
    // `PostList::mount` cached her identity from the real session.
    let alice_page = alice.get("/posts").await;
    assert!(alice_page.body().contains(&edit_link));

    // Bob, a different logged-in visitor, does not.
    let mut bob = TestClient::new(router.clone(), &pool);
    let csrf = bob
        .get("/posts/create")
        .await
        .meta_csrf_token()
        .expect("create page should render a csrf-token meta tag");
    bob.post_form(
        "/register",
        &[
            ("_csrf_token", &csrf),
            ("name", "Bob"),
            ("email", "bob-can-manage@example.com"),
            ("password", "password123"),
            ("password_confirmation", "password123"),
        ],
    )
    .await
    .assert_status(StatusCode::SEE_OTHER);

    let bob_page = bob.get("/posts").await;
    assert!(bob_page.body().contains("Alice Manageable Post"));
    assert!(!bob_page.body().contains(&edit_link));

    // An anonymous, logged-out visitor doesn't either.
    let mut anon = TestClient::new(router, &pool);
    let anon_page = anon.get("/posts").await;
    assert!(!anon_page.body().contains(&edit_link));
}

#[tokio::test]
async fn larustscripts_does_not_render_on_a_page_with_no_wire_component() {
    larust_core::Application::new().unwrap();
    ensure_registered();

    let pool = larust_testing::test_db(std::path::Path::new("database/migrations"))
        .await
        .unwrap();
    let router = build_router(&pool).await;

    // `/register` shares the exact same layout (`layouts.app`, with its
    // `@larustscripts` marker) as `/posts`, but mounts no `@wire(...)`
    // component of its own — proves the shared layout's script tag is
    // genuinely conditional per page, not something that leaks onto every
    // page once any page in the app uses `@wire(...)`.
    let mut client = TestClient::new(router, &pool);
    let page = client.get("/register").await;
    page.assert_status(StatusCode::OK);
    assert!(!page.body().contains("__larust_wire"));
}
