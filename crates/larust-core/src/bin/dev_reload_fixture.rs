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

#[tokio::main]
async fn main() -> Result<(), larust_core::AppError> {
    let app = Application::new()?;
    let router = Router::new().route("/ping", get(ping));
    app.router(router).serve().await
}
