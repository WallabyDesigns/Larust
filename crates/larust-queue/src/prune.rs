//! Prunes old `failed_jobs` history - see [`crate::sql_worker::prune_failed_jobs`]/
//! [`crate::redis_worker::prune_failed_jobs`] for the two implementations.
//! Unlike [`crate::dispatch`]/[`crate::worker`], this has no per-job
//! correctness requirement at all - it's best-effort housekeeping, meant to
//! be called periodically (see `larust_support::maintenance::clean`), not
//! from the worker loop itself.

use larust_core::AppError;

/// Deletes/trims old entries from whichever `failed_jobs` store
/// [`crate::queue_driver`] selects - see the two backend-specific
/// implementations' own doc comments for the exact retention policy each
/// uses (time-based for SQL, count-based for Redis).
pub async fn prune_failed_jobs() -> Result<(), AppError> {
    match crate::queue_driver() {
        "redis" => crate::redis_worker::prune_failed_jobs().await,
        _ => crate::sql_worker::prune_failed_jobs().await,
    }
}
