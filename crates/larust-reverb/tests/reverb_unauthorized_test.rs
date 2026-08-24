//! The one authorization scenario that can't share a process with
//! `reverb_test.rs`'s own tests: no `authorize(...)` callback registered
//! at all. `authorize` is a real, process-wide, call-once registration —
//! any other test in the same binary calling it first would poison this
//! one, so this lives in its own file, which Rust compiles as its own
//! test binary/process with a fresh, untouched `AUTHORIZER`.

use axum::routing::get;
use axum::Router;
use larust_reverb::socket;
use tokio::net::TcpListener;
use tokio_tungstenite::connect_async;

#[tokio::test]
async fn a_private_channel_with_no_authorizer_registered_is_rejected() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    let session_layer = larust_http::session::sqlite_session_layer(&pool, false)
        .await
        .unwrap();
    let router = Router::new()
        .route("/__larust_reverb/:channel", get(socket))
        .layer(session_layer);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let url = format!("ws://127.0.0.1:{port}/__larust_reverb/private-nobody-authorized-this");
    let result = connect_async(url).await;
    assert!(
        result.is_err(),
        "a private channel with no authorizer registered should fail closed"
    );
}
