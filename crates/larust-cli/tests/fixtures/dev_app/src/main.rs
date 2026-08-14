//! Minimal, real, standalone Larust app used only by `tests/dev_e2e.rs` to
//! exercise `xr dev`'s zero-downtime reload end-to-end: a real `cargo
//! build`, a real spawn, a real rebuild triggered by a real file change,
//! driving real HTTP traffic through the whole thing. Copied into a fresh
//! tempdir before each test run (see `tests/dev_e2e.rs`) rather than built
//! in place, so repeated/parallel runs never collide on `target/`,
//! `storage/releases/`, or a shared admin-channel address.
//!
//! Its own `[workspace]` table (see `Cargo.toml`) keeps it from being
//! swept into the outer `RustLaravel` workspace.

use axum::routing::get;
use axum::Router;
use larust_core::Application;

async fn ping() -> String {
    format!("pong-pid-{}", std::process::id())
}

#[tokio::main]
async fn main() -> Result<(), larust_core::AppError> {
    let app = Application::new()?;
    let router = Router::new().route("/ping", get(ping));
    app.router(router).serve().await
}
