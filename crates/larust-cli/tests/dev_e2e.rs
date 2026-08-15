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

/// One `GET /` against whatever's currently listening, returning the
/// parsed HTTP status code and the raw response body — unlike `ping`,
/// which only cares whether a request *succeeded*, this is used to prove
/// something specific is answering (the placeholder page, not the real
/// app) before any build has ever succeeded.
fn get_status_and_body(addr: &str) -> Option<(u16, String)> {
    use std::io::Write;

    let mut stream = TcpStream::connect(addr).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
    write!(
        stream,
        "GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    let status_line = response.lines().next()?;
    let status = status_line.split_whitespace().nth(1)?.parse().ok()?;
    Some((status, response))
}

fn wait_for_response(addr: &str, timeout: Duration) -> (u16, String) {
    let start = Instant::now();
    loop {
        if let Some(result) = get_status_and_body(addr) {
            return result;
        }
        assert!(
            start.elapsed() < timeout,
            "never got any response from {addr} within {timeout:?}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Unlike `wait_for_ping` (which returns the moment *anything* non-empty
/// answers `/ping`), this keeps polling until the response is actually
/// the real app's own `pong-pid-...` — required here specifically because
/// the placeholder page also answers every path, `/ping` included, with
/// its own non-empty body, so `wait_for_ping` alone can't tell "the
/// placeholder is still up" apart from "the real app has taken over."
fn wait_for_real_app(addr: &str, timeout: Duration) -> String {
    let start = Instant::now();
    loop {
        if let Some(response) = ping(addr) {
            if response.starts_with("pong-pid-") {
                return response;
            }
        }
        assert!(
            start.elapsed() < timeout,
            "the real app never took over from the placeholder at {addr} within {timeout:?}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
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

/// End-to-end proof of the placeholder described in `docs/ARCHITECTURE.md`'s
/// "the first-build placeholder": before this existed, a fresh `xr dev`
/// session whose *very first* build failed left nothing listening on the
/// port at all (`ServerState::NotStarted` never spawns anything) — a bare
/// connection-refused, indistinguishable from the whole app being broken.
/// The sibling test above only ever proves the already-covered case
/// (rebuild while a good build is already serving); this is the one that
/// exercises the actual gap.
#[test]
#[ignore = "slow: runs real `cargo build`s (the first one intentionally broken) in an \
            isolated target dir -- `cargo test -p larust-cli --test dev_e2e -- --ignored --nocapture`"]
fn xr_dev_serves_a_placeholder_page_when_the_first_build_fails() {
    let port = reserve_port();
    let app_name = format!("dev_e2e_placeholder_{port}");
    let admin_address = admin::channel_address(&app_name);
    let addr = format!("127.0.0.1:{port}");

    let app_dir = tempfile::tempdir().unwrap();
    copy_fixture(app_dir.path());
    write_config(app_dir.path(), &app_name);

    // Break the very first build on purpose — a real syntax error, so
    // `cargo build` fails deterministically rather than relying on
    // something more subtle. Kept for later: restored once the
    // placeholder is confirmed reachable, to prove the real app still
    // takes over normally afterward.
    let main_rs = app_dir.path().join("src/main.rs");
    let valid_contents = std::fs::read_to_string(&main_rs).unwrap();
    let mut broken_contents = valid_contents.clone();
    broken_contents.push_str("\nthis is not valid rust;\n");
    std::fs::write(&main_rs, &broken_contents).unwrap();

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

    // The placeholder binds synchronously before `xr dev` ever invokes
    // `cargo build`, so it should answer almost immediately — regardless
    // of how long that (doomed) first build itself takes in the
    // background compiling dependencies before it ever reaches the
    // syntax error in `main.rs`.
    let (status, body) = wait_for_response(&addr, Duration::from_secs(60));
    assert_eq!(
        status, 503,
        "the placeholder should answer 503 while there's no real build yet: {body}"
    );
    assert!(
        body.contains("xr dev"),
        "placeholder body should mention xr dev: {body}"
    );

    // Fix the error — the same real, watched, compilable source change
    // the sibling test exercises, just landing on a session that's never
    // had a successful build at all yet.
    std::fs::write(&main_rs, &valid_contents).unwrap();

    // The first real build still has to compile every dependency from
    // scratch in this fresh target dir — same generous budget the sibling
    // test's own first build uses. Polls specifically for the real app's
    // own response, not just any response — the placeholder keeps
    // answering right up until the handoff actually completes.
    let response = wait_for_real_app(&addr, Duration::from_secs(300));
    assert!(
        response.starts_with("pong-pid-"),
        "the real app should be serving now: {response}"
    );

    drop(guard);
}
