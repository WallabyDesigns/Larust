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

pub use application::Application;
pub use config::{config, Config};
pub use error::AppError;

pub use axum;
