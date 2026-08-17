//! Process lifecycle: graceful shutdown today, the dual-process
//! zero-downtime restart handoff (listener passing, readiness protocol,
//! admin restart channel) in later stages of the same feature.

// `pub`, not `pub(crate)` on all three of these — needed so `lib.rs`'s
// `#[doc(hidden)] pub mod __internal` can re-export them for this crate's
// own fixture binaries and integration tests (which are separate crates
// from this library, even though they share this package — see that
// module's doc comment).
pub mod admin;
pub mod handoff;
pub mod listener;
pub(crate) mod readiness;
mod signal;
// Not re-exported via `__internal` like the four above -- only
// `handoff.rs` ever calls into this; no fixture or external test needs
// to reach it directly (see `supervisor`'s own doc comment).
pub(crate) mod supervisor;

pub(crate) use signal::wait_for_termination;

use std::time::Duration;

/// Configuration for [`crate::Application::with_graceful_shutdown`] —
/// **opt-in**: an `Application` that never calls that method keeps today's
/// exact behavior (a bare `axum::serve` that exits the instant Ctrl+C is
/// pressed). Flipping every existing app's shutdown behavior silently
/// would be exactly the kind of surprise a framework upgrade shouldn't
/// introduce.
#[derive(Debug, Clone)]
pub struct GracefulShutdown {
    /// Upper bound on how long `serve()` waits for in-flight requests to
    /// finish after a shutdown signal, before forcing the process to exit
    /// anyway. Never "wait forever" — a stuck connection (a slow client, a
    /// hung upstream call) must not prevent a deploy or a plain Ctrl+C
    /// from ever completing.
    pub drain_timeout: Duration,
    /// If `true`, also runs the local restart-trigger admin channel (see
    /// `lifecycle::admin`) alongside plain Ctrl+C/SIGTERM handling — a
    /// second, independent opt-in on top of graceful shutdown itself,
    /// since not every app wants a local IPC surface open even when it
    /// does want graceful shutdown. `false` by default. The binary to
    /// spawn as a replacement is `std::env::current_exe()` for now (a
    /// release-pointer convention, so a *newly built* binary rather than
    /// the exact file currently running can be targeted, is a later
    /// addition to this same feature).
    pub restart_channel: bool,
}

impl Default for GracefulShutdown {
    fn default() -> Self {
        Self {
            drain_timeout: Duration::from_secs(30),
            restart_channel: false,
        }
    }
}
