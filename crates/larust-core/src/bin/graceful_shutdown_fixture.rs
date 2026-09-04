//! A real, standalone app binary used only by
//! `tests/graceful_shutdown.rs` - spawned as a genuine OS process so the
//! test can exercise real signal delivery and real socket-level request
//! draining, neither of which a `#[tokio::test]` in-process can express.
//! Not part of any generated app; lives here purely so
//! `env!("CARGO_BIN_EXE_graceful_shutdown_fixture")` resolves it without
//! any manual `target/debug/...` path guessing.

use axum::routing::get;
use axum::Router;
use larust_core::{Application, GracefulShutdown};
use std::time::Duration;

/// Deliberately slow, so the test can start a request, confirm it's
/// in-flight, send a shutdown signal mid-request, and assert the response
/// still completes successfully instead of being dropped.
async fn slow() -> &'static str {
    tokio::time::sleep(Duration::from_secs(2)).await;
    "slow-ok"
}

async fn fast() -> &'static str {
    "fast-ok"
}

/// No app-specific `config/app.rs` exists for this fixture (it lives
/// inside `larust-core` itself, which can't depend on `larust-support`'s
/// `config_env` helpers without a circular dependency) - reads `APP_PORT`
/// directly, the one field the test harness (`tests/graceful_shutdown.rs`)
/// actually overrides per run to avoid port collisions across parallel
/// runs. Everything else falls back to `Config`'s own defaults.
fn config() -> serde_json::Value {
    let mut value = serde_json::json!({});
    if let Ok(port) = std::env::var("APP_PORT").unwrap_or_default().parse::<u16>() {
        value["app_port"] = serde_json::json!(port);
    }
    value
}

#[tokio::main]
async fn main() -> Result<(), larust_core::AppError> {
    let app = Application::new(config)?;
    let router = Router::new()
        .route("/slow", get(slow))
        .route("/fast", get(fast));

    app.router(router)
        .with_graceful_shutdown(GracefulShutdown {
            // Comfortably longer than /slow's own sleep, short enough that
            // a genuinely stuck test still fails in reasonable time.
            drain_timeout: Duration::from_secs(8),
            ..Default::default()
        })
        .serve()
        .await
}
