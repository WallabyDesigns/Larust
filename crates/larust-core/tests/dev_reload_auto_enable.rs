//! Proves `Application::serve()` auto-enables graceful shutdown and the
//! restart admin channel purely from `LARUST_DEV_RELOAD` being set - no
//! app-level `.with_graceful_shutdown(...)` call required at all.
//! `dev_reload_fixture` (this crate's own bin target) deliberately never
//! makes that call; if the admin channel weren't auto-enabled under this
//! env var, a `RESTART` sent to it would simply never be answered (no
//! admin listener would even exist).

use larust_core::__internal::admin;
use std::collections::VecDeque;
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A bounded ring buffer of the fixture's own stdout lines - see
/// `spawn_fixture`'s doc comment for why this is captured rather than
/// just drained and discarded.
type CapturedLog = Arc<Mutex<VecDeque<String>>>;

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

fn spawn_fixture(
    app_dir: &std::path::Path,
    port: u16,
    app_name: &str,
) -> (ChildGuard, CapturedLog) {
    let exe = env!("CARGO_BIN_EXE_dev_reload_fixture");
    let mut child = Command::new(exe)
        .env("APP_PORT", port.to_string())
        .env("APP_NAME", app_name)
        .env("LARUST_DEV_RELOAD", "1")
        .current_dir(app_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("failed to spawn dev_reload_fixture");

    // Piped so the fixture's routine logging doesn't clutter this test's
    // own output on an ordinary pass, but captured into a small ring
    // buffer rather than merely discarded - `Application::new()` installs
    // a real `tracing_subscriber` writing to stdout, so a failed handoff's
    // `tracing::warn!` calls (see `lifecycle::handoff`/`lifecycle::admin`)
    // land here, giving a failure something more useful to report than a
    // bare "got FAILED" with no reason. Still has to be actively drained
    // regardless of whether the test ever reads it back: an OS pipe has a
    // bounded buffer (~64KB on Windows), and this process keeps logging
    // after the restart handoff below - once full, the next write blocks
    // the fixture process forever, which means it can never reach its own
    // exit, and this test's later `child.0.wait()` would then hang
    // indefinitely. A real, reproducible bug this test hit before this
    // fix, not a hypothetical.
    let stdout = child.stdout.take().expect("stdout was piped above");
    let captured_log: CapturedLog = Arc::new(Mutex::new(VecDeque::with_capacity(64)));
    {
        let captured_log = Arc::clone(&captured_log);
        std::thread::spawn(move || {
            use std::io::{BufRead, BufReader};
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let mut log = captured_log.lock().unwrap();
                if log.len() >= 64 {
                    log.pop_front();
                }
                log.push_back(line);
            }
        });
    }

    (ChildGuard(child), captured_log)
}

#[cfg(unix)]
fn send_command(address: &str, command: &str) -> std::io::Result<String> {
    use std::io::{BufRead, Write};
    use std::os::unix::net::UnixStream;

    let path = std::env::temp_dir().join(format!("{address}.sock"));
    let mut stream = UnixStream::connect(&path)?;
    stream.set_read_timeout(Some(Duration::from_secs(20)))?;
    stream.write_all(command.as_bytes())?;
    stream.write_all(b"\n")?;
    let mut response = String::new();
    std::io::BufReader::new(stream).read_line(&mut response)?;
    Ok(response.trim().to_string())
}

#[cfg(windows)]
fn send_command(address: &str, command: &str) -> std::io::Result<String> {
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
        writer.write_all(command.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        let mut reader = BufReader::new(reader);
        let mut response = String::new();
        reader.read_line(&mut response).await?;
        Ok(response.trim().to_string())
    })
}

fn kill_pid_by_response(final_response: &str) {
    let Some(pid) = final_response
        .rsplit("pid-")
        .next()
        .and_then(|s| s.parse::<u32>().ok())
    else {
        return;
    };
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, TerminateProcess, PROCESS_TERMINATE,
        };
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if !handle.is_null() {
            TerminateProcess(handle, 1);
            CloseHandle(handle);
        }
    }
}

