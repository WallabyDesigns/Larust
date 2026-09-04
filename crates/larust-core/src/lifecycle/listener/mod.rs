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

/// Binds `addr` fresh - the ordinary startup path, unchanged from before
/// this feature existed.
pub fn bind(addr: SocketAddr) -> io::Result<TcpListener> {
    TcpListener::bind(addr)
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
