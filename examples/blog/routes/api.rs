//! Laravel's `routes/api.php` equivalent — mounted under the configured
//! API prefix (`config/app.toml`'s `api_prefix`, `"/api"` by default) by
//! `main.rs`'s `.group(&app.config().api_prefix, ...)` call. Deliberately
//! empty of app routes for now: this app has no API-only endpoints yet,
//! and there's nothing here to move from `main.rs` (unlike `routes/web.rs`).
//!
//! Deliberately does **not** apply `.middleware(csrf::verify)` the way
//! `routes/web.rs` does — CSRF protects cookie-authenticated browser form
//! submissions specifically, which an API consumer doesn't participate in.
//! Add routes here the same way `routes/web.rs` does, e.g.:
//! `Route::get("/posts", ApiPostController::index)`.
//!
//! Rate-limited (60 requests/minute per caller, keyed by their real IP
//! address) — Laravel's own `throttle:60,1` default. Adjust via
//! `larust_http::throttle::per(max_requests, window)`.

use larust_http::Router;

pub fn routes() -> Router {
    Router::new().middleware(larust_http::throttle::per_minute(60))
}
