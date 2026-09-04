//! Process-wide debug-mode flag. Set once from `Application::new()`, read
//! by `AppError::into_response` (and the panic-catching layer in
//! `application.rs`) to decide whether to render a detailed error page or
//! today's generic one. Same `OnceLock`-backed process-wide-state idiom
//! `crates/larust-orm/src/pool.rs` uses for the connection pool.

use std::sync::OnceLock;

static DEBUG: OnceLock<bool> = OnceLock::new();

/// Idempotent - a second call (e.g. `Application::new()` running more than
/// once in the same process, such as in tests) is a silent no-op rather
/// than an error, since there's no meaningful conflict to report: the
/// value either already matches or it doesn't matter which one "won" for
/// a value this coarse.
pub(crate) fn set(value: bool) {
    let _ = DEBUG.set(value);
}

/// Unset defaults to `false` (production-safe), not an error - unlike
/// `pool()`'s getter, `AppError::into_response` must never fail while
/// building an error response, and "debug mode wasn't explicitly turned
/// on" is exactly the safe default anyway.
pub(crate) fn is_enabled() -> bool {
    DEBUG.get().copied().unwrap_or(false)
}
