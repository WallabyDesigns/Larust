//! End-to-end proof of `larust_live::push`: a real WebSocket client
//! connects to a real running server, and a [`larust_live::push::broadcast`]
//! call reaches it. Unlike `wire_test.rs`/`registry_test.rs` (both driven
//! via `tower::ServiceExt::oneshot`, a single request/response), a
//! WebSocket needs a genuinely long-lived, duplex connection — `oneshot`
//! can't express that — so this binds a real TCP listener and runs the
//! server in a background task, exactly like a real deployment would.

use axum::routing::get;
use axum::Router;
use futures_util::StreamExt;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

/// Starts a real server on an OS-assigned loopback port, returning that
/// port. The server keeps running for the rest of the process (background
/// `tokio::spawn`, never joined) — acceptable in a test binary, which
/// exits the whole process when done anyway.
async fn spawn_server() -> u16 {
    let router = Router::new().route("/__larust_push/:channel", get(larust_live::push::socket));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    port
}

/// A broadcast sent *before* the server's socket task has actually called
/// `.subscribe()` (a real race inherent to any pub/sub-over-WebSocket
/// design — connecting the client doesn't guarantee the server side has
/// reached the subscribe point yet) is simply never delivered, by design
/// — there's nothing to buffer it for. Retrying the broadcast a few times
/// with a short delay, rather than one fixed sleep-then-hope, is what
/// makes this deterministic without being flaky: it just takes as many
/// attempts as the scheduler needs, bounded by the outer test timeout.
async fn broadcast_until_delivered(channel: &str, html: &str) {
    for _ in 0..50 {
        larust_live::push::broadcast(channel, html.to_string());
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_websocket_subscriber_receives_a_broadcast() {
    let port = spawn_server().await;
    let url = format!("ws://127.0.0.1:{port}/__larust_push/push-test-channel");

    let (mut ws, _response) = connect_async(url).await.expect("client should connect");

    let received = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                _ = broadcast_until_delivered("push-test-channel", "<div data-live-channel=\"push-test-channel\"><span>5 posts</span></div>") => {}
                msg = ws.next() => {
                    if let Some(Ok(Message::Text(text))) = msg {
                        return text;
                    }
                }
            }
        }
    })
    .await
    .expect("should have received a broadcast within the timeout");

    assert_eq!(
        received.to_string(),
        "<div data-live-channel=\"push-test-channel\"><span>5 posts</span></div>"
    );
}

#[tokio::test]
async fn a_broadcast_on_a_different_channel_is_not_received() {
    let port = spawn_server().await;
    let url = format!("ws://127.0.0.1:{port}/__larust_push/channel-a");
    let (mut ws, _response) = connect_async(url).await.expect("client should connect");

    // Give the server a moment to actually subscribe, same reasoning as
    // `broadcast_until_delivered` above, then broadcast on an *unrelated*
    // channel this subscriber never asked for.
    tokio::time::sleep(Duration::from_millis(200)).await;
    larust_live::push::broadcast(
        "channel-b",
        "<div data-live-channel=\"channel-b\">nope</div>",
    );

    let result = tokio::time::timeout(Duration::from_millis(300), ws.next()).await;
    assert!(
        result.is_err(),
        "a message arrived on channel-a's socket from an unrelated broadcast on channel-b"
    );
}
