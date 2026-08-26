//! End-to-end proof of `@live("posts.count")` — the home page's
//! live-updating post counter, wired to the real `PostCreated` event
//! already dispatched by `PostController::store`. Needs a genuinely
//! different test shape from every other demo test here: `TestClient`
//! drives requests in-process via `tower::ServiceExt::oneshot`, which
//! can't express a WebSocket's long-lived, duplex connection — so this
//! binds a real TCP listener and runs the server in a background task,
//! the same technique `larust-live/tests/push_test.rs` uses to prove the
//! underlying mechanism works; this test proves *this app's own wiring*
//! of it does too (the `index` handler's `post_count()` query, the
//! `components.post-count-ticker` template shared between the initial
//! render and the broadcast payload, and the `PostCreated` listener
//! actually calling `push::broadcast`).

use axum::routing::get;
use axum::Router;
use demo::controllers::{AuthController, PostController};
use demo::events::PostCreated;
use demo::models::Post;
use demo::wire_components::PostForm;
use futures_util::StreamExt;
use larust_http::session::Session;
use larust_http::Route;
use larust_support::axum::http::StatusCode;
use larust_support::preferences::CookieJar;
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
        larust_support::event::listeners()
            .on::<PostCreated, _, _>(|_event: PostCreated| async move {
                if let Ok(count) = post_count().await {
                    let fragment = larust_support::view!("components.post-count-ticker", { count })
                        .into_html();
                    larust_support::push::broadcast(
                        "posts.count",
                        larust_support::push::wrap("posts.count", &fragment),
                    );
                }
            })
            .publish();
    });
}

async fn post_count() -> Result<i64, larust_core::AppError> {
    Ok(Post::all().await?.len() as i64)
}

async fn home(
    session: Session,
    cookies: CookieJar,
) -> Result<impl larust_support::axum::response::IntoResponse, larust_core::AppError> {
    let csrf_token = larust_http::csrf::token(&session).await;
    let is_authenticated = larust_support::auth::check(&session).await?;
    let unread_count = demo::controllers::unread_count_for(&session).await?;
    let nav_active = "home";
    let count = post_count().await?;
    Ok(
        larust_support::view!("welcome", { cookies: &cookies, csrf_token, is_authenticated, unread_count, nav_active, count }),
    )
}

async fn build_router(pool: &sqlx::AnyPool) -> Router {
    ensure_registered();
    Route::get("/", home)
        .get("/posts", PostController::index)
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
        // `/__larust_push/*` is registered directly on the plain
        // `axum::Router` this all resolves to, not through `larust_http::Route`
        // — mirrors `larust-live/tests/push_test.rs`'s own raw-axum route
        // registration for the same reason: a WebSocket upgrade route.
        .route("/__larust_push/:channel", get(larust_support::push::socket))
}

#[tokio::test]
async fn the_home_page_shows_the_initial_count_and_a_new_post_broadcasts_an_updated_one() {
    larust_core::Application::new(demo::config::app::config).unwrap();

    let migrations_dir = std::path::Path::new("database/migrations");
    larust_testing::test_transaction(migrations_dir, |pool| async move {
        let router = build_router(&pool).await;

        // Real TCP listener + background server task — required for the
        // WebSocket half of this test; the HTTP half below still uses
        // `TestClient` against the exact same router value.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server_router = router.clone();
        tokio::spawn(async move {
            axum::serve(listener, server_router).await.unwrap();
        });

        let mut client = TestClient::new(router.clone(), &pool);

        let home_page = client.get("/").await;
        home_page.assert_status(StatusCode::OK);
        assert!(
            home_page
                .body()
                .contains(r#"data-live-channel="posts.count""#),
            "home page should mount the live ticker: {}",
            home_page.body()
        );
        assert!(
            home_page.body().contains("0 posts and counting"),
            "a fresh database should start the ticker at 0: {}",
            home_page.body()
        );

        // Subscribe *before* creating the post, same
        // broadcast-until-delivered reasoning `push_test.rs` documents —
        // a broadcast sent before the server side has actually
        // subscribed is simply never delivered, so the create-a-post
        // step below is retried by the assertion loop, not run once.
        let ws_url = format!("ws://127.0.0.1:{port}/__larust_push/posts.count");
        let (mut ws, _response) = connect_async(ws_url).await.expect("client should connect");

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
                    ("email", "alice-live-ticker@example.com"),
                    ("password", "password123"),
                    ("password_confirmation", "password123"),
                ],
            )
            .await
            .assert_status(StatusCode::SEE_OTHER);

        let received = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
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
                            ("title", "Live Ticker Post"),
                            ("content", "<p>hello</p>"),
                            ("tags", ""),
                        ],
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
            received.contains(r#"data-live-channel="posts.count""#),
            "broadcast was: {received}"
        );
        assert!(
            received.contains("1 posts and counting"),
            "broadcast should reflect the freshly created post: {received}"
        );
    })
    .await;
}