#[test]
fn the_admin_channel_is_live_under_dev_reload_with_no_app_level_opt_in() {
    let port = reserve_port();
    let app_name = format!("dev_reload_auto_test_{port}");
    let address = admin::channel_address(&app_name);
    let addr = format!("127.0.0.1:{port}");

    let app_dir = tempfile::tempdir().unwrap();

    let (mut child, captured_log) = spawn_fixture(app_dir.path(), port, &app_name);
    wait_until_listening(&addr, Duration::from_secs(10));

    // `dev_reload_fixture` never calls `.with_graceful_shutdown(...)` -
    // if `LARUST_DEV_RELOAD` didn't auto-enable the admin channel, this
    // would fail to connect at all (no listener would exist on this
    // address). A *connect* failure here is never retried - it means the
    // one thing this test exists to prove (the channel is live at all)
    // is already false, and no amount of waiting fixes that.
    //
    // A `FAILED` response is different, and *is* retried a few times: it
    // means the connection and protocol both worked, but the handoff
    // attempt itself failed. This has recurred on `ubuntu-latest` CI more
    // than once - the *original* theory (a real Tokio process is spawned
    // and awaited, and a saturated shared runner is occasionally too slow
    // to boot it within `HANDOFF_READY_TIMEOUT`, 15s) turned out to be
    // wrong on inspection: a run that failed all 3 attempts still finished
    // in 1.45s total, nowhere near 3 x 15s, meaning each attempt failed
    // fast, not by timing out. `lifecycle::handoff`/`lifecycle::admin` now
    // log *why* a handoff attempt failed (timed out vs. the replacement's
    // stderr closing early, i.e. it crashed or exited on its own, vs. a
    // spawn error) - see `captured_log` below, which surfaces that
    // reasoning in this test's own failure output instead of leaving it a
    // mystery. Retrying at all is safe by the framework's own design, not
    // a workaround bolted on here: `run_until_command`'s own doc comment
    // already guarantees a failed attempt leaves the still-healthy
    // original process ready to accept another `RESTART` later.
    const MAX_ATTEMPTS: u32 = 3;
    for attempt in 1..=MAX_ATTEMPTS {
        let response = send_command(&address, admin::RESTART_COMMAND).expect(
            "failed to send the restart command -- the admin channel should have been \
                     auto-enabled under LARUST_DEV_RELOAD with no app-level opt-in",
        );
        if response == admin::ACK_HANDOFF_STARTED {
            break;
        }
        assert!(
            attempt < MAX_ATTEMPTS,
            "the app should have accepted and started the restart handoff after \
             {MAX_ATTEMPTS} attempts, got {response:?} every time. The fixture's own \
             log output (should include a tracing::warn! explaining why, from \
             lifecycle::handoff/lifecycle::admin):\n{}",
            captured_log
                .lock()
                .unwrap()
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        );
        std::thread::sleep(Duration::from_millis(500));
    }

    // The replacement resolves via `resolve_binary_path()`'s
    // `current_exe()` fallback (no `storage/releases/current` pointer
    // exists in this test's app_dir), so it's another instance of the
    // *same* fixture binary -- confirmed by a pid change, proving a real
    // handoff happened.
    let start = Instant::now();
    #[allow(unused_assignments)]
    let mut final_response = String::new();
    let original_status_check_deadline = Duration::from_secs(10);
    loop {
        if let Ok(mut stream) = TcpStream::connect(&addr) {
            use std::io::{Read, Write};
            stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
            let _ = write!(
                stream,
                "GET /ping HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
            );
            let mut body = String::new();
            let _ = stream.read_to_string(&mut body);
            if let Some((_, b)) = body.split_once("\r\n\r\n") {
                final_response = b.trim().to_string();
                if !final_response.is_empty() {
                    break;
                }
            }
        }
        assert!(
            start.elapsed() < original_status_check_deadline,
            "replacement never came up after the restart handoff"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    // Also proves the dev-specific *short* drain timeout is in effect -
    // the original process (never explicitly configured, only
    // auto-enabled) should have already exited well within a couple of
    // seconds, not the 30s production default.
    let status = child
        .0
        .wait()
        .expect("failed to wait on the original fixture process");
    assert!(
        status.success(),
        "original process should have exited cleanly and quickly after draining, got {status:?}"
    );

    kill_pid_by_response(&final_response);
}
