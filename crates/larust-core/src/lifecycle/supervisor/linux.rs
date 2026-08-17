//! Linux backend for `lifecycle::supervisor`: `prctl(PR_SET_PDEATHSIG,
//! SIGTERM)`, set on every spawned replacement (inside the child, before
//! `exec`, via a `pre_exec` hook) so the kernel delivers `SIGTERM` to it
//! the moment its parent (`xr dev` itself) dies — for any reason, a crash
//! or a `kill -9`/closed terminal included, not just the graceful paths
//! this codebase already handles elsewhere. `SIGTERM`, not `SIGKILL`,
//! gives an orphaned replacement a chance to run its own existing
//! graceful-shutdown path first; if it has none, `SIGTERM`'s default
//! disposition still terminates it, so this is never weaker than
//! `SIGKILL` in practice — just occasionally slower.
//!
//! `libc` 0.2 (as pinned in this workspace) declares neither
//! `PR_SET_PDEATHSIG` nor `prctl` itself for real Linux targets (only for
//! Android and an obscure L4Re variant — confirmed by inspecting the
//! vendored source) — both are declared locally below rather than adding
//! a new dependency for two constants.

const PR_SET_PDEATHSIG: libc::c_int = 1; // <linux/prctl.h>, stable since Linux 2.1.57

extern "C" {
    fn prctl(
        option: libc::c_int,
        arg2: libc::c_ulong,
        arg3: libc::c_ulong,
        arg4: libc::c_ulong,
        arg5: libc::c_ulong,
    ) -> libc::c_int;
}

/// Attaches the `pre_exec` hook — must run *before* `.spawn()`, since
/// `PR_SET_PDEATHSIG` has to be set inside the child itself, before
/// `exec`; there's no way to apply it to an already-running process from
/// the outside the way `lifecycle::supervisor`'s Windows backend can.
pub(super) fn prepare(command: &mut tokio::process::Command) {
    // `tokio::process::Command` forwards `pre_exec` as an inherent method
    // on Unix (not via `std::os::unix::process::CommandExt` -- importing
    // that trait here is flagged unused by the compiler), so no extra
    // `use` is needed to call it below.

    // SAFETY: `pre_exec`'s own contract requires the closure be
    // async-signal-safe (it runs in the child between `fork` and `exec`,
    // where only a narrow, well-defined set of operations is safe) — this
    // closure makes only raw `prctl`/`getppid`/`_exit` libc calls, no
    // allocation, no locking, satisfying that.
    unsafe {
        command.pre_exec(|| {
            // Arms the kernel's death-signal delivery for this (about to
            // become the child's) process.
            prctl(PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong, 0, 0, 0);

            // Closes a real race: `PR_SET_PDEATHSIG` only takes effect
            // from this call onward — if the real parent already exited
            // between `fork()` and this line, the signal was never armed
            // in time to catch it, and this process has already been
            // reparented to init (pid 1). Detect that directly rather
            // than trust the signal alone: if the parent is already gone,
            // stop immediately instead of proceeding to `exec` a
            // replacement that would itself become just as orphaned as
            // the one this whole feature exists to prevent.
            if libc::getppid() == 1 {
                libc::_exit(1);
            }

            Ok(())
        });
    }
}
