//! Application bootstrap: config loading, logging, and the Axum server.
//!
//! Re-exports `axum` so generated Larust applications don't need to depend
//! on it directly for router/type access. `tokio` cannot be re-exported the
//! same way because `#[tokio::main]`'s macro expansion requires `tokio` to
//! be a directly resolvable crate name at the call site.

mod application;
mod config;
mod debug;
mod dev_reload;
mod error;
mod lifecycle;
mod paths;
mod state;

pub use application::Application;
pub use config::{config, Config};
pub use error::AppError;
pub use lifecycle::GracefulShutdown;
pub use paths::AppPaths;
pub use state::AppState;

pub use axum;

/// Escape hatch for this crate's own `src/bin/*_fixture.rs` integration-test
/// fixtures — a binary under `src/bin/` is a *separate* crate from this
/// library, even though it shares the same package, so it can only reach
/// `pub` items, never `pub(crate)` ones, the same as any other external
/// consumer. Not part of the stable public API: no semver guarantees, not
/// intended for use outside this crate's own `tests/`.
#[doc(hidden)]
pub mod __internal {
    pub use crate::lifecycle::{admin, handoff, listener};
}
