//! End-to-end proof of real-time comments: a WebSocket subscriber on
//! `posts.{id}.comments` receives a `CommentCreated` broadcast the moment
//! another client's `POST /posts/{post}/comments` request completes — the
//! same "two browsers on the same post page see each other's comments
//! live, no refresh" case a manual demo shows, automated the same way
//! `live_ticker_test.rs` proves `@live`'s ticker: a real TCP listener +
//! background server task (needed for the WebSocket half — a plain
//! `tower::ServiceExt::oneshot` can't express a long-lived duplex
//! connection), with the HTTP half still driven through `TestClient`
//! against the very same router value.

use axum::Router;
use demo::controllers::{AuthController, CommentController, PostController};
use demo::models::{Comment, Post};
use demo::wire_components::PostForm;
use futures_util::StreamExt;
use larust_http::Route;
use larust_support::axum::http::StatusCode;
use larust_testing::TestClient;
use std::sync::Once;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

static REGISTER_ONCE: Once = Once::new();

fn ensure_registered() {
    REGISTER_ONCE.call_once(|| {
        larust_support::wire::components()
            .register::<PostForm>()
            .publish();
    });
}

async fn build_router(pool: &sqlx::SqlitePool) -> Router {
    ensure_registered();
    Route::get("/posts", PostController::index)
        .name("posts.index")
        .get("/posts/create", PostController::create)
        .post("/posts", PostController::store)
        .get("/posts/{post}", PostController::show)
        .post("/posts/{post}/comments", CommentController::store)
        .post(
            "/posts/{post}/comments/{comment}/delete",
            CommentController::destroy,
        )
        .post("/posts/{post}/comments/typing", CommentController::typing)
        .get("/register", AuthController::show_register)
        .post("/register", AuthController::register)
        // Unlike `__larust_push`'s socket (no `Session` extractor needed —
        // see `live_ticker_test.rs`, which appends it as a raw route
        // *after* `.into_axum_router()`), `reverb::socket` extracts
        // `Session` to check `private-` channel authorization, so it has
        // to be registered here, before `.with_sessions(...)` below, or
        // the session layer never wraps it and every connection attempt
        // 500s with "Can't extract session".
        .get("/__larust_reverb/{channel}", larust_support::reverb::socket)
        .middleware(larust_http::axum::middleware::from_fn(
            larust_http::csrf::verify,
        ))
        .with_sessions(pool, false)
        .await
        .unwrap()
        .into_axum_router()
}

#[tokio::test]
async fn a_websocket_subscriber_sees_a_comment_another_client_just_posted() {
    larust_core::Application::new(demo::config::app::config).unwrap();

    let migrations_dir = std::path::Path::new("database/migrations");
    larust_testing::test_transaction(migrations_dir, |pool| async move {
        let router = build_router(&pool).await;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server_router = router.clone();
        tokio::spawn(async move {
            axum::serve(listener, server_router).await.unwrap();
        });

        let mut client = TestClient::new(router.clone(), &pool);

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
                    ("name", "Alice"),
                    ("email", "alice-live-comments@example.com"),
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
                    ("title", "Live Comments Post"),
                    ("content", "<p>hello</p>"),
                    ("tags", ""),
                ],
            )
            .await
            .assert_status(StatusCode::SEE_OTHER);

        let post = Post::all()
            .await
            .unwrap()
            .into_iter()
            .next()
            .expect("the post created above should exist");

        // Subscribe *before* posting the comment, same broadcast-until-
        // delivered reasoning `live_ticker_test.rs`/`push_test.rs` document
        // — a broadcast sent before the server side has actually
        // subscribed is simply never delivered, so the comment-post step
        // below is retried by the assertion loop, not run once.
        let ws_url = format!(
            "ws://127.0.0.1:{port}/__larust_reverb/posts.{}.comments",
            post.id
        );
        let (mut ws, _response) = connect_async(ws_url).await.expect("client should connect");

        let received = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let csrf = client
                    .get(&format!("/posts/{}", post.id))
                    .await
                    .meta_csrf_token()
                    .expect("show page should render a csrf-token meta tag");
                client
                    .post_form(
                        &format!("/posts/{}/comments", post.id),
                        &[("_csrf_token", &csrf), ("body", "Hello from Alice")],
                    )
                    .await;

                tokio::select! {
                    msg = ws.next() => {
                        // Filtered by event name *and* this exact comment
                        // body, not "the first thing that arrives" — this
                        // channel name is process-wide (`larust-reverb`'s
                        // registry isn't reset between tests) and every
                        // test in this file creates "post #1" in its own
                        // rolled-back transaction, so `posts.1.comments`
                        // is the same literal channel across tests
                        // running in parallel. Event name alone isn't
                        // enough here specifically — the delete test's own
                        // setup phase also posts a (differently-worded)
                        // comment on this same channel, so it's a second,
                        // genuine `CommentCreated` broadcast, not a
                        // different event type a name check would catch.
                        if let Some(Ok(Message::Text(text))) = msg {
                            if text.contains(r#""event":"CommentCreated""#)
                                && text.contains("Hello from Alice")
                            {
                                return text;
                            }
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_millis(50)) => {}
                }
            }
        })
        .await
        .expect("should have received a broadcast within the timeout");

        assert!(
            received.contains("Alice"),
            "broadcast should contain the commenter's name: {received}"
        );
    })
    .await;
}

