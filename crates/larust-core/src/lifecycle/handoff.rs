//! Parent-side orchestration for a restart handoff: spawn a replacement
//! process, hand it the listener via `lifecycle::listener`, and wait
//! (bounded) for it to report it's actually serving before this process
//! begins its own graceful shutdown. See `readiness.rs` for the child
//! side of the same protocol.

use crate::lifecycle::{listener, supervisor};
use std::io;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

/// Where a deploy step writes the path of the release that should be
/// spawned on the next restart handoff — a plain text file (not a
/// symlink: Windows symlinks need elevated privilege or Developer Mode
/// enabled, which can't be assumed), relative to the app's own root, the
/// same convention `public/`/`config/app.toml` already use elsewhere in
/// this crate.
pub const RELEASE_POINTER_PATH: &str = "storage/releases/current";

/// Resolves which binary a restart handoff should spawn as the
/// replacement. `storage/releases/current`, if present, wins — the real
/// production deploy story: new builds land at a fresh, versioned path
/// (`storage/releases/<version-or-hash>/<name>`), and a deploy step
/// updates this pointer to the new one, atomically and auditably, with a
/// trivial rollback (just point it back). Falls back to `std::env::
/// current_exe()` — re-executing this exact running binary — when no
/// pointer file exists at all, which is only really meaningful for local
/// dev/testing (re-execing a binary the current process still holds open
/// fails outright on Windows, the same constraint `xr dev` already works
/// around by killing before rebuilding — see `docs/GOTCHAS.md`; the
/// pointer file is what a real deploy is expected to always provide
/// instead of relying on this fallback).
pub fn resolve_binary_path() -> io::Result<PathBuf> {
    resolve_binary_path_from(RELEASE_POINTER_PATH)
}

/// The testable half of `resolve_binary_path` — split out so a unit test
/// can point it at a real temp file instead of needing to change this
/// whole process's current directory (unsafe to do in a suite that runs
/// tests concurrently) just to exercise the "pointer file present" case.
fn resolve_binary_path_from(pointer_path: &str) -> io::Result<PathBuf> {
    if let Ok(contents) = std::fs::read_to_string(pointer_path) {
        let path = contents.trim();
        if !path.is_empty() {
            return Ok(PathBuf::from(path));
        }
    }
    std::env::current_exe()
}

/// The exact line `readiness::announce_ready` writes to a replacement's
/// stdout once it's genuinely about to start serving. Distinct and
/// greppable; anything else the child's own stdout produces first
/// (`tracing_subscriber`'s default formatter also writes to stdout, so
/// the app's own "starting server" log line is expected to precede this)
/// is simply skipped while scanning for this exact line, never treated
/// as a failure on its own.
pub const READY_MARKER: &str = "__LARUST_HANDOFF_READY__";

/// Spawns `binary_path` as a replacement for the process currently
/// serving on `listener`, hands the listener off to it, and waits up to
/// `ready_timeout` for it to report readiness.
///
/// - `Ok(Some(child))` — the replacement is confirmed live and serving;
///   the caller should now begin its own graceful shutdown.
/// - `Ok(None)` — the replacement crashed, exited, or never reported
///   ready within the timeout; any process that *was* spawned has
///   already been killed here, so the caller can simply keep serving as
///   if the handoff attempt never happened.
/// - `Err(_)` — couldn't even attempt the handoff (e.g. spawning
///   `binary_path` itself failed) — also always safe to just keep
///   serving.
///
/// Never partially leaves a live orphan behind: every path that doesn't
/// return `Ok(Some(child))` has already killed and reaped whatever was
/// spawned.
///
/// Stdout is deliberately `Stdio::inherit()`, not piped: only stderr
/// carries the one-line readiness handshake (see `readiness::
/// announce_ready`'s own doc comment for why routing routine logging
/// through a pipe that gets dropped once ready would otherwise break
/// every log line the replacement emits afterward). Once ready, the
/// stderr reader is kept alive and draining in the background — not just
/// dropped — for the same reason, one level up: a `tracing::warn!`/
/// `error!` call later in the replacement's life would otherwise hit the
/// exact same closed-pipe problem stdout would have.
pub async fn spawn_replacement_and_wait_for_ready(
    listener: &TcpListener,
    binary_path: &Path,
    ready_timeout: Duration,
) -> io::Result<Option<Child>> {
    let mut command = Command::new(binary_path);
    command
        .env(listener::INHERIT_LISTENER_ENV, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::piped());
    // Guarantees the OS kills this replacement if *this* process dies for
    // any reason (crash, force-kill, closed terminal) — not just the
    // graceful paths (Ctrl+C, `STOP`) already handled elsewhere. See
    // `lifecycle::supervisor`'s own doc comment.
    supervisor::prepare(&mut command);

    let mut child = command.spawn()?;
    supervisor::register(&child);
    let child_pid = child
        .id()
        .ok_or_else(|| io::Error::other("spawned replacement has no pid"))?;

    let encoded = listener::prepare_for_handoff(listener, child_pid)?;
    {
        let mut stdin = child.stdin.take().expect("stdin was piped above");
        stdin.write_all(encoded.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
    }

    let stderr = child.stderr.take().expect("stderr was piped above");
    let mut lines = BufReader::new(stderr).lines();

    let became_ready = tokio::time::timeout(ready_timeout, async {
        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim() == READY_MARKER {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);

    if became_ready {
        // Keep draining rather than dropping `lines` here — an unread
        // stderr pipe would break the replacement's own error/warn
        // logging the same way an unread stdout pipe used to (see this
        // function's own doc comment).
        tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
        Ok(Some(child))
    } else {
        let _ = child.kill().await;
        let _ = child.wait().await;
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_to_the_pointer_files_contents_when_present_and_non_empty() {
        let dir = tempfile::tempdir().unwrap();
        let pointer = dir.path().join("current");
        std::fs::write(&pointer, "  /releases/abc123/my-app  \n").unwrap();

        let resolved = resolve_binary_path_from(pointer.to_str().unwrap()).unwrap();
        assert_eq!(resolved, PathBuf::from("/releases/abc123/my-app"));
    }

    #[test]
    fn falls_back_to_current_exe_when_the_pointer_file_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let pointer = dir.path().join("does-not-exist");

        let resolved = resolve_binary_path_from(pointer.to_str().unwrap()).unwrap();
        assert_eq!(resolved, std::env::current_exe().unwrap());
    }

    #[test]
    fn falls_back_to_current_exe_when_the_pointer_file_is_empty_or_blank() {
        let dir = tempfile::tempdir().unwrap();
        let pointer = dir.path().join("current");
        std::fs::write(&pointer, "   \n").unwrap();

        let resolved = resolve_binary_path_from(pointer.to_str().unwrap()).unwrap();
        assert_eq!(resolved, std::env::current_exe().unwrap());
    }
}
