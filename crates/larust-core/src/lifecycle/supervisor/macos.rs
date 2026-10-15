//! macOS has no direct equivalent to Linux's `prctl(PR_SET_PDEATHSIG, ...)`.
//! Darwin/BSD's own idiomatic "watch another process die" mechanism is
//! `kqueue`'s `EVFILT_PROC`/`NOTE_EXIT`, which (unlike `prctl`) delivers
//! nothing automatically - something has to actually block on the kqueue
//! waiting for the event. `arm_pdeathsig` below spawns a dedicated OS
//! thread for exactly that (not a tokio task: this runs very early, before
//! anything async-runtime-shaped is assumed ready, and the wait itself is
//! a blocking `kevent` call that would starve an executor thread) and,
//! once it sees the real parent exit, sends itself `SIGTERM` - not
//! `SIGKILL`, the same choice `linux.rs` makes and for the same reason:
//! gives an orphaned replacement a chance to run its own graceful-shutdown
//! path first.
//!
//! Mirrors both real bugs `linux.rs`'s own doc comment documents in full:
//! - **The spawn-to-arm race.** If the real parent already exited by the
//!   time this runs, this process has already been reparented to
//!   `launchd` (Darwin's init-equivalent, pid 1) - `getppid() == 1` is
//!   checked directly and exits immediately, rather than registering a
//!   kqueue watch against a parent PID that may already be gone (and,
//!   worse, may since have been *reused* by an unrelated process).
//! - **The disarm requirement.** This process's own predecessor is
//!   *expected* to exit, deliberately, the moment its own drain completes,
//!   the successful conclusion of every ordinary handoff. Left watching,
//!   that completely normal exit would be indistinguishable from a real
//!   crash. `disarm_pdeathsig` must stop the watcher reacting to it before
//!   this process starts actually serving.
//!
//! **Why the watcher thread closes its own kqueue fd, never `disarm_pdeathsig`
//! from another thread:** closing a file descriptor while a *different*
//! thread is blocked on it in a syscall is a well-known hazard - the
//! blocked call's behavior is unreliable, and if the fd number happens to
//! get reused by something else in the process before the blocked thread
//! notices, operations meant for the old resource can land on the new one
//! instead. `disarm_pdeathsig` never touches the fd directly: it submits a
//! second, independent `EVFILT_USER` event on the *same* kqueue, which
//! wakes the watcher thread through the completely ordinary "a new event
//! is ready" path `kevent` already handles - the watcher then closes its
//! own fd itself, from the one thread that was ever blocked on it, once
//! it's done.

use std::sync::atomic::{AtomicI32, Ordering};

/// Identifies `disarm_pdeathsig`'s own wake-up event among a kqueue that
/// also watches a real process exit - an arbitrary, fixed value (`kqueue`
/// never generates idents of its own for `EVFILT_USER`; the caller picks
/// one), distinct in *filter* from the `EVFILT_PROC` registration, so a
/// wake from this event is never mistaken for the parent actually exiting.
const DISARM_EVENT_IDENT: libc::uintptr_t = 1;

/// The armed watcher's own kqueue fd - `-1` when disarmed or never armed.
/// A plain `AtomicI32`, not a `Mutex`: the only two operations against it
/// are "set once, from `arm_pdeathsig`" and "read-and-clear, from
/// `disarm_pdeathsig`" - independent, single accesses with nothing larger
/// to coordinate.
static KQUEUE_FD: AtomicI32 = AtomicI32::new(-1);

