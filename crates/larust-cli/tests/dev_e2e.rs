//! End-to-end proof that `xr dev`'s rebuild-on-save loop is genuinely
//! zero-downtime: a real `cargo build`, a real spawn, continuous real HTTP
//! traffic driven through the whole thing, a real file touch triggering a
//! real rebuild, and an assertion of zero failed requests plus a pid
//! change across the reload. This is the test that actually substantiates
//! "zero-downtime `xr dev`" as a real claim rather than just an
//! architecture — everything else in this session's own admin-channel
//! test suite (`larust-core`'s `stale_binary_path.rs`, `admin_stop.rs`,
//! `dev_reload_auto_enable.rs`) only proves the underlying primitives
//! work in isolation, not that `xr dev`'s own build loop actually wires
//! them together correctly.
//!
//! Genuinely slow: two full `cargo build`s against a freshly bootstrapped,
//! isolated target dir (the first compiles every one of `larust-core`'s
//! own dependencies from scratch). Marked `#[ignore]` — run explicitly:
//! `cargo test -p larust-cli --test dev_e2e -- --ignored --nocapture`.

use larust_core::__internal::admin;
use std::io::Read;
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn reserve_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn fixture_source_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dev_app")
}

/// Copies the checked-in fixture template into a fresh tempdir, rewriting
/// its `larust-core` path dependency from the relative path that's valid
/// when building the template in place (`../../../../larust-core`) to an
/// absolute one — the tempdir this test builds from lives far outside
/// this repo's own directory tree, where that relative path no longer
/// resolves to anything.
fn copy_fixture(dest: &Path) {
    std::fs::create_dir_all(dest.join("src")).unwrap();
    std::fs::copy(
        fixture_source_dir().join("src/main.rs"),
        dest.join("src/main.rs"),
    )
    .unwrap();

    let cargo_toml = std::fs::read_to_string(fixture_source_dir().join("Cargo.toml")).unwrap();
    // Deliberately not `.canonicalize()`d: on Windows that produces a
    // `\\?\`-prefixed verbatim path, which Cargo's manifest parser
    // rejects outright ("invalid path url") — confirmed empirically, not
    // assumed. `CARGO_MANIFEST_DIR` is already absolute, so a plain
    // `join` is both sufficient and portable.
    let larust_core_abs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../larust-core");
    let rewritten = cargo_toml.replace(
        "../../../../larust-core",
        &larust_core_abs.to_string_lossy().replace('\\', "/"),
    );
    std::fs::write(dest.join("Cargo.toml"), rewritten).unwrap();
}

/// A fresh, random `app_name` per test run — not the fixture template's
/// own checked-in default — so repeated or parallel runs never collide
/// on the same OS-global admin-channel address (a named pipe name on
/// Windows, a path under the shared temp dir on Unix).
fn write_config(dest: &Path, app_name: &str) {
    std::fs::create_dir_all(dest.join("config")).unwrap();
    std::fs::write(
        dest.join("config/app.toml"),
        format!("app_name = \"{app_name}\"\n"),
    )
    .unwrap();
}

