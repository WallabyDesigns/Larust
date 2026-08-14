//! A real, standalone app binary used only by
//! `tests/zero_downtime_restart.rs` — the end-to-end proof that the whole
//! restart-handoff feature (Stages 2-5) delivers on its actual name.
//! Unlike `graceful_shutdown_fixture.rs`, this one opts into the admin
//! restart channel (`restart_channel: true`), and its `/ping` response
//! includes this process's own pid, so the test can prove *which*
//! process served each request across a live handoff, not just that
//! responses kept succeeding.

use axum::routing::get;
use axum::Router;
use larust_core::{Application, GracefulShutdown};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static REQUEST_COUNT: AtomicU64 = AtomicU64::new(0);

async fn ping() -> String {
    let n = REQUEST_COUNT.fetch_add(1, Ordering::SeqCst);
    format!("pong-{n}-pid-{}", std::process::id())
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
