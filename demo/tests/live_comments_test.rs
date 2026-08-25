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
use demo::models::Post;
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
                        if let Some(Ok(Message::Text(text))) = msg {
                            return text;
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_millis(50)) => {}
                }
            }
        })
        .await
        .expect("should have received a broadcast within the timeout");

        assert!(
            received.contains(r#""event":"CommentCreated""#),
            "broadcast was: {received}"
        );
        assert!(
            received.contains("Hello from Alice"),
            "broadcast should contain the posted comment body: {received}"
        );
        assert!(
            received.contains("Alice"),
            "broadcast should contain the commenter's name: {received}"
        );
    })
    .await;
}
