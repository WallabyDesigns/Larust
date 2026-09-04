//! `JobRegistry` (shared, driver-agnostic) plus `work()`'s runtime driver
//! dispatch - mirrors `dispatch.rs`'s own shape (see that module's doc
//! comment for the underlying precedent).

use larust_core::AppError;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

/// How often a worker checks for new work again after finding none. Not
/// configurable in v1 - a fixed, reasonable default, same "hardcoded, not
/// a toggle, until real pressure justifies one" shape as
/// `larust_http::session::EXPIRED_SESSION_CLEANUP_INTERVAL`. Shared by
/// both drivers' own `work()` loop.
pub(crate) const INITIAL_POLL_INTERVAL: Duration = Duration::from_millis(250);
pub(crate) const MAX_POLL_INTERVAL: Duration = Duration::from_secs(5);
/// How long a claimed-but-unfinished job's lease is honored before another
/// worker is allowed to reclaim it (a crashed worker's own leftover claim).
pub(crate) const JOB_LEASE_TIMEOUT_SECS: i64 = 5 * 60;
pub(crate) const MAX_ATTEMPTS: i64 = 3;

pub(crate) type BoxedHandler =
    Box<dyn Fn(String) -> Pin<Box<dyn Future<Output = Result<(), AppError>> + Send>> + Send + Sync>;

/// Maps a `Job::JOB_TYPE` tag back to the concrete type that can
/// deserialize and run it - built fresh by whatever calls `work()`
/// (typically the generated app's own `queue:work` branch in `main.rs`),
/// not a process-wide static: only one `work()` loop reads it, in the one
/// process running it, so there's no cross-request sharing need the way
/// `larust-events`' listener registry or `larust_http::route`'s
/// named-route registry have.
#[must_use]
pub struct JobRegistry {
    pub(crate) handlers: HashMap<String, BoxedHandler>,
}

impl Default for JobRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl JobRegistry {
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    /// Registers `J` so `work()` can run it. Panics on a duplicate
    /// `J::JOB_TYPE` - a startup-time registration collision is a real
    /// programmer bug (the shadowed job type would silently never run its
    /// own handler again); failing loudly and immediately here beats
    /// surfacing it later as "jobs of type X mysteriously stopped
    /// deserializing correctly."
    pub fn register<J: crate::Job>(mut self) -> Self {
        let handler: BoxedHandler = Box::new(|payload: String| {
            Box::pin(async move {
                let job: J = serde_json::from_str(&payload)
                    .map_err(|source| AppError::Internal(Box::new(source)))?;
                job.handle().await
            })
        });
        let existing = self.handlers.insert(J::JOB_TYPE.to_string(), handler);
        assert!(
            existing.is_none(),
            "duplicate JobRegistry::register for JOB_TYPE {:?} - each job type must be \
             registered exactly once",
            J::JOB_TYPE,
        );
        self
    }
}

/// Runs forever, claiming and executing jobs one at a time until the
/// process is stopped (Ctrl+C / killed - no signal handling here, same as
/// Laravel's own `queue:work` without `--stop-when-empty`). A job whose
/// handler fails, or whose `job_type` has no registered handler, is
/// recorded as a failure rather than retried forever or silently dropped.
pub async fn work(registry: JobRegistry) -> Result<(), AppError> {
    match crate::queue_driver() {
        "redis" => crate::redis_worker::work(registry).await,
        _ => crate::sql_worker::work(registry).await,
    }
}
