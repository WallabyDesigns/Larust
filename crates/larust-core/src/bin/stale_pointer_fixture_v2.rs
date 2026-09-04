//! A second, deliberately distinguishable copy of `zero_downtime_fixture`
//! - used only by `tests/stale_binary_path.rs` to prove
//! `resolve_binary_path()` is re-read fresh at `RESTART`-receipt time
//! rather than captured once at boot. Its `/ping` response is
//! byte-distinguishable from `zero_downtime_fixture`'s own
//! (`pong-v2-...` vs `pong-...`), so a test can tell which binary
//! actually ended up serving after a handoff without any other
//! instrumentation.

use axum::routing::get;
use axum::Router;
use larust_core::{Application, GracefulShutdown};
use std::time::Duration;

async fn ping() -> String {
    format!("pong-v2-pid-{}", std::process::id())
}

/// No app-specific `config/app.rs` exists for this fixture (it lives
/// inside `larust-core` itself, which can't depend on `larust-support`'s
/// `config_env` helpers without a circular dependency) - reads `APP_PORT`/
/// `APP_NAME` directly. This process is only ever spawned as a
/// restart-handoff replacement for `zero_downtime_fixture` (never
/// directly by a test), so it relies on inheriting both from the
/// predecessor's own environment (`std::process::Command`'s default
/// behavior) rather than a test setting them on it explicitly - but reads
/// them the same way regardless, so it agrees with the test harness
/// (`tests/stale_binary_path.rs`) on the same `app_port`/`app_name` the
/// predecessor was using. Everything else falls back to `Config`'s own
/// defaults.
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

    app.router(router)
        .with_graceful_shutdown(GracefulShutdown {
            drain_timeout: Duration::from_secs(10),
            restart_channel: true,
        })
        .serve()
        .await
}
