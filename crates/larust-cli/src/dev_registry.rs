//! Tracks every currently-running `xr dev` supervisor process on this
//! machine - what `xr list`/`xr kill` need that nothing else here already
//! provides.
//!
//! The *served app* half of "stop what's running for this project" needed
//! no new mechanism at all: `larust_core::__internal::admin`'s existing
//! channel (see that module's own doc comment) already reaches "whoever is
//! currently serving this app" by address, regardless of whether it was
//! started by `xr dev`'s own handoff, `xr deploy --run`'s cold start, or a
//! bare `cargo run` - `xr kill` just sends it the same `STOP` command `xr
//! dev`'s own Ctrl+C handler already sends on the `HandedOff` path (see
//! `dev.rs::register_ctrlc_handler`).
//!
//! `xr dev`'s own long-lived supervisor process is the one genuinely new
//! gap: it never itself listens on an admin channel (it only ever *speaks*
//! one, to whatever it's watching over), so there was previously no way to
//! reach it - or even discover it exists - from a second `xr` invocation.
//! This module closes that gap with the simplest mechanism that works: one
//! small JSON file per running session, in a well-known per-user directory,
//! written at startup and best-effort removed on a clean exit. A crashed or
//! forcibly-killed session leaves a stale file behind - harmless, since
//! every read here (`list_live`, `find_by_pid`, `find_by_dir`) verifies the
//! PID is still actually alive first and quietly deletes the entry if not,
//! the same self-healing a stale lockfile gets from whoever next reads it.
//!
//! Deliberately files, not a database or a long-lived daemon: `xr dev`
//! sessions are inherently few (a handful of terminals at most) and
//! short-lived relative to a real service, so there's no scale or
//! concurrency problem a directory of small JSON files can't handle, and it
//! needs no server of its own to stay available between unrelated `xr`
//! invocations.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub pid: u32,
    pub app_name: String,
    pub project_dir: PathBuf,
    pub port: u16,
    pub admin_address: String,
    pub started_at_unix: u64,
}

/// Per-user, machine-wide directory every `xr dev` session's own registry
/// file lives in - deliberately not inside any one project's own directory
/// (a session needs to be discoverable from `xr list`/`xr kill --id`
/// regardless of which directory that later call runs from).
fn registry_dir() -> PathBuf {
    #[cfg(windows)]
    {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        base.join("larust").join("dev-sessions")
    }
    #[cfg(not(windows))]
    {
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
            })
            .unwrap_or_else(std::env::temp_dir);
        base.join("larust").join("dev-sessions")
    }
}

fn session_path(pid: u32) -> PathBuf {
    registry_dir().join(format!("{pid}.json"))
}

/// Called once, early in `xr dev`'s `run()` - see `dev.rs`'s own call site
/// for exactly where and why (after the placeholder is bound, so `port` is
/// the real bound port rather than the possibly-already-taken one it
/// started from).
pub fn register(session: &Session) -> Result<()> {
    let dir = registry_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let path = session_path(session.pid);
    let json = serde_json::to_vec_pretty(session).context("failed to serialize session")?;
    std::fs::write(&path, json).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

/// Best-effort - called both from `xr dev`'s own clean-exit path (Ctrl+C)
/// and from `xr kill` after forcibly terminating a session it found. Never
/// fails the caller: a file that's already gone (the other of those two
/// callers got there first) is exactly as good as one this call removed
/// itself.
pub fn unregister(pid: u32) {
    let _ = std::fs::remove_file(session_path(pid));
}

/// Every session whose registry file still exists *and* whose PID is still
/// actually alive - a dead PID's stale file is deleted on the way past,
/// not just skipped, so it doesn't keep costing every future `xr list`/`xr
/// kill` a wasted liveness check forever.
pub fn list_live() -> Vec<Session> {
    let dir = registry_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut sessions: Vec<Session> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                return None;
            }
            let contents = std::fs::read(&path).ok()?;
            let session: Session = serde_json::from_slice(&contents).ok()?;
            if is_alive(session.pid) {
                Some(session)
            } else {
                let _ = std::fs::remove_file(&path);
                None
            }
        })
        .collect();
    sessions.sort_by_key(|s| s.started_at_unix);
    sessions
}

pub fn find_by_pid(pid: u32) -> Option<Session> {
    list_live().into_iter().find(|s| s.pid == pid)
}