/// Registers a user and creates one post through `client`'s own session,
/// returning the post's id — the common setup every test below needs
/// before it can exercise comment creation/deletion/typing on a real
/// post. `email` must be unique per test (they all share one process,
/// though each runs in its own rolled-back transaction via
/// `test_transaction`, so collisions are only a same-test-file-reuse risk
/// if two tests pass the same address).
async fn register_and_create_post(client: &mut TestClient, name: &str, email: &str) -> i64 {
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
                ("title", "Live Comments Post"),
                ("content", "<p>hello</p>"),
                ("tags", ""),
            ],
        )
        .await
        .assert_status(StatusCode::SEE_OTHER);

    Post::all()
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("the post created above should exist")
        .id
}

#[tokio::test]
async fn a_websocket_subscriber_sees_a_comment_deleted_by_its_own_author() {
    larust_core::Application::new(demo::config::app::config).unwrap();

    let migrations_dir = std::path::Path::new("database/migrations");
    larust_testing::test_transaction(migrations_dir, |pool| async move {
        let router = build_router(&pool).await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server_router = router.clone();
        tokio::spawn(async move {
            axum::serve(listener, server_router).await.unwrap();
        });

        let mut client = TestClient::new(router.clone(), &pool);
        let post_id =
            register_and_create_post(&mut client, "Alice", "alice-delete-comment@example.com")
                .await;

        // A single, non-retried post — the row just needs to exist; unlike
        // the create-broadcast test above, this test isn't proving
        // `CommentCreated` delivery, so there's no race to retry through.
        let csrf = client
            .get(&format!("/posts/{post_id}"))
            .await
            .meta_csrf_token()
            .unwrap();
        client
            .post_form(
                &format!("/posts/{post_id}/comments"),
                &[("_csrf_token", &csrf), ("body", "Delete me")],
            )
            .await
            .assert_status(StatusCode::SEE_OTHER);
        let comment_id = Comment::all()
            .await
            .unwrap()
            .into_iter()
            .next()
            .expect("the comment created above should exist")
            .id;

        let ws_url = format!("ws://127.0.0.1:{port}/__larust_reverb/posts.{post_id}.comments");
        let (mut ws, _response) = connect_async(ws_url).await.expect("client should connect");

        let received = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let csrf = client
                    .get(&format!("/posts/{post_id}"))
                    .await
                    .meta_csrf_token()
                    .unwrap();
                client
                    .post_form(
                        &format!("/posts/{post_id}/comments/{comment_id}/delete"),
                        &[("_csrf_token", &csrf)],
                    )
                    .await;

                tokio::select! {
                    msg = ws.next() => {
                        // Filtered by event name — see the create test's
                        // own comment on why this channel is shared
                        // across every test in this file.
                        if let Some(Ok(Message::Text(text))) = msg {
                            if text.contains(r#""event":"CommentDeleted""#) {
                                return text;
                            }
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_millis(50)) => {}
                }
            }
        })
        .await
        .expect("should have received a broadcast within the timeout");

        assert!(
            received.contains(&comment_id.to_string()),
            "broadcast should name the deleted comment's id: {received}"
        );
    })
    .await;
}

