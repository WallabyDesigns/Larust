//! The restart-trigger admin channel: a small, local-only listener
//! (`tokio::net::UnixListener` on Unix, a named pipe on Windows) an
//! already-running process listens on so `xr restart` (or any other local
//! caller) can ask it to perform a zero-downtime restart handoff.
//!
//! Preferred over a loopback TCP port: OS-level file/pipe permissions
//! give real access control for free (no auth-token scheme needed for a
//! first version), there's no risk of colliding with the app's own
//! `app_port` or another local service, and it doesn't show up as an open
//! network port to scan. The path/name is derived deterministically from
//! `Config::app_name` — both `Application::serve()` and `xr restart`
//! compute it identically, independently, with no runtime negotiation
//! needed to agree on where to find it.

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

use std::net::TcpListener as StdTcpListener;
use std::time::Duration;

pub const RESTART_COMMAND: &str = "RESTART";
/// Asks the running process to shut down gracefully with no replacement
/// spawned at all — for a caller that no longer holds a `Child` handle to
/// whatever's currently serving (e.g. `xr dev`, once it's handed off past
/// the first generation) and needs a reliable way to reach "whoever is
/// currently listening" for a clean teardown. OS signals don't help here
/// on Windows specifically: `signal.rs`'s own reasoning already
/// establishes that `GenerateConsoleCtrlEvent(CTRL_C_EVENT)` can't target
/// one specific process, and `CTRL_BREAK_EVENT` only works for a child
/// spawned with `CREATE_NEW_PROCESS_GROUP` — not the case for a
/// generation the caller never itself spawned. The admin channel, being
/// address-based rather than pid-based, is the one mechanism that
/// already reaches "whoever owns this address right now" regardless of
/// spawn lineage.
pub const STOP_COMMAND: &str = "STOP";
pub const ACK_HANDOFF_STARTED: &str = "OK";
pub const ACK_HANDOFF_FAILED: &str = "FAILED";

/// What the admin channel loop resolved to — either a restart handoff
/// actually succeeded (`Handoff`, carrying the new, already-serving
/// child), or a plain `STOP` was received (no replacement spawned at
/// all). The caller (`Application::serve()`) treats both the same way
/// from here on: begin its own graceful shutdown.
pub enum AdminOutcome {
    Handoff(Box<tokio::process::Child>),
    Stop,
}

/// Deterministic per-app admin-channel address — a plain identifier, not
/// a full path/pipe-name; each platform's own module turns it into the
/// concrete form it needs (a socket file path on Unix, a
/// `\\.\pipe\...` name on Windows).
pub fn channel_address(app_name: &str) -> String {
    let safe: String = app_name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("larust-{safe}")
}

/// Runs the admin channel loop until either a restart handoff actually
/// succeeds or a plain `STOP` is received — either way, the caller
/// (`Application::serve()`) treats the result as "begin my own graceful
/// shutdown now" (see `AdminOutcome`). A *failed* handoff attempt (the
/// spawned replacement crashed, or never reported ready within
/// `ready_timeout`) is reported back to whoever asked, and this function
/// keeps looping, ready to accept another attempt later — a bad build
/// doesn't wedge the currently-running, still-healthy process out of
/// ever restarting again.
///
/// Deliberately does **not** take a `binary_path` parameter — each
/// platform's own implementation resolves `handoff::resolve_binary_path()`
/// fresh, at the moment a `RESTART` is actually received, not once up
/// front. A `storage/releases/current` pointer written *after* this
/// process booted but *before* a `RESTART` arrives must be respected;
/// resolving once at boot (the original, buggy shape of this function)
/// silently ignored any such update. See `docs/GOTCHAS.md`.
pub async fn run_until_command(
    address: &str,
    listener: &StdTcpListener,
    ready_timeout: Duration,
) -> AdminOutcome {
    #[cfg(unix)]
    {
        unix::run_until_command(address, listener, ready_timeout).await
    }
    #[cfg(windows)]
    {
        windows::run_until_command(address, listener, ready_timeout).await
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (address, listener, ready_timeout);
        std::future::pending().await
    }
}
