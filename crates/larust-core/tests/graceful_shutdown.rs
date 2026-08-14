//! End-to-end proof that `Application::with_graceful_shutdown(...)` (added
//! in `crates/larust-core/src/lifecycle/`) actually drains an in-flight
//! request and rejects new work instead of either exiting instantly (the
//! old, still-default behavior with no builder call) or hanging forever.
//!
//! This genuinely can't be a `#[tokio::test]` — it needs a real OS process
//! (spawned from `graceful_shutdown_fixture.rs`, this crate's own bin
//! target, resolved via `CARGO_BIN_EXE_...` so there's no manual
//! `target/debug/...` path guessing) and real signal delivery.
//!
//! Signal delivery itself needed its own empirical spike before this test
//! could be written correctly: `tokio::signal::ctrl_c()` on Windows only
//! ever resolves on a real `CTRL_C_EVENT`, but `GenerateConsoleCtrlEvent`
//! can only target a *specific* other process (not "all processes sharing
//! the sender's console," which is all `CTRL_C_EVENT` allows) via
//! `CTRL_BREAK_EVENT` — and an application with no handler for that event
//! is simply terminated outright by the OS's own default handler
//! (`STATUS_CONTROL_C_EXIT`), skipping graceful shutdown entirely. Fixed
//! in `lifecycle::signal::wait_for_termination` by also listening for
//! `tokio::signal::windows::ctrl_break()`; see `docs/GOTCHAS.md`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

fn reserve_port() -> u16 {
    // Bind to an OS-assigned free port, read it back, then drop the
    // listener so the fixture process can bind it instead. A small,
    // standard test-suite race (something else could grab the port in the
    // gap) — acceptable here, not worth a more elaborate reservation
    // scheme for one test.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn spawn_fixture(port: u16) -> Child {
    let exe = env!("CARGO_BIN_EXE_graceful_shutdown_fixture");
    let mut cmd = Command::new(exe);
    cmd.env("APP_PORT", port.to_string())
        .env_remove("LARUST_DEV_RELOAD")
        // Isolated from any `.env`/`config/app.toml` this crate's own
        // directory (or anything above it) might otherwise contain.
        .current_dir(std::env::temp_dir())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Required for `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid)`
        // below to be able to target this one child specifically, rather
        // than every process sharing this test's own console.
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }

    cmd.spawn()
        .expect("failed to spawn graceful_shutdown_fixture")
}

#[cfg(windows)]
fn send_termination_signal(child: &Child) {
    use windows_sys::Win32::System::Console::{GenerateConsoleCtrlEvent, CTRL_BREAK_EVENT};
    let pid = child.id();
    let ok = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) };
    assert_ne!(
        ok,
        0,
        "GenerateConsoleCtrlEvent failed: {}",
        std::io::Error::last_os_error()
    );
}

#[cfg(unix)]
fn send_termination_signal(child: &Child) {
    let pid = child.id() as i32;
    let ret = unsafe { libc::kill(pid, libc::SIGTERM) };
    assert_eq!(
        ret,
        0,
        "kill(SIGTERM) failed: {}",
        std::io::Error::last_os_error()
    );
}

fn wait_until_listening(addr: &str, timeout: Duration) {
    let start = std::time::Instant::now();
    loop {
        if TcpStream::connect(addr).is_ok() {
            return;
        }
        assert!(
            start.elapsed() <= timeout,
            "graceful_shutdown_fixture never started listening on {addr}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn http_get(addr: &str, path: &str, timeout: Duration) -> std::io::Result<(u16, String)> {
    let mut stream = TcpStream::connect(addr)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let status = response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default();
    Ok((status, body))
}

#[test]
fn graceful_shutdown_drains_an_in_flight_request_and_rejects_new_ones() {
    let port = reserve_port();
    let addr = format!("127.0.0.1:{port}");

    let mut child = spawn_fixture(port);
    wait_until_listening(&addr, Duration::from_secs(10));

    // Started in the background: this is what actually proves the
    // shutdown *drains* in-flight work instead of dropping it. `/slow`
    // sleeps 2s server-side, comfortably longer than every delay below.
    let slow_addr = addr.clone();
    let slow_handle =
        std::thread::spawn(move || http_get(&slow_addr, "/slow", Duration::from_secs(10)));

    // Give the slow request time to actually reach the server (not just
    // be queued client-side) before signaling shutdown.
    std::thread::sleep(Duration::from_millis(300));

    send_termination_signal(&child);

    // Shortly after the signal, a fresh request must not be served
    // successfully — whether the OS surfaces that as a refused
    // connection, a reset, or a hang past this short timeout depends on
    // platform/timing details this test shouldn't have to pin exactly;
    // what matters is that it's not a normal 200 "fast-ok" response.
    std::thread::sleep(Duration::from_millis(300));
    let post_signal = http_get(&addr, "/fast", Duration::from_millis(800));
    let served_successfully = matches!(post_signal, Ok((200, ref body)) if body == "fast-ok");
    assert!(
        !served_successfully,
        "a new request was served successfully after the shutdown signal: {post_signal:?}"
    );

    // The actually-in-flight request must still complete successfully —
    // the entire point of graceful, as opposed to instant, shutdown.
    let (status, body) = slow_handle
        .join()
        .expect("slow request thread panicked")
        .expect("slow request failed outright");
    assert_eq!(
        status, 200,
        "in-flight request did not complete successfully"
    );
    assert_eq!(body, "slow-ok");

    // Must actually exit, within the fixture's own 8s drain_timeout —
    // never hang forever.
    let status = child
        .wait()
        .expect("failed to wait on graceful_shutdown_fixture");
    assert!(
        status.success(),
        "graceful_shutdown_fixture exited non-zero: {status:?}"
    );
}
