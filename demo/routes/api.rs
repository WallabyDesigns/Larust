//! Laravel's `routes/api.php` equivalent — mounted under the configured
//! API prefix (`config/app.toml`'s `api_prefix`, `"/api"` by default) by
//! `main.rs`'s `.group(&app.config().api_prefix, ...)` call.
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
//!
//! `Route::get(path, handler)` here is the exact same entry point
//! `routes/web.rs` chains off of — it accepts an inline closure just as
//! readily as a named function/controller method, both being anything that
//! implements axum's `Handler` trait, so a route can stay a one-off closure
//! the way Laravel's `Route::get('/test/{id}', function (Request $request,
//! $id) { ... })` does instead of needing its own named handler.
//!
//! `larust_http::Request` (below) is this stack's `Illuminate\Http\Request`
//! — a thin wrapper over the current request's headers with the same
//! `->header('X-Foo')` / `->headers` shape Laravel devs already know.
//! `{id}` still comes through its own `Path<i64>` parameter rather than
//! `$request->route('id')`: unlike Laravel's untyped route-parameter
//! lookup, `Path<T>` parses `{id}` into `T` (here `i64`) before the closure
//! body even runs, rejecting the request with a 400 if it doesn't parse —
//! no `is_numeric()`/manual-cast check needed inside the handler.

use larust_http::{Request, Route, Router};
use larust_support::axum::extract::Path;
use larust_support::axum::Json;
use larust_support::serde_json::json;

pub fn routes() -> Router {
    Route::get(
        "/demo/{id}",
        |request: Request, Path(id): Path<i64>| async move {
            Json(json!({
                "status": 200,
                "headers": request.headers(),
                "id": id,
            }))
        },
    )
    .middleware(larust_http::throttle::per_minute(60))
}
