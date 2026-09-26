//! Framework-owned process supervision: every replacement `handoff.rs`
//! spawns is guaranteed to die if `xr dev` (or a production `xr restart`-
//! managed process) does, for any reason - a crash, `taskkill /F`/
//! `kill -9`, a closed terminal or IDE window. Closes a real, repeatedly-
//! hit gap: the zero-downtime handoff design deliberately drops the
//! parent's own handle to a generation once handed off (`ServerState::
//! HandedOff` in `xr dev` has no `Child` left to kill), so without this,
//! an orphaned replacement just keeps running, holding its port, until
//! something notices and kills it by hand.
//!
//! One goal, two platform-specific mechanisms underneath - there is no
//! single OS primitive for "kill my children no matter how I die"; each
//! platform exposes a different one, or none. Windows: a Job Object with
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, registered by the parent right
//! after `.spawn()` returns (`register`, called from `handoff.rs`).
//! Linux: `prctl(PR_SET_PDEATHSIG, ...)`, armed by the *replacement
//! itself*, from inside its own already-`exec`'d `Application::serve()`
//! (`arm_pdeathsig`, called from `application.rs`) - see that function's
//! own doc comment for why this isn't done via a `Command::pre_exec` hook
//! the way it originally was.
//!
//! Anything else (there is no third supported platform today - see
//! `docs/ARCHITECTURE.md`'s "Built and verified on both Linux and Windows")
//! gets a silent no-op rather than new unsupported-platform error
//! handling, matching this crate's other `lifecycle` modules' own
//! `#[cfg(not(any(unix, windows)))]` fallback arms. Both mechanisms are
//! best-effort - a failure here is logged and otherwise ignored, never
//! propagated as a reason to fail the handoff itself.
//!
//! `pub(crate)`, not `pub` like the sibling `admin`/`handoff`/`listener`
//! modules - only `handoff.rs`/`application.rs` ever call into this; no
//! fixture or external integration test needs to reach it directly (they
//! exercise it indirectly, through a real `handoff::
//! spawn_replacement_and_wait_for_ready` call, the same as production).

#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
mod linux;

/// Call from inside an already-`exec`'d restart-handoff replacement's own
/// `Application::serve()`, as early as possible - see `linux::
/// arm_pdeathsig`'s own doc comment for why this runs here (post-exec, in
/// the child) rather than via a pre-fork `Command::pre_exec` hook the way
/// this used to work. A no-op on every platform but Linux - Windows'
/// equivalent (`register`, below) is entirely parent-side instead.
///
/// Temporary - must be paired with `disarm_pdeathsig` once this process
/// actually announces itself ready. See that function's own doc comment
/// for the real, confirmed bug leaving this armed permanently caused.
pub(crate) fn arm_pdeathsig() {
    #[cfg(target_os = "linux")]
    linux::arm_pdeathsig();
}

/// Call right after this process announces itself ready
/// (`readiness::announce_ready`), pairing with `arm_pdeathsig` above - see
/// `linux::disarm_pdeathsig`'s own doc comment for why this is required,
/// not optional cleanup. A no-op on every platform but Linux, matching
/// `arm_pdeathsig`.
pub(crate) fn disarm_pdeathsig() {
    #[cfg(target_os = "linux")]
    linux::disarm_pdeathsig();
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
