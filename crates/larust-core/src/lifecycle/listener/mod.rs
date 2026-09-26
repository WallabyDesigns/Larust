//! Cross-platform listener handoff - shares one already-bound, already-
//! listening kernel socket between an old process and its replacement, so
//! neither has to briefly stop listening (or, worse, both bind
//! independently and fight over the same port) during a restart. The
//! underlying mechanism is necessarily different per platform (fd
//! inheritance across `fork`+`exec` on Unix, `WSADuplicateSocket` on
//! Windows - there's no cross-platform "just works" API for this), but
//! both are unified behind the same interface here: encode the listener as
//! a string to hand a specific child process (`prepare_for_handoff`), and
//! reconstruct it from that same string on the child side (`inherit`).
//!
//! **Why one shared socket, not two independent ones (`SO_REUSEPORT`)**:
//! that alternative was tried and reverted - see `unix::prepare_for_handoff`'s
//! own doc comment for the real, reproduced bug it introduced (a stale or
//! never-serviced socket bound to the same port silently steals a
//! fraction of traffic forever, with the kernel's own load-balancing hash
//! giving no error or signal anywhere). The shared-socket design here has
//! no such ambiguity: there is only ever one accept queue, so whichever
//! process actually calls `accept()` gets the connection, full stop.
//!
//! Transport is the child's own stdin, not an env var, even on Unix where
//! that isn't strictly required - Windows' `WSADuplicateSocketW` needs the
//! child's real PID, which only exists *after* `Command::spawn()` returns,
//! by which point env vars can no longer be added to its environment;
//! stdin can still be written to at that point. Using the same transport
//! on both platforms keeps the parent-side orchestration (`handoff.rs`, a
//! later stage of this same feature) to one code path instead of two.

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

use std::io;
use std::net::{SocketAddr, TcpListener};

/// Set (to `"1"`) in a spawned replacement's environment *before* it
/// starts, so its own startup knows to read an inherited listener's
/// encoding from stdin instead of binding `addr` fresh. The encoded value
/// itself deliberately does **not** travel through an env var - see the
/// module doc comment above for why.
pub const INHERIT_LISTENER_ENV: &str = "LARUST_INHERIT_LISTENER";

/// Larger than std's own `TcpListener::bind`, which hardcodes a small
/// default (128 on most platforms) with no way to override it - real,
/// reachable in production under any sudden burst of concurrent
/// connections, not just this framework's own restart-handoff window.
/// Matches the same ballpark other production web servers ship with by
/// default (nginx, Node.js both default to 511); the kernel clamps this
/// to `net.core.somaxconn` regardless, so requesting more than a given
/// machine allows is always safe, never an error.
const LISTEN_BACKLOG: i32 = 1024;

/// Binds `addr` fresh - the ordinary startup path, unchanged from before
/// this feature existed. Uses `socket2` instead of `std::net::
/// TcpListener::bind` directly specifically for its explicit
/// `listen(backlog)` control - see `LISTEN_BACKLOG`'s own doc comment for
/// why the value matters. Otherwise identical to `TcpListener::bind`'s own
/// defaults: a plain blocking socket, no `SO_REUSEADDR` (std doesn't set
/// it either), `FD_CLOEXEC` left alone (only the restart-handoff's own
/// duplicated fd needs that cleared, done separately and explicitly in
/// `prepare_for_handoff`).
pub fn bind(addr: SocketAddr) -> io::Result<TcpListener> {
    use socket2::{Domain, Socket, Type};

    let socket = Socket::new(Domain::for_address(addr), Type::STREAM, None)?;
    socket.bind(&addr.into())?;
    socket.listen(LISTEN_BACKLOG)?;
    Ok(socket.into())
}

/// Prepares `listener` to be handed to a specific child process
/// (`child_pid` - required by the Windows implementation, ignored by the
/// Unix one; see the module doc comment). Returns the line of text to
/// write to that child's stdin.
pub fn prepare_for_handoff(listener: &TcpListener, child_pid: u32) -> io::Result<String> {
    #[cfg(unix)]
    {
        let _ = child_pid;
        unix::prepare_for_handoff(listener)
    }
    #[cfg(windows)]
    {
        windows::prepare_for_handoff(listener, child_pid)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (listener, child_pid);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "listener handoff is only supported on Unix and Windows",
        ))
    }
}

/// Closes this process's own copy of the fd/handle `prepare_for_handoff`
/// created for the child to inherit, once that child has actually been
/// spawned. Unix-specific: fork already gave the child its own
/// independent copy at the same fd number by the time this is safe to
/// call, and without this call this process's own copy would otherwise
/// leak for the rest of its life, one fd per restart attempt. A no-op on
/// Windows - `WSADuplicateSocketW` never hands this process a live
/// handle of its own to leak in the first place, just opaque bytes
/// describing how the *other* process should reconstruct one.
pub fn close_duplicated_fd(encoded: &str) {
    #[cfg(unix)]
    {
        unix::close_duplicated_fd(encoded);
    }
    #[cfg(not(unix))]
    {
        let _ = encoded;
    }
}

/// Reconstructs a listener from the line of text a parent wrote to this
/// process's own stdin (see `prepare_for_handoff`).
pub fn inherit(encoded: &str) -> io::Result<TcpListener> {
    #[cfg(unix)]
    {
        unix::inherit(encoded)
    }
    #[cfg(windows)]
    {
        windows::inherit(encoded)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = encoded;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "listener handoff is only supported on Unix and Windows",
        ))
    }
}
