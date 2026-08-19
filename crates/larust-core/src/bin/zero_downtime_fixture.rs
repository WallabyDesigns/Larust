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

/// No app-specific `config/app.rs` exists for this fixture (it lives
/// inside `larust-core` itself, which can't depend on `larust-support`'s
/// `config_env` helpers without a circular dependency) — reads `APP_PORT`/
/// `APP_NAME` directly, the two fields the test harnesses
/// (`tests/zero_downtime_restart.rs`, `tests/admin_stop.rs`,
/// `tests/stale_binary_path.rs`) override per run: a reserved port to
/// avoid collisions, and a unique `app_name` so this process and the
/// test's own admin-channel client agree on the same channel address.
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

    app.router(router)
        .with_graceful_shutdown(GracefulShutdown {
            drain_timeout: Duration::from_secs(10),
            restart_channel: true,
        })
        .serve()
        .await
}
