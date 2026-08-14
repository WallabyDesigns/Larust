use std::io;
use std::net::TcpListener;
use std::os::fd::{FromRawFd, IntoRawFd, RawFd};

/// Clears `FD_CLOEXEC` on a *duplicate* of `listener`'s underlying fd, so
/// that duplicate survives into a child process across `fork`+`exec` (std
/// sets `FD_CLOEXEC` on every socket it creates by default, specifically
/// to *prevent* this — it has to be explicitly undone here, via a raw
/// `fcntl` call — `socket2` was tried first but doesn't expose a
/// `set_cloexec` setter on `Socket`, only caught by cross-compile
/// type-checking this file from the Windows machine this feature was
/// actually built on, since it can't run Unix code directly; see
/// `docs/GOTCHAS.md`). Must run before the child is spawned — Unix fd
/// inheritance is captured at `fork()` time, not something that can be
/// granted retroactively afterward.
///
/// Duplicating rather than clearing the flag on `listener`'s own fd
/// directly is deliberate: `listener` keeps serving in *this* process
/// (via its own, still-CLOEXEC, still-open fd) regardless of what happens
/// to the duplicate handed to the child — the two are independent fds
/// pointing at the same underlying kernel socket, exactly the "both
/// processes can `accept()` on one shared listen queue" shape this whole
/// mechanism needs.
pub(super) fn prepare_for_handoff(listener: &TcpListener) -> io::Result<String> {
    let fd: RawFd = listener.try_clone()?.into_raw_fd();
    clear_cloexec(fd)?;
    Ok(fd.to_string())
}

fn clear_cloexec(fd: RawFd) -> io::Result<()> {
    // SAFETY: `fd` is a valid, open file descriptor owned by this process
    // (just produced by `TcpListener::try_clone` + `into_raw_fd` above).
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Reconstructs a `TcpListener` from the fd number a parent process wrote
/// (via `prepare_for_handoff`, over this process's own stdin — see
/// `lifecycle::listener`'s module doc comment for why stdin and not an
/// env var).
pub(super) fn inherit(encoded: &str) -> io::Result<TcpListener> {
    let fd: RawFd = encoded
        .trim()
        .parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid inherited fd"))?;
    // SAFETY: `fd` was produced by `prepare_for_handoff` in the parent
    // process, with its `FD_CLOEXEC` flag explicitly cleared specifically
    // so it would survive into this child's own fd table under the exact
    // same number, per POSIX `fork`/`exec` semantics — it's expected to
    // still be open and valid here.
    //
    // Returned in ordinary blocking mode, same as a fresh `TcpListener::
    // bind` — `tokio::net::TcpListener::from_std` specifically requires
    // its input already be in non-blocking mode, but that's a detail of
    // *that* conversion, not of reconstructing the listener itself; the
    // call site that actually needs it (a later stage of this feature)
    // sets it right before making that conversion.
    Ok(unsafe { TcpListener::from_raw_fd(fd) })
}
