//! Regression test for a real bug found while designing zero-downtime
//! `xr dev` reload: `Application::serve()` used to call
//! `lifecycle::handoff::resolve_binary_path()` exactly once, before ever
//! entering the admin-channel accept loop - meaning whatever
//! `storage/releases/current` said *at boot* is what a long-running
//! process would respawn on every future `RESTART` it ever received, not
//! whatever the pointer says *at the moment* `RESTART` actually arrives.
//! A process that outlives even one pointer update would silently
//! respawn a stale binary forever.
//!
//! This test starts a real app with no pointer file present (so it falls
//! back to `current_exe()`, i.e. `stale_pointer_fixture_v1`'s own path),
//! *then* writes `storage/releases/current` pointing at a genuinely
//! different binary (`stale_pointer_fixture_v2`, distinguishable by its
//! `/ping` response), *then* sends `RESTART` - and asserts the
//! replacement that comes up is v2, not v1.

use larust_core::__internal::admin;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Kills and reaps `child` on drop, including during a panic-driven
/// unwind (unlike a plain `child.wait()` at the end of a test function,
/// which never runs if an earlier assertion panics). Without this, a
/// *failing* run of this test - the exact case a regression test must
/// handle gracefully - orphans a real, still-listening process: bitten by
/// this once already while first writing this test, where the orphan's
/// inherited stderr handle kept the test runner's own output pipe open
/// long after the test itself had already finished, making a completed,
/// correctly-failing run look like an indefinite hang from the outside.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

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

fn spawn_fixture(app_dir: &std::path::Path, port: u16, app_name: &str) -> Child {
    let exe = env!("CARGO_BIN_EXE_zero_downtime_fixture");
    Command::new(exe)
        .env("APP_PORT", port.to_string())
        .env("APP_NAME", app_name)
        .current_dir(app_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("failed to spawn zero_downtime_fixture")
}

#[test]
fn a_restart_uses_the_pointer_files_current_value_not_whatever_it_said_at_boot() {
    let port = reserve_port();
    let app_name = format!("stale_pointer_test_{port}");
    let address = admin::channel_address(&app_name);
    let addr = format!("127.0.0.1:{port}");

    let app_dir = tempfile::tempdir().unwrap();

    // No `storage/releases/current` exists yet at boot - the v1 process
    // falls back to `current_exe()` (its own path). That's deliberate:
    // the bug this test targets is specifically about a pointer written
    // *after* boot being ignored, not about the fallback path itself.
    //
    // Wrapped immediately so every exit path -- including a panicking
    // assertion below, the actual failure mode this regression test is
    // meant to produce when the bug is present -- still kills and reaps
    // it, rather than orphaning a real, still-listening process.
    let mut child = ChildGuard(spawn_fixture(app_dir.path(), port, &app_name));
    wait_until_listening(&addr, Duration::from_secs(10));

    let initial = http_get_ping(&addr).expect("initial /ping failed");
    assert!(
        !initial.contains("pong-v2"),
        "expected the v1 fixture to answer first, got: {initial}"
    );

    // Now point `storage/releases/current` at a genuinely different
    // binary, *after* v1 is already up and running.
    std::fs::create_dir_all(app_dir.path().join("storage").join("releases")).unwrap();
    let v2_exe = env!("CARGO_BIN_EXE_stale_pointer_fixture_v2");
    std::fs::write(
        app_dir
            .path()
            .join("storage")
            .join("releases")
            .join("current"),
        v2_exe,
    )
    .unwrap();

    let response = send_restart_command(&address).expect("failed to send the restart command");
    assert_eq!(
        response,
        admin::ACK_HANDOFF_STARTED,
        "the app should have accepted and started the restart handoff"
    );

    // Poll /ping until it reflects the replacement having taken over.
    let start = Instant::now();
    let mut final_response = String::new();
    while start.elapsed() < Duration::from_secs(20) {
        if let Ok(body) = http_get_ping(&addr) {
            final_response = body;
            if final_response.contains("pong-v2") {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    assert!(
        final_response.contains("pong-v2"),
        "expected the replacement to be spawned from the *current* pointer value \
         (stale_pointer_fixture_v2), but got: {final_response:?} -- this means \
         resolve_binary_path() is still being captured once at boot instead of \
         re-read fresh when RESTART is received"
    );

    let status = child
        .0
        .wait()
        .expect("failed to wait on the original fixture process");
    assert!(
        status.success(),
        "original process should have exited cleanly after draining, got {status:?}"
    );

    // The replacement (v2) is still alive and correctly serving -- this
    // test never held a `Child` handle to it (the admin channel spawned
    // it internally), so it's cleaned up by pid, extracted from its own
    // `/ping` response, same technique `zero_downtime_restart.rs` uses.
    if let Some(pid) = final_response
        .rsplit("pid-")
        .next()
        .and_then(|s| s.parse::<u32>().ok())
    {
        kill_pid(pid);
    }
}

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
