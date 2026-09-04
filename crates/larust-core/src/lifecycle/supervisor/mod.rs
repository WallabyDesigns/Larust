//! Framework-owned process supervision: every replacement `handoff.rs`
//! spawns is registered here, so the OS itself guarantees it dies if
//! `xr dev` (or a production `xr restart`-managed process) does, for any
//! reason - a crash, `taskkill /F`/`kill -9`, a closed terminal or IDE
//! window. Closes a real, repeatedly-hit gap: the zero-downtime handoff
//! design deliberately drops the parent's own handle to a generation once
//! handed off (`ServerState::HandedOff` in `xr dev` has no `Child` left to
//! kill), so without this, an orphaned replacement just keeps running,
//! holding its port, until something notices and kills it by hand.
//!
//! One API, two platform-specific mechanisms underneath - there is no
//! single OS primitive for "kill my children no matter how I die"; each
//! platform exposes a different one, or none. Windows: a Job Object with
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. Linux: `prctl(PR_SET_PDEATHSIG,
//! ...)`. Anything else (there is no third supported platform today - see
//! `docs/ARCHITECTURE.md`'s "Built and verified on both Linux and Windows")
//! gets a silent no-op rather than new unsupported-platform error
//! handling, matching this crate's other `lifecycle` modules' own
//! `#[cfg(not(any(unix, windows)))]` fallback arms.
//!
//! Two hook points, not one, because the two mechanisms act at different
//! points relative to spawning: Linux's has to be attached to the
//! `Command` *before* `.spawn()` (it modifies what happens inside the
//! child between `fork` and `exec`); Windows' needs the child's real
//! handle, which only exists *after* `.spawn()` returns. Both are
//! best-effort - a failure here is logged and otherwise ignored, never
//! propagated as a reason to fail the handoff itself.
//!
//! `pub(crate)`, not `pub` like the sibling `admin`/`handoff`/`listener`
//! modules - only `handoff.rs` ever calls into this; no fixture or
//! external integration test needs to reach it directly (they exercise it
//! indirectly, through a real `handoff::spawn_replacement_and_wait_for_ready`
//! call, the same as production).

#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
mod linux;

/// Call before `.spawn()` - see this module's own doc comment for why
/// this has to happen before, not after.
pub(crate) fn prepare(command: &mut tokio::process::Command) {
    #[cfg(target_os = "linux")]
    linux::prepare(command);
    #[cfg(not(target_os = "linux"))]
    let _ = command;
}

/// Call after `.spawn()` succeeds - but only for the *first* hop of a
/// handoff chain (`xr dev`/`xr restart` spawning generation 1 directly).
/// See `handoff::spawn_replacement_and_wait_for_ready`'s own doc comment
/// for the full explanation: on Windows, calling this again for a later,
/// server-to-server hop doesn't add protection, it *removes* the
/// replacement from the job it already automatically inherited and
/// re-homes it in a new one tied to the wrong process's lifetime.
pub(crate) fn register(child: &tokio::process::Child) {
    #[cfg(windows)]
    windows::register(child);
    #[cfg(not(windows))]
    let _ = child;
}