/// One `GET /ping`, or `None` on any failure — nothing listening yet, a
/// connection reset mid-handoff, a read timeout. The caller treats every
/// `None` as a request failure; that's the entire zero-downtime claim
/// this test exists to check.
fn ping(addr: &str) -> Option<String> {
    use std::io::Write;

    let mut stream = TcpStream::connect(addr).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
    write!(
        stream,
        "GET /ping HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut body = String::new();
    stream.read_to_string(&mut body).ok()?;
    let (_, b) = body.split_once("\r\n\r\n")?;
    let b = b.trim();
    (!b.is_empty()).then(|| b.to_string())
}

fn wait_for_ping(addr: &str, timeout: Duration) -> String {
    let start = Instant::now();
    loop {
        if let Some(body) = ping(addr) {
            return body;
        }
        assert!(
            start.elapsed() < timeout,
            "never got a response from {addr} within {timeout:?}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn extract_pid(response: &str) -> &str {
    response.rsplit("pid-").next().unwrap()
}

#[cfg(unix)]
fn send_admin_command(address: &str, command: &str) -> std::io::Result<String> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let path = std::env::temp_dir().join(format!("{address}.sock"));
    let mut stream = UnixStream::connect(&path)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.write_all(command.as_bytes())?;
    stream.write_all(b"\n")?;
    let mut response = String::new();
    BufReader::new(stream).read_line(&mut response)?;
    Ok(response.trim().to_string())
}

#[cfg(windows)]
fn send_admin_command(address: &str, command: &str) -> std::io::Result<String> {
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

/// Guarantees the real server process this test drives is asked to stop
/// gracefully (via the same admin channel `xr restart`/`xr dev` itself
/// speak) and that the `xr dev` supervisor process is torn down, on every
/// exit path — including a panicked assertion. Without this, a panic
/// partway through would leave a real, still-listening server process
/// (and the `xr` process supervising it) running indefinitely: the same
/// orphaned-process hazard this session already hit once with
/// `Child`-spawning tests that didn't guard against panics (see
/// `docs/GOTCHAS.md`).
struct DevGuard {
    child: Child,
    admin_address: String,
}

impl Drop for DevGuard {
    fn drop(&mut self) {
        let _ = send_admin_command(&self.admin_address, admin::STOP_COMMAND);
        std::thread::sleep(Duration::from_millis(500));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
#[ignore = "slow: runs two real `cargo build`s in an isolated target dir -- \
            `cargo test -p larust-cli --test dev_e2e -- --ignored --nocapture`"]
fn xr_dev_reloads_with_zero_failed_requests_and_a_new_pid() {
    let port = reserve_port();
    let app_name = format!("dev_e2e_fixture_{port}");
    let admin_address = admin::channel_address(&app_name);
    let addr = format!("127.0.0.1:{port}");

    let app_dir = tempfile::tempdir().unwrap();
    copy_fixture(app_dir.path());
    write_config(app_dir.path(), &app_name);

    let xr = env!("CARGO_BIN_EXE_xr");
    let child = Command::new(xr)
        .arg("dev")
        .current_dir(app_dir.path())
        .env("APP_PORT", port.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("failed to spawn `xr dev`");

    let guard = DevGuard {
        child,
        admin_address,
    };

    // The first build compiles every one of `larust-core`'s own
    // dependencies from scratch in a brand-new target dir — generously
    // bounded, not tuned tight.
    let initial_response = wait_for_ping(&addr, Duration::from_secs(300));
    let initial_pid = extract_pid(&initial_response).to_string();

    // Continuous real traffic for the whole rest of the test — this is
    // what actually substantiates "zero downtime" rather than just "it
    // eventually comes back up".
    let failures = Arc::new(AtomicU32::new(0));
    let requests = Arc::new(AtomicU32::new(0));
    let stop_traffic = Arc::new(AtomicBool::new(false));
    let traffic_handle = {
        let failures = Arc::clone(&failures);
        let requests = Arc::clone(&requests);
        let stop_traffic = Arc::clone(&stop_traffic);
        let addr = addr.clone();
        std::thread::spawn(move || {
            while !stop_traffic.load(Ordering::Relaxed) {
                requests.fetch_add(1, Ordering::Relaxed);
                if ping(&addr).is_none() {
                    failures.fetch_add(1, Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        })
    };

    // A trailing comment is a real, watched, compilable source change —
    // exactly what a developer's own save triggers.
    let main_rs = app_dir.path().join("src/main.rs");
    let mut contents = std::fs::read_to_string(&main_rs).unwrap();
    contents.push_str("\n// touched by dev_e2e\n");
    std::fs::write(&main_rs, contents).unwrap();

    // Incremental this time (dependencies already built) — still
    // generously bounded.
    let deadline = Instant::now() + Duration::from_secs(180);
    let new_pid = loop {
        if let Some(response) = ping(&addr) {
            let pid = extract_pid(&response).to_string();
            if pid != initial_pid {
                break pid;
            }
        }
        assert!(
            Instant::now() < deadline,
            "the rebuilt server never came up with a new pid within the deadline"
        );
        std::thread::sleep(Duration::from_millis(100));
    };

    stop_traffic.store(true, Ordering::Relaxed);
    traffic_handle.join().unwrap();

    assert_ne!(
        new_pid, initial_pid,
        "the reload should have switched to a new process"
    );
    assert!(
        requests.load(Ordering::Relaxed) > 0,
        "the traffic thread should have made at least one request"
    );
    assert_eq!(
        failures.load(Ordering::Relaxed),
        0,
        "every one of the {} requests driven during the rebuild should have succeeded -- \
         this is the actual zero-downtime claim",
        requests.load(Ordering::Relaxed)
    );

    drop(guard);
}