#[tokio::test]
async fn a_websocket_subscriber_sees_a_typing_broadcast() {
    larust_core::Application::new(demo::config::app::config).unwrap();

    let migrations_dir = std::path::Path::new("database/migrations");
    larust_testing::test_transaction(migrations_dir, |pool| async move {
        let router = build_router(&pool).await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server_router = router.clone();
        tokio::spawn(async move {
            axum::serve(listener, server_router).await.unwrap();
        });

        let mut client = TestClient::new(router.clone(), &pool);
        let post_id =
            register_and_create_post(&mut client, "Alice", "alice-typing@example.com").await;

        let ws_url = format!("ws://127.0.0.1:{port}/__larust_reverb/posts.{post_id}.comments");
        let (mut ws, _response) = connect_async(ws_url).await.expect("client should connect");

        let received = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let csrf = client
                    .get(&format!("/posts/{post_id}"))
                    .await
                    .meta_csrf_token()
                    .unwrap();
                client
                    .post_form(
                        &format!("/posts/{post_id}/comments/typing?tab_id=test-tab"),
                        &[("_csrf_token", &csrf)],
                    )
                    .await;

                tokio::select! {
                    msg = ws.next() => {
                        // Filtered by event name — see the create test's
                        // own comment on why this channel is shared
                        // across every test in this file.
                        if let Some(Ok(Message::Text(text))) = msg {
                            if text.contains(r#""event":"UserTyping""#) {
                                return text;
                            }
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_millis(50)) => {}
                }
            }
        })
        .await
        .expect("should have received a broadcast within the timeout");

        assert!(
            received.contains("Alice"),
            "broadcast should name the typing user: {received}"
        );
    })
    .await;
}

#[tokio::test]
async fn a_non_owner_non_moderator_cannot_delete_another_users_comment() {
    larust_core::Application::new(demo::config::app::config).unwrap();

    let migrations_dir = std::path::Path::new("database/migrations");
    larust_testing::test_transaction(migrations_dir, |pool| async move {
        let router = build_router(&pool).await;

        let mut alice = TestClient::new(router.clone(), &pool);
        let post_id =
            register_and_create_post(&mut alice, "Alice", "alice-owns-comment@example.com").await;
        let csrf = alice
            .get(&format!("/posts/{post_id}"))
            .await
            .meta_csrf_token()
            .unwrap();
        alice
            .post_form(
                &format!("/posts/{post_id}/comments"),
                &[
                    ("_csrf_token", &csrf),
                    ("body", "Only Alice may delete this"),
                ],
            )
            .await
            .assert_status(StatusCode::SEE_OTHER);
        let comment_id = Comment::all()
            .await
            .unwrap()
            .into_iter()
            .next()
            .expect("the comment created above should exist")
            .id;

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
                ("email", "bob-cannot-delete@example.com"),
                ("password", "password123"),
                ("password_confirmation", "password123"),
            ],
        )
        .await
        .assert_status(StatusCode::SEE_OTHER);

        let csrf = bob
            .get(&format!("/posts/{post_id}"))
            .await
            .meta_csrf_token()
            .unwrap();
        bob.post_form(
            &format!("/posts/{post_id}/comments/{comment_id}/delete"),
            &[("_csrf_token", &csrf)],
        )
        .await
        .assert_status(StatusCode::FORBIDDEN);

        assert!(
            Comment::find(comment_id).await.unwrap().is_some(),
            "the comment should still exist after a rejected delete attempt"
        );
    })
    .await;
}
