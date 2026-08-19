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

/// No app-specific `config/app.rs` exists for this minimal fixture (it
/// doesn't depend on `larust-support`) — reads `APP_PORT`/`APP_NAME`
/// directly, the two fields `tests/dev_e2e.rs` overrides per run (a
/// reserved port, and a unique `app_name` so this process and the test's
/// own admin-channel client agree on the same channel address).
/// Everything else falls back to `Config`'s own defaults.
fn config() -> serde_json::Value {
    let mut value = serde_json::json!({});
    if let Ok(port) = std::env::var("APP_PORT").unwrap_or_default().parse::<u16>() {
        value["app_port"] = serde_json::json!(port);
    }
    if let Ok(name) = std::env::var("APP_NAME") {
        value["app_name"] = serde_json::json!(name);
    }
    value
}

#[tokio::main]
async fn main() -> Result<(), larust_core::AppError> {
    let app = Application::new(config)?;
    let router = Router::new().route("/ping", get(ping));
    app.router(router).serve().await
}
