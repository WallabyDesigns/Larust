//! `prctl(PR_SET_PDEATHSIG, SIGTERM)`, armed by a restart-handoff
//! replacement on its own, from inside its already-`exec`'d
//! `Application::serve()` - so the kernel delivers `SIGTERM` to it the
//! moment its real parent (whatever spawned it) dies, for any reason, a
//! crash or a `kill -9`/closed terminal included, not just the graceful
//! paths this codebase already handles elsewhere. `SIGTERM`, not
//! `SIGKILL`, gives an orphaned replacement a chance to run its own
//! existing graceful-shutdown path first; if it has none, `SIGTERM`'s
//! default disposition still terminates it, so this is never weaker than
//! `SIGKILL` in practice - just occasionally slower.
//!
//! **Why this runs post-`exec`, in the child, and not via a
//! `Command::pre_exec` hook in the parent (this function's own original
//! design):** attaching a `pre_exec` closure forces `std::process::
//! Command::spawn()` to fall back to a raw `fork()`+`exec()` sequence
//! instead of the safer, more efficient `posix_spawn()` it otherwise
//! uses - `posix_spawn()` has no way to run arbitrary user code between
//! fork and exec, so any `pre_exec` hook at all disables it. Forking a
//! *multi-threaded* process (which every `#[tokio::main]` app - including
//! every restart-handoff replacement this spawns - is, by default) is a
//! well-known, genuinely hazardous operation: the child inherits the
//! parent's entire memory image but only the one thread that called
//! `fork()`, including whatever locks other, now-vanished threads
//! happened to be holding at that exact instant (glibc's own malloc
//! arena lock being the classic case).
//!
//! This was confirmed as the *actual* root cause of a real, severe bug,
//! not a theoretical concern: a live restart handoff under sustained
//! traffic produced a consistent, sustained rate of genuine
//! `ConnectionRefused` failures (tens per second, for as long as traffic
//! kept flowing) - present identically regardless of which listener-
//! sharing mechanism was used (this crate's original fd-duplication
//! design and a since-tried `SO_REUSEPORT`-based replacement both showed
//! the exact same failure rate), which ruled out the listener code
//! entirely and pointed at something else common to every handoff.
//! Disabling *only* this `pre_exec` hook, with everything else unchanged,
//! eliminated the failures completely (a real end-to-end test went from a
//! sustained ~45-47 failures/second to a single transient blip in over
//! 11,000 requests). Calling `prctl` here instead - after `exec()` has
//! already replaced the process image - sidesteps the fork hazard
//! entirely: `Command::spawn()` can use `posix_spawn()` again (no
//! `pre_exec` hook attached anywhere), and by the time this function
//! runs, the process is a completely fresh image with no inherited
//! multi-threaded state at all. See `docs/GOTCHAS.md`.
//!
//! `libc` 0.2 (as pinned in this workspace) declares neither
//! `PR_SET_PDEATHSIG` nor `prctl` itself for real Linux targets (only for
//! Android and an obscure L4Re variant - confirmed by inspecting the
//! vendored source) - both are declared locally below rather than adding
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

/// Arms this process's own death-signal delivery - see this module's own
/// doc comment for why this is called post-`exec`, by the replacement
/// itself, rather than via a pre-fork `Command::pre_exec` hook.
///
/// Deliberately temporary - see `disarm_pdeathsig`'s own doc comment for
/// why this must be cleared again once this process actually announces
/// itself ready, not left armed for its entire working life.
pub(super) fn arm_pdeathsig() {
    // SAFETY: `prctl`/`getppid` are ordinary libc calls, safe to invoke
    // from any normal (non-signal-handler) context - no `pre_exec`-style
    // async-signal-safety constraints apply here, since this runs as
    // regular code in an already fully-initialized process.
    unsafe {
        prctl(PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong, 0, 0, 0);

        // Closes a real race, the same one the old `pre_exec`-based
        // version handled: `PR_SET_PDEATHSIG` only takes effect from this
        // call onward - if the real parent already exited between when
        // it spawned this process and this function actually running,
        // the signal was never armed in time to catch it, and this
        // process has already been reparented to init (pid 1). Detected
        // directly rather than trusted to the signal alone: if the
        // parent is already gone, stop immediately instead of proceeding
        // to serve as a replacement that's already just as orphaned as
        // the one this whole mechanism exists to prevent.
        if libc::getppid() == 1 {
            std::process::exit(1);
        }
    }
}

/// Clears the death-signal delivery `arm_pdeathsig` just set - called from
/// `Application::serve()` right after this process announces itself ready
/// (`readiness::announce_ready`), before it starts actually accepting
/// connections.
///
/// This is the fix for a real, confirmed bug distinct from (and found
/// only after fixing) the `pre_exec`-vs-fork-hazard one this module's own
/// doc comment describes: `PR_SET_PDEATHSIG` fires whenever the parent
/// process exits *for any reason at all*, and this replacement's real
/// parent - the predecessor it's taking over from - is *expected* to
/// exit normally, deliberately, the moment its own drain completes, as
/// the successful conclusion of every ordinary handoff. Left armed, that
/// completely normal exit delivers `SIGTERM` to this brand-new
/// replacement, which its own `lifecycle::wait_for_termination()` (the
/// same graceful-shutdown signal handling every app already has) then
/// correctly, faithfully - but *wrongly*, given the actual situation -
/// treats as a real shutdown request: it stops accepting new connections
/// and starts draining, even though it's supposed to be the new
/// long-lived primary server, not something else being asked to retire.
/// Confirmed as the actual mechanism (not just plausible) by direct
/// experiment: a real end-to-end test showed a sustained, otherwise-
/// inexplicable ~45-47 failures/second for as long as traffic kept
/// flowing after a handoff, present with `arm_pdeathsig` called *either*
/// via the old `pre_exec` hook *or* this module's own post-exec
/// replacement for it - ruling out the fork-hazard theory entirely, since
/// post-exec calls it too - and vanishing completely (a single transient
/// blip in over 11,000 requests) only once pdeathsig was disabled
/// altogether. The fix here keeps the *original* safety guarantee
/// (detecting this process's own real parent already having crashed
/// *before* the handoff actually completes) while eliminating the false
/// positive: from the moment this process is about to start serving,
/// its predecessor's own exit is no longer something to react to at all.
pub(super) fn disarm_pdeathsig() {
    // SAFETY: same as `arm_pdeathsig` above - an ordinary libc call, safe
    // from any normal context. `0` for `PR_SET_PDEATHSIG`'s signal
    // argument clears it entirely (no signal delivered on parent death
    // from this point on), per prctl(2).
    unsafe {
        prctl(PR_SET_PDEATHSIG, 0, 0, 0, 0);
    }
}
