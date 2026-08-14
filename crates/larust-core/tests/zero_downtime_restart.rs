//! End-to-end proof that the whole restart-handoff feature (graceful
//! shutdown, listener passing, the readiness protocol, the admin channel)
//! actually delivers "zero-downtime": a real app process
//! (`zero_downtime_fixture`) serves continuous, real HTTP traffic from a
//! background thread while this test sends it the exact same admin-
//! channel `RESTART` command `xr restart` sends, and asserts **zero**
//! failed requests across the entire handoff — not just that the feature
//! exists, but that it works under real concurrent load. Also asserts the
//! process actually serving requests changes pid partway through (proving
//! a genuine handoff happened, not just that the same process kept
//! running) and that exactly one process is left listening afterward (no
//! orphaned predecessor still holding the port).
//!
//! Config isolation: `zero_downtime_fixture` (via `Application::new()`)
//! and this test's own admin-channel client both need to agree on the
//! same `app_name` to compute the same admin-channel address — done via
//! a real `config/app.toml` in a dedicated tempdir (not an env var:
//! `Config::load()` has no `APP_NAME` override), keyed on the reserved
//! port so concurrent test runs can't collide with each other.

use larust_core::__internal::admin;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn reserve_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn http_get_ping(addr: &str) -> std::io::Result<String> {
    let mut stream = TcpStream::connect(addr)?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    write!(
        stream,
        "GET /ping HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.trim().to_string())
        .filter(|body| !body.is_empty())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "no body"))
}

