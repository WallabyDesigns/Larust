//! End-to-end proof of `larust_reverb`: a real WebSocket client connects
//! to a real running server, and a [`larust_reverb::broadcast_event`] call
//! reaches it as a `{event, data}` envelope — plus the authorization gate
//! for `private-`-prefixed channels, which has no equivalent anywhere in
//! `larust_live::push`. Same "bind a real TCP listener, run the server in
//! a background task" technique `larust-live/tests/push_test.rs` uses,
//! for the same reason: a WebSocket needs a genuinely long-lived, duplex
//! connection that `tower::ServiceExt::oneshot` can't express.
//!
//! `authorize(...)` is a real, process-wide, call-once registration (see
//! its own doc comment) — every `#[tokio::test]` in this file shares the
//! same OS process and therefore the same registration, and `cargo test`
//! runs them concurrently, not in file order. So every test below installs
//! the *same* callback content (via `ensure_authorizer_registered`,
//! itself `Once`-guarded) rather than each trying to register its own —
//! whichever test's call actually wins the race, the behavior every other
//! test observes is identical either way. That shared callback also
//! records every channel name it's invoked for (`INVOKED_CHANNELS`), so
//! the "a public channel never invokes it" test can check for its own
//! unique channel name rather than an absolute/delta count, which would
//! otherwise race against other tests' legitimate concurrent private-
//! channel invocations. The one genuinely different case — no authorizer
//! registered *at all* — can't coexist with any of that in one process,
//! so it lives in its own file (`reverb_unauthorized_test.rs`), which
//! Rust compiles as a separate test binary/process.
//!
//! `Session` extraction (needed only for the private-channel path) means
//! every test router here carries `tower_sessions`' session layer, backed
//! by a temp-file SQLite pool — `session_layer` only ever creates its own
//! `sessions` table, so no app migrations directory is needed just to
//! exercise this.

use axum::routing::get;
use axum::Router;
use futures_util::StreamExt;
use larust_reverb::{authorize, broadcast_event, socket};
use serde_json::json;
use std::sync::{Mutex, Once, OnceLock};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

/// Every channel this test file's shared authorizer allows.
const ALLOWED_PRIVATE_CHANNEL: &str = "private-always-allow-this-one";

/// Every channel name the shared authorizer has ever been invoked for —
/// checked by *name*, not by count, so `a_public_channel_never_invokes_
/// the_authorizer` stays correct even though other tests in this same
/// process legitimately invoke the authorizer for their own private
/// channels concurrently (a flat invocation counter raced against that:
/// a delta taken before/after this test's own connection could still
/// pick up an unrelated test's concurrent private-channel invocation in
/// between). Each test's channel name is unique to that test, so
/// checking "was *this* name ever recorded" is race-free regardless of
/// what else is running at the same time.
static INVOKED_CHANNELS: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
static REGISTER_ONCE: Once = Once::new();

fn ensure_authorizer_registered() {
    REGISTER_ONCE.call_once(|| {
        authorize(|_session, channel| async move {
            INVOKED_CHANNELS
                .get_or_init(|| Mutex::new(Vec::new()))
                .lock()
                .unwrap()
                .push(channel.clone());
            channel == ALLOWED_PRIVATE_CHANNEL
        });
    });
}

/// Every test in this file shares one process-wide pool —
/// `larust_orm::connect()` is a real once-per-process singleton, so the
/// first call here wins and every later call's "already connected" error
/// is deliberately swallowed. A real temp-file database, not
/// `sqlite::memory:`: a pool can open more than one physical connection,
/// and pooled `:memory:` connections each get their own private, empty
/// database without explicit shared-cache URI mode.
async fn spawn_server() -> u16 {
    let dir = tempfile::tempdir().unwrap().keep();
    let database_url = format!("sqlite://{}/test.sqlite", dir.display());
    let _ = larust_orm::connect(&database_url).await;
    let pool = larust_orm::pool().unwrap().clone();
    let session_layer = larust_http::session::session_layer(&pool, false)
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
    port
}

/// Same "a broadcast sent before the server side has actually subscribed
/// is simply never delivered" race `push_test.rs` documents — retried
/// rather than relying on one fixed sleep.
async fn broadcast_until_delivered(channel: &str, event_name: &str, payload: &serde_json::Value) {
    for _ in 0..50 {
        broadcast_event(channel, event_name, payload).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_public_channel_subscriber_receives_the_event_envelope() {
    let port = spawn_server().await;
    let url = format!("ws://127.0.0.1:{port}/__larust_reverb/orders.42");
    let (mut ws, _response) = connect_async(url).await.expect("client should connect");

    let payload = json!({ "id": 42 });
    let received = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                _ = broadcast_until_delivered("orders.42", "OrderShipped", &payload) => {}
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

    assert_eq!(received, r#"{"data":{"id":42},"event":"OrderShipped"}"#);
}

#[tokio::test]
async fn a_broadcast_on_a_different_channel_is_not_received() {
    let port = spawn_server().await;
    let url = format!("ws://127.0.0.1:{port}/__larust_reverb/orders.1");
    let (mut ws, _response) = connect_async(url).await.expect("client should connect");

    tokio::time::sleep(Duration::from_millis(200)).await;
    broadcast_event("orders.2", "OrderShipped", &json!({ "id": 2 })).unwrap();

    let result = tokio::time::timeout(Duration::from_millis(300), ws.next()).await;
    assert!(
        result.is_err(),
        "a message arrived on orders.1's socket from an unrelated broadcast on orders.2"
    );
}

#[tokio::test]
async fn a_private_channel_the_authorizer_denies_is_rejected() {
    ensure_authorizer_registered();

    let port = spawn_server().await;
    let url = format!("ws://127.0.0.1:{port}/__larust_reverb/private-not-on-the-allow-list");
    let result = connect_async(url).await;
    assert!(
        result.is_err(),
        "a channel the authorizer denies should refuse the upgrade"
    );
}

#[tokio::test]
async fn a_private_channel_the_authorizer_allows_connects_and_receives_broadcasts() {
    ensure_authorizer_registered();

    let port = spawn_server().await;
    let url = format!("ws://127.0.0.1:{port}/__larust_reverb/{ALLOWED_PRIVATE_CHANNEL}");
    let (mut ws, _response) = connect_async(url)
        .await
        .expect("an allowed private channel should connect");

    let payload = json!({ "n": 1 });
    let received = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                _ = broadcast_until_delivered(ALLOWED_PRIVATE_CHANNEL, "Notified", &payload) => {}
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

    assert_eq!(received, r#"{"data":{"n":1},"event":"Notified"}"#);
}

#[tokio::test]
async fn a_public_channel_never_invokes_the_authorizer() {
    ensure_authorizer_registered();

    // Unique to this test — see `INVOKED_CHANNELS`'s own doc comment for
    // why this is checked by name rather than via a before/after count.
    const PUBLIC_CHANNEL: &str = "a-plain-public-channel-for-the-never-invoked-test";

    let port = spawn_server().await;
    let url = format!("ws://127.0.0.1:{port}/__larust_reverb/{PUBLIC_CHANNEL}");
    let (_ws, _response) = connect_async(url)
        .await
        .expect("a public channel should connect");

    tokio::time::sleep(Duration::from_millis(200)).await;
    let invoked = INVOKED_CHANNELS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap();
    assert!(
        !invoked.iter().any(|channel| channel == PUBLIC_CHANNEL),
        "a public channel must never invoke the private-channel authorizer: {invoked:?}"
    );
}