/// See this module's own doc comment for why this is a background thread
/// blocking on `kqueue`, not a one-shot call the way Linux's `prctl`-based
/// version is.
pub(super) fn arm_pdeathsig() {
    // SAFETY: `getppid` is an ordinary libc call, safe from any normal
    // context.
    let parent_pid = unsafe { libc::getppid() };
    // Same race `linux.rs` closes - see this module's own doc comment.
    if parent_pid == 1 {
        std::process::exit(1);
    }

    // SAFETY: `kqueue()` takes no arguments and either returns a valid new
    // fd or `-1` on failure - checked immediately below.
    let kq = unsafe { libc::kqueue() };
    if kq < 0 {
        // Best-effort, like every other platform's mechanism here - see
        // this module's parent (`mod.rs`) doc comment. Nothing to arm if
        // the kqueue itself couldn't even be created.
        return;
    }

    let changes = [
        libc::kevent {
            ident: parent_pid as libc::uintptr_t,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_ENABLE,
            fflags: libc::NOTE_EXIT,
            data: 0,
            udata: std::ptr::null_mut(),
        },
        libc::kevent {
            ident: DISARM_EVENT_IDENT,
            filter: libc::EVFILT_USER,
            flags: libc::EV_ADD | libc::EV_CLEAR,
            fflags: 0,
            data: 0,
            udata: std::ptr::null_mut(),
        },
    ];
    // SAFETY: `kq` was just created above and is a valid kqueue fd;
    // `changes` is a fully-initialized, plain stack array of exactly the
    // shape `kevent` expects for a changelist with no output eventlist
    // requested (`nevents = 0`, `eventlist = null`).
    let registered = unsafe {
        libc::kevent(
            kq,
            changes.as_ptr(),
            changes.len() as libc::c_int,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    };
    if registered < 0 {
        // SAFETY: `kq` is this function's own, still-open, not-yet-shared
        // fd - nothing else could possibly be blocked on it yet.
        unsafe {
            libc::close(kq);
        }
        return;
    }

    KQUEUE_FD.store(kq, Ordering::SeqCst);
    std::thread::spawn(move || watch(kq, parent_pid));
}

/// Blocks on exactly one of the two events `arm_pdeathsig` registered:
/// either the watched parent actually exiting (`EVFILT_PROC`) or
/// `disarm_pdeathsig`'s own wake-up (`EVFILT_USER`) - see this module's
/// own doc comment for why the fd is closed from here, not from
/// `disarm_pdeathsig` on a different thread.
fn watch(kq: libc::c_int, parent_pid: libc::pid_t) {
    // SAFETY: a plain, zeroed `kevent` is a valid starting value for an
    // output buffer `kevent` below only ever writes into.
    let mut event: libc::kevent = unsafe { std::mem::zeroed() };
    // SAFETY: `kq` is the valid kqueue fd `arm_pdeathsig` created; `event`
    // is a correctly-sized single-element output buffer for `nevents = 1`.
    let n = unsafe { libc::kevent(kq, std::ptr::null(), 0, &mut event, 1, std::ptr::null()) };
    if n > 0 && event.filter == libc::EVFILT_PROC && event.ident == parent_pid as libc::uintptr_t {
        // The real parent actually exited - see this module's own doc
        // comment for why `SIGTERM`, not `SIGKILL`.
        // SAFETY: ordinary libc calls.
        unsafe {
            libc::kill(libc::getpid(), libc::SIGTERM);
        }
    }
    // Either the parent exited (handled above) or `disarm_pdeathsig`
    // triggered the `EVFILT_USER` wake-up - either way, this thread's only
    // job is done. Closing `kq` here, and only here, is what makes this
    // safe: no other thread ever touches this fd once `arm_pdeathsig`
    // hands it off.
    // SAFETY: `kq` is this thread's own fd, still open, never shared.
    unsafe {
        libc::close(kq);
    }
}

/// Wakes the watcher thread `arm_pdeathsig` spawned, so it stops watching
/// before this process starts actually serving - see this module's own
/// doc comment for why that matters (this process's own predecessor is
/// *expected* to exit at the end of a successful handoff).
pub(super) fn disarm_pdeathsig() {
    let kq = KQUEUE_FD.swap(-1, Ordering::SeqCst);
    if kq < 0 {
        // Never armed, or already disarmed - nothing to wake. The atomic
        // swap above guarantees at most one caller ever sees a real fd
        // here, so a second `disarm_pdeathsig` call (there should only
        // ever be one, but this is unsafe low-level code, not something
        // to trust callers alone to get right) is a safe no-op rather
        // than operating on a value someone else already claimed.
        return;
    }

    let wake = libc::kevent {
        ident: DISARM_EVENT_IDENT,
        filter: libc::EVFILT_USER,
        flags: 0,
        fflags: libc::NOTE_TRIGGER,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    // SAFETY: `kq` is the fd `arm_pdeathsig` created and registered this
    // exact `EVFILT_USER` event on; the swap above guarantees this is the
    // only call that can ever see and use this value, so it can't race a
    // second `disarm_pdeathsig` triggering (or, far worse, a closed and
    // possibly-reused fd being triggered) the same kqueue.
    unsafe {
        libc::kevent(kq, &wake, 1, std::ptr::null_mut(), 0, std::ptr::null());
    }
    // Deliberately doesn't close `kq` here - see this module's own doc
    // comment for why that's `watch`'s own job, from the one thread that
    // was ever blocked on it.
}