/// Matches on the canonicalized directory so `xr kill` finds the right
/// session regardless of trailing slashes, `.`/`..` segments, or symlinks -
/// the same normalization gap `Path::eq` alone wouldn't close. `dir` failing
/// to canonicalize (it no longer exists) simply matches nothing, which is
/// the correct outcome - there's no session for a directory that isn't
/// there anymore.
pub fn find_by_dir(dir: &Path) -> Option<Session> {
    let target = dir.canonicalize().ok()?;
    list_live()
        .into_iter()
        .find(|s| s.project_dir.canonicalize().ok().as_deref() == Some(target.as_path()))
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(windows)]
fn is_alive(pid: u32) -> bool {
    let Ok(output) = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
    else {
        // Can't even run `tasklist` - assume alive rather than risk
        // deleting a live session's registry entry over a transient local
        // tooling failure.
        return true;
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stdout = stdout.trim();
    !stdout.is_empty() && !stdout.starts_with("INFO:")
}

#[cfg(unix)]
fn is_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|status| status.success())
        .unwrap_or(true)
}

/// Forcibly ends the session's own OS process. Not graceful by choice, not
/// by omission: `xr dev`'s supervisor never listens on an admin channel of
/// its own (see this module's own doc comment), and `larust_core::
/// lifecycle::admin::STOP_COMMAND`'s own doc comment already establishes
/// why a *targeted* graceful OS signal isn't available on Windows either
/// (`GenerateConsoleCtrlEvent` can't address one specific process) - so
/// there is no gentler option to reach for here. The served app it was
/// watching over, if any, was already asked to stop gracefully via the
/// admin channel *before* this is ever called - see `kill.rs`'s own
/// ordering.
pub fn terminate(pid: u32) -> Result<()> {
    #[cfg(windows)]
    {
        std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .output()
            .context("failed to run taskkill")?;
    }
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .context("failed to run kill")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_session(pid: u32) -> Session {
        Session {
            pid,
            app_name: "testapp".to_string(),
            project_dir: PathBuf::from("/tmp/testapp"),
            port: 34187,
            admin_address: "larust-testapp".to_string(),
            started_at_unix: 1_700_000_000,
        }
    }

    #[test]
    fn a_pid_this_process_cannot_possibly_be_reports_as_not_alive() {
        // PIDs are 32-bit on Windows and typically much smaller in practice
        // on Unix - this value is astronomically unlikely to ever be a real
        // running process on any platform this runs on.
        assert!(!is_alive(u32::MAX - 1));
    }

    #[test]
    fn this_process_own_pid_reports_as_alive() {
        assert!(is_alive(std::process::id()));
    }

    // A single test, not one each for `find_by_pid`/`find_by_dir`: both go
    // through `list_live()`, which only ever returns a session whose PID is
    // genuinely alive right now - the only PID a test can honestly claim
    // that about is its own (`std::process::id()`), and `cargo test` runs
    // every `#[test]` fn in this file in the same process, so two tests
    // both registering under that identical, shared PID would race each
    // other's `register`/`unregister` calls. One test, one register, one
    // unregister sidesteps that entirely - the exact same "shared mutable
    // resource -> one test, not several" reasoning `mail_test.rs` already
    // documents for its own process-wide fake-mail recorder.
    #[test]
    fn register_then_find_by_pid_and_by_dir_round_trip() {
        let pid = std::process::id();
        let tmp = tempfile::tempdir().unwrap();
        let mut session = sample_session(pid);
        session.project_dir = tmp.path().to_path_buf();
        register(&session).unwrap();

        let by_pid = find_by_pid(pid).expect("just-registered session should be found by pid");
        assert_eq!(by_pid.app_name, "testapp");
        assert_eq!(by_pid.port, 34187);

        let by_dir = find_by_dir(tmp.path()).expect("should also be found by its directory");
        assert_eq!(by_dir.pid, pid);

        unregister(pid);
        assert!(find_by_pid(pid).is_none());
    }

    #[test]
    fn list_live_prunes_a_stale_entry_for_a_dead_pid() {
        let dead_pid = u32::MAX - 2;
        register(&sample_session(dead_pid)).unwrap();
        assert!(session_path(dead_pid).exists());

        let live = list_live();
        assert!(!live.iter().any(|s| s.pid == dead_pid));
        assert!(
            !session_path(dead_pid).exists(),
            "a dead session's stale registry file should be deleted, not just skipped"
        );
    }
}
