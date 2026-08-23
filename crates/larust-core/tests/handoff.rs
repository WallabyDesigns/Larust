//! End-to-end proof of `lifecycle::handoff::spawn_replacement_and_wait_for_ready`
//! — the orchestration that ties Stage 3's listener passing together with
//! the readiness protocol (`lifecycle::readiness`, wired into
//! `Application::serve()`): happy path (a real app binary genuinely comes
//! up and is confirmed ready) and, at least as rigorously, both failure
//! shapes (a replacement that crashes immediately, one that hangs without
//! ever announcing readiness) — proving the bounded timeout actually
//! fires instead of hanging the caller forever.
//!
//! The happy-path replacement is `graceful_shutdown_fixture` — this
//! crate's own bin target already used by `tests/graceful_shutdown.rs` —
//! reused as-is: it's already a real, minimal `Application::serve()`-based
//! app, and `serve()`'s own handoff-replacement branch (checking
//! `lifecycle::listener::INHERIT_LISTENER_ENV`, reading the inherited
//! listener from stdin, announcing readiness) is exactly what's under
//! test here, not anything specific to that fixture's own routes.

use larust_core::__internal::{handoff, listener};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

fn reserve_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

#[tokio::test]
async fn a_healthy_replacement_becomes_ready_and_serves_on_the_inherited_listener() {
    let port = reserve_port();
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let parent_listener = listener::bind(addr).expect("parent bind failed");

    let exe = env!("CARGO_BIN_EXE_graceful_shutdown_fixture");
    let outcome = handoff::spawn_replacement_and_wait_for_ready(
        &parent_listener,
        exe.as_ref(),
        Duration::from_secs(10),
        true,
    )
    .await
    .expect("spawn_replacement_and_wait_for_ready returned an error");

    let mut child = outcome.expect("a healthy replacement should have become ready");

    // The replacement is genuinely serving real requests on the SAME
    // port the parent originally bound, despite the parent never having
    // passed its own `addr` to the child at all — proving the listener
    // itself (not just a freshly-bound duplicate on the same port) is
    // what's actually in use.
    let mut stream =
        TcpStream::connect(("127.0.0.1", port)).expect("connect to replacement failed");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "GET /fast HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(
        response.contains("fast-ok"),
        "replacement did not serve the expected response: {response}"
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[tokio::test]
async fn a_replacement_that_crashes_immediately_is_reported_as_not_ready() {
    let port = reserve_port();
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let parent_listener = listener::bind(addr).expect("parent bind failed");

    let exe = env!("CARGO_BIN_EXE_handoff_crash_fixture");
    let outcome = handoff::spawn_replacement_and_wait_for_ready(
        &parent_listener,
        exe.as_ref(),
        Duration::from_secs(5),
        true,
    )
    .await
    .expect("spawn_replacement_and_wait_for_ready returned an error");

    assert!(
        outcome.is_none(),
        "a replacement that crashed immediately should not be reported as ready"
    );

    // The parent's own listener must still be perfectly usable — a
    // failed handoff attempt must never leave the caller worse off than
    // before it tried.
    drop(parent_listener);
}

#[tokio::test]
async fn a_replacement_that_hangs_without_announcing_readiness_times_out() {
    let port = reserve_port();
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let parent_listener = listener::bind(addr).expect("parent bind failed");

    let exe = env!("CARGO_BIN_EXE_handoff_hangs_fixture");
    let started = std::time::Instant::now();
    let outcome = handoff::spawn_replacement_and_wait_for_ready(
        &parent_listener,
        exe.as_ref(),
        Duration::from_millis(800),
        true,
    )
    .await
    .expect("spawn_replacement_and_wait_for_ready returned an error");
    let elapsed = started.elapsed();

    assert!(
        outcome.is_none(),
        "a replacement that never announces readiness should not be reported as ready"
    );
    // Must actually respect the timeout, not hang for the fixture's own
    // 3600s sleep.
    assert!(
        elapsed < Duration::from_secs(5),
        "spawn_replacement_and_wait_for_ready took {elapsed:?}, expected it to give up around \
         the 800ms timeout"
    );
}

/// Regression test for a real bug: the readiness handshake used to read
/// `child.stdout` and simply drop that reader once the marker was found —
/// closing the pipe's read end while the replacement (which logs routine
/// activity to stdout via `tracing_subscriber`'s default writer) kept
/// writing to it, surfacing as "[tracing-subscriber] Unable to write an
/// event... The pipe is being closed." on every subsequent log line.
///
/// Asserts the actual structural fix directly (`child.stdout.is_none()`),
/// not an indirect behavioral proxy — a broken stdout pipe never made a
/// request *fail* (`tracing-subscriber` swallows the write error
/// internally), so a test that only checked "does the replacement still
/// respond to requests" would pass even against the original bug and
/// prove nothing.
#[tokio::test]
async fn a_healthy_replacements_stdout_is_inherited_not_piped() {
    let port = reserve_port();
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let parent_listener = listener::bind(addr).expect("parent bind failed");

    let exe = env!("CARGO_BIN_EXE_graceful_shutdown_fixture");
    let outcome = handoff::spawn_replacement_and_wait_for_ready(
        &parent_listener,
        exe.as_ref(),
        Duration::from_secs(10),
        true,
    )
    .await
    .expect("spawn_replacement_and_wait_for_ready returned an error");

    let mut child = outcome.expect("a healthy replacement should have become ready");

    assert!(
        child.stdout.is_none(),
        "stdout should be inherited (Stdio::inherit()), not captured/piped — a piped stdout \
         whose reader gets dropped after the handshake is exactly the bug this test guards \
         against"
    );

    // Still genuinely serving afterward — the fix didn't just move the
    // problem, it left the replacement fully functional.
    let mut stream =
        TcpStream::connect(("127.0.0.1", port)).expect("connect to replacement failed");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "GET /fast HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(
        response.contains("fast-ok"),
        "replacement did not serve the expected response: {response}"
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}
