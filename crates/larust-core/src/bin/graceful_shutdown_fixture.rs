//! A real, standalone app binary used only by
//! `tests/graceful_shutdown.rs` — spawned as a genuine OS process so the
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

#[tokio::main]
async fn main() -> Result<(), larust_core::AppError> {
    let app = Application::new()?;
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
