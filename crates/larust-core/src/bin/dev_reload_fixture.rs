//! A real, standalone app binary used only by
//! `tests/dev_reload_auto_enable.rs` — deliberately does **not** call
//! `.with_graceful_shutdown(...)` at all, to prove `Application::serve()`
//! auto-enables graceful shutdown plus the restart admin channel purely
//! from `LARUST_DEV_RELOAD` being set, with zero app-level opt-in
//! required (exactly the behavior `xr dev`'s own rebuild-on-save loop
//! depends on).

use axum::routing::get;
use axum::Router;
use larust_core::Application;

async fn ping() -> String {
    format!("pong-pid-{}", std::process::id())
}

/// No app-specific `config/app.rs` exists for this fixture (it lives
/// inside `larust-core` itself, which can't depend on `larust-support`'s
/// `config_env` helpers without a circular dependency) — reads `APP_PORT`/
/// `APP_NAME` directly, the two fields the test harness
/// (`tests/dev_reload_auto_enable.rs`) overrides per run: a reserved port
/// to avoid collisions, and a unique `app_name` so this process and the
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
    app.router(router).serve().await
}
