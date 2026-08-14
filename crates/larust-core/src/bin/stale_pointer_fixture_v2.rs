//! A second, deliberately distinguishable copy of `zero_downtime_fixture`
//! — used only by `tests/stale_binary_path.rs` to prove
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

#[tokio::main]
async fn main() -> Result<(), larust_core::AppError> {
    let app = Application::new()?;
    let router = Router::new().route("/ping", get(ping));

    app.router(router)
        .with_graceful_shutdown(GracefulShutdown {
            drain_timeout: Duration::from_secs(10),
            restart_channel: true,
        })
        .serve()
        .await
}
