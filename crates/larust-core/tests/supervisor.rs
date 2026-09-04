//! End-to-end proof of `lifecycle::supervisor`: a genuinely orphan-proof
//! spawned replacement, not just "the graceful paths already worked".
//!
//! Needs a three-process shape, not two - the property under test is "the
//! OS kills the child when the *parent* dies with zero chance to run any
//! cleanup code", and this test process is itself the top-level process,
//! so it can't hard-kill itself. Instead: this test spawns
//! `supervisor_parent_fixture` (simulating `xr dev`), which spawns
//! `zero_downtime_fixture` as a real handoff replacement (simulating a
//! `dev-N.exe` generation) via the actual `handoff::
//! spawn_replacement_and_wait_for_ready` - exercising
//! `lifecycle::supervisor::prepare`/`register` exactly as production
//! does. The test then hard-kills the *parent* fixture and checks whether
//! the grandchild died too.
//!
//! Windows-only: `lifecycle::supervisor`'s Linux backend can only be
//! cross-compile-checked from this Windows-only test suite, not run - see
//! `docs/GOTCHAS.md` for the existing "cargo check alone doesn't prove
//! Unix-only code actually works" caveat, which applies here too. A real
//! run against the Linux backend needs a Linux machine/CI, not something
//! this file can claim to prove.
//!
//! **App-name isolation matters here more than in most fixture-based
//! tests**: `zero_downtime_fixture` (the replacement `supervisor_parent_fixture`
//! spawns) sets `restart_channel: true`, opening a real admin-channel
//! listener. With no config of its own, it would fall back to
//! `Config`'s default `app_name` ("Larust") - the exact same name a
//! locally-running `demo` app uses (`demo/.env`'s own `APP_NAME="Larust"`),
//! which means the two would compute the identical admin-channel address
//! and could cross-talk with a real `xr dev demo` session running on the
//! same machine at the same time. `zero_downtime_restart.rs` already
//! established the fix for this exact risk (a unique, per-test-run
//! `app_name` set as a real `APP_NAME` env var on the spawned fixture,
//! which reads it directly - see that fixture's own `config()` function)
//! - mirrored here, one process level removed since the replacement is
//! spawned *by* `supervisor_parent_fixture`, not directly by this test:
//! `Command::spawn` inherits the parent's environment by default (and
//! `handoff::spawn_replacement_and_wait_for_ready` never clears it), so
//! setting `APP_NAME` on *this* test's own spawn of `supervisor_parent_fixture`
//! is enough for it to propagate two process levels down to the
//! replacement, with no explicit passing needed at either hop.

#![cfg(windows)]

use std::net::TcpStream;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

/// Polls `port` until a plain TCP connect either succeeds or the deadline
/// passes - used both to wait for the replacement to come up and, later,
/// to wait for it to go away once the parent is killed.
fn wait_for(port: u16, want_connectable: bool, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let connected = TcpStream::connect(("127.0.0.1", port)).is_ok();
        if connected == want_connectable {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[tokio::test]
async fn a_hard_killed_parents_replacement_is_also_killed_by_the_os() {
    let parent_exe = env!("CARGO_BIN_EXE_supervisor_parent_fixture");
    let replacement_exe = env!("CARGO_BIN_EXE_zero_downtime_fixture");

    // See this file's own module doc comment for why this matters: without
    // it, the replacement `zero_downtime_fixture` spawns would default to
    // the same `app_name` ("Larust") a real local `demo` app uses, and its
    // `restart_channel: true` admin listener could cross-talk with one.
    let app_name = format!("supervisor_test_{}", std::process::id());

    let mut parent = Command::new(parent_exe)
        .arg(replacement_exe)
        .env("APP_NAME", &app_name)
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn supervisor_parent_fixture");

    // Stderr, not stdout - see `supervisor_parent_fixture.rs`'s own doc
    // comment for why: the replacement's stdout is inherited all the way
    // through to this process's own stdout, so reading stdout here would
    // race against that other process's own log lines landing in the same
    // stream.
    let stderr = parent.stderr.take().expect("stderr was piped");
    let mut lines = BufReader::new(stderr).lines();
    let ready_line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
        .await
        .expect("timed out waiting for the parent fixture's READY line")
        .expect("reading the parent fixture's stderr failed")
        .expect("parent fixture should print a READY line before blocking forever");

    let port: u16 = ready_line
        .split("port=")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("couldn't parse port from: {ready_line}"));

    assert!(
        wait_for(port, true, Duration::from_secs(5)),
        "the replacement should be reachable before the parent is ever touched"
    );

    // The hard kill: `TerminateProcess` on Windows, no chance for
    // `supervisor_parent_fixture` to run any code at all - genuinely
    // simulating a crashed or force-killed `xr dev`, not a graceful
    // Ctrl+C (which already worked before this feature existed).
    parent.kill().await.expect("failed to kill parent fixture");
    let _ = parent.wait().await;

    assert!(
        wait_for(port, false, Duration::from_secs(10)),
        "the replacement should have been killed by the OS once its supervising parent was \
         hard-killed, but it's still accepting connections on port {port}"
    );
}