fn wait_until_listening(addr: &str, timeout: Duration) {
    let start = Instant::now();
    loop {
        if TcpStream::connect(addr).is_ok() {
            return;
        }
        assert!(
            start.elapsed() < timeout,
            "app never started listening on {addr}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Extracts the pid a `/ping` response body embeds (`pong-{n}-pid-{pid}`).
fn pid_from_response(body: &str) -> Option<&str> {
    body.rsplit("pid-").next()
}

/// Hard-kills a process by pid — used only for test cleanup of the
/// *replacement* process, which the admin channel loop spawns internally
/// (this test never holds a `Child` handle to it) and which would
/// otherwise leak as a real, still-listening orphan once the test
/// function returns.
#[cfg(unix)]
fn kill_pid(pid: u32) {
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
}

#[cfg(windows)]
fn kill_pid(pid: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if !handle.is_null() {
            TerminateProcess(handle, 1);
            CloseHandle(handle);
        }
    }
}

#[cfg(unix)]
fn send_restart_command(address: &str) -> std::io::Result<String> {
    use std::io::BufRead;
    use std::os::unix::net::UnixStream;

    let path = std::env::temp_dir().join(format!("{address}.sock"));
    let mut stream = UnixStream::connect(&path)?;
    stream.set_read_timeout(Some(Duration::from_secs(20)))?;
    stream.write_all(admin::RESTART_COMMAND.as_bytes())?;
    stream.write_all(b"\n")?;
    let mut response = String::new();
    std::io::BufReader::new(stream).read_line(&mut response)?;
    Ok(response.trim().to_string())
}

// std has no named pipe support at all -- this mirrors
// `larust-cli/src/restart.rs`'s own client logic (a small embedded tokio
// runtime, since the rest of this test deliberately stays plain
// blocking/threaded for the concurrent-traffic-generation part below).
#[cfg(windows)]
fn send_restart_command(address: &str) -> std::io::Result<String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::windows::named_pipe::ClientOptions;

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async move {
        let name = format!(r"\\.\pipe\{address}");
        let mut last_error = None;
        let mut client = None;
        for _ in 0..50 {
            match ClientOptions::new().open(&name) {
                Ok(c) => {
                    client = Some(c);
                    break;
                }
                Err(source) => {
                    last_error = Some(source);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
        let client = client.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("couldn't connect to {name}: {last_error:?}"),
            )
        })?;
        let (reader, mut writer) = tokio::io::split(client);
        writer.write_all(admin::RESTART_COMMAND.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        let mut reader = BufReader::new(reader);
        let mut response = String::new();
        reader.read_line(&mut response).await?;
        Ok(response.trim().to_string())
    })
}

fn spawn_fixture(app_dir: &std::path::Path, port: u16) -> Child {
    let exe = env!("CARGO_BIN_EXE_zero_downtime_fixture");
    Command::new(exe)
        .env("APP_PORT", port.to_string())
        .current_dir(app_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("failed to spawn zero_downtime_fixture")
}

#[test]
fn a_live_restart_serves_every_request_with_zero_failures_and_switches_process() {
    let port = reserve_port();
    let app_name = format!("zero_downtime_test_{port}");
    let address = admin::channel_address(&app_name);
    let addr = format!("127.0.0.1:{port}");

    let app_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(app_dir.path().join("config")).unwrap();
    std::fs::write(
        app_dir.path().join("config").join("app.toml"),
        format!("app_name = \"{app_name}\"\n"),
    )
    .unwrap();

    let mut child = spawn_fixture(app_dir.path(), port);
    wait_until_listening(&addr, Duration::from_secs(10));

    // Continuous real traffic from a background thread, running for the
    // entire test — this is what actually proves "zero downtime" rather
    // than just "the feature exists": every single request's outcome is
    // recorded, and the assertion at the end demands all of them
    // succeeded, including whichever ones landed exactly during the
    // handoff window.
    let stop = Arc::new(AtomicBool::new(false));
    let failures = Arc::new(AtomicU32::new(0));
    let successes = Arc::new(AtomicU32::new(0));
    let seen_pids = Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));

    let traffic_handle = {
        let stop = Arc::clone(&stop);
        let failures = Arc::clone(&failures);
        let successes = Arc::clone(&successes);
        let seen_pids = Arc::clone(&seen_pids);
        let addr = addr.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                match http_get_ping(&addr) {
                    Ok(body) => {
                        successes.fetch_add(1, Ordering::SeqCst);
                        if let Some(pid) = pid_from_response(&body) {
                            seen_pids.lock().unwrap().insert(pid.to_string());
                        }
                    }
                    Err(_) => {
                        failures.fetch_add(1, Ordering::SeqCst);
                    }
                }
            }
        })
    };

    // Let a healthy baseline of traffic flow before triggering anything.
    std::thread::sleep(Duration::from_millis(300));

    let response = send_restart_command(&address).expect("failed to send the restart command");
    assert_eq!(
        response,
        admin::ACK_HANDOFF_STARTED,
        "the app should have accepted and started the restart handoff"
    );

    // Give the handoff (spawn replacement, wait for ready, drain, exit)
    // comfortably long enough to complete under test-machine load, while
    // traffic keeps flowing the entire time.
    std::thread::sleep(Duration::from_secs(3));

    stop.store(true, Ordering::SeqCst);
    traffic_handle.join().expect("traffic thread panicked");

    let failure_count = failures.load(Ordering::SeqCst);
    let success_count = successes.load(Ordering::SeqCst);
    assert_eq!(
        failure_count, 0,
        "expected zero failed requests across the restart, got {failure_count} \
         (out of {success_count} successful)"
    );
    assert!(
        success_count > 10,
        "expected substantial real traffic during the test, only got {success_count} requests"
    );

    let pids: Vec<String> = seen_pids.lock().unwrap().iter().cloned().collect();
    assert_eq!(
        pids.len(),
        2,
        "expected exactly two distinct process pids to have served traffic \
         (the original and its replacement), saw: {pids:?}"
    );

    // The original process should have exited on its own once its drain
    // completed — `child` here is the *original* spawned process, not
    // the replacement (which the admin channel loop inside it spawned
    // independently and this test never directly held a handle to).
    let original_pid = child.id().to_string();
    let original_status = child
        .wait()
        .expect("failed to wait on the original fixture process");
    assert!(
        original_status.success(),
        "original process should have exited cleanly after draining, got {original_status:?}"
    );

    // Exactly one process should still be listening on the port —
    // proven by successfully connecting and getting a real response one
    // more time now that the dust has settled, with no orphaned
    // predecessor left holding the port to cause a conflict.
    let final_response = http_get_ping(&addr).expect("port should still be served after restart");
    assert!(pid_from_response(&final_response).is_some());

    // Cleanup: the replacement process is still alive and listening
    // (correctly — that's the whole point) but this test has no further
    // use for it and never held a `Child` handle to it in the first
    // place, so it's killed by pid directly rather than left to leak.
    if let Some(replacement_pid) = pids.iter().find(|pid| **pid != original_pid) {
        if let Ok(pid) = replacement_pid.parse::<u32>() {
            kill_pid(pid);
        }
    }
}
