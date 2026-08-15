use crate::{ensure_tables, now_unix_secs, Job};
use larust_core::AppError;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

/// How often `work()` checks the `jobs` table again after finding it
/// empty. Not configurable in v1 — a fixed, reasonable default, same
/// "hardcoded, not a toggle, until real pressure justifies one" shape as
/// `larust_http::session::EXPIRED_SESSION_CLEANUP_INTERVAL`.
const INITIAL_POLL_INTERVAL: Duration = Duration::from_millis(250);
const MAX_POLL_INTERVAL: Duration = Duration::from_secs(5);
const JOB_LEASE_TIMEOUT_SECS: i64 = 5 * 60;
const MAX_ATTEMPTS: i64 = 3;

type BoxedHandler =
    Box<dyn Fn(String) -> Pin<Box<dyn Future<Output = Result<(), AppError>> + Send>> + Send + Sync>;

/// Maps a `Job::JOB_TYPE` tag back to the concrete type that can
/// deserialize and run it — built fresh by whatever calls `work()`
/// (typically the generated app's own `queue:work` branch in `main.rs`),
/// not a process-wide static: only one `work()` loop reads it, in the one
/// process running it, so there's no cross-request sharing need the way
/// `larust-events`' listener registry or `larust_http::route`'s
/// named-route registry have.
#[must_use]
pub struct JobRegistry {
    handlers: HashMap<String, BoxedHandler>,
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
    /// `J::JOB_TYPE` — a startup-time registration collision is a real
    /// programmer bug (the shadowed job type would silently never run its
    /// own handler again); failing loudly and immediately here beats
    /// surfacing it later as "jobs of type X mysteriously stopped
    /// deserializing correctly."
    pub fn register<J: Job>(mut self) -> Self {
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
            "duplicate JobRegistry::register for JOB_TYPE {:?} — each job type must be \
             registered exactly once",
            J::JOB_TYPE,
        );
        self
    }
}

struct ClaimedJob {
    id: i64,
    job_type: String,
    payload: String,
    attempts: i64,
}

/// Atomically leases the oldest available row. A crashed worker leaves its
/// lease behind temporarily; a later claim releases stale leases first, so
/// work is at-least-once rather than silently lost.
async fn claim_next(pool: &SqlitePool) -> Result<Option<ClaimedJob>, AppError> {
    let now = now_unix_secs();
    sqlx::query("UPDATE jobs SET reserved_at = NULL WHERE reserved_at < ?")
        .bind(now - JOB_LEASE_TIMEOUT_SECS)
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    let row: Option<(i64, String, String, i64)> = sqlx::query_as(
        "UPDATE jobs SET reserved_at = ?, attempts = attempts + 1 \
         WHERE id = (SELECT id FROM jobs \
                     WHERE reserved_at IS NULL AND available_at <= ? \
                     ORDER BY id LIMIT 1) \
         RETURNING id, job_type, payload, attempts",
    )
    .bind(now)
    .bind(now)
    .fetch_optional(pool)
    .await
    .map_err(|source| AppError::Internal(Box::new(source)))?;

    Ok(row.map(|(id, job_type, payload, attempts)| ClaimedJob {
        id,
        job_type,
        payload,
        attempts,
    }))
}

async fn record_failure(pool: &SqlitePool, job: &ClaimedJob, error: &str) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO failed_jobs (job_type, payload, error, failed_at) VALUES (?, ?, ?, ?)",
    )
    .bind(&job.job_type)
    .bind(&job.payload)
    .bind(error)
    .bind(now_unix_secs())
    .execute(pool)
    .await
    .map_err(|source| AppError::Internal(Box::new(source)))?;
    Ok(())
}

async fn release_for_retry(pool: &SqlitePool, job: &ClaimedJob) -> Result<(), AppError> {
    // Small exponential delay keeps a permanently failing job from hot-looping
    // while preserving prompt retries for transient failures.
    let delay = 2_i64.pow((job.attempts - 1).clamp(0, 10) as u32);
    sqlx::query("UPDATE jobs SET reserved_at = NULL, available_at = ? WHERE id = ?")
        .bind(now_unix_secs() + delay)
        .bind(job.id)
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    Ok(())
}

/// Claims and executes at most one job. Returns `Ok(true)` if a job was
/// found (whether it succeeded or landed in `failed_jobs`), `Ok(false)` if
/// the queue was empty. Split out from `work()`'s infinite loop
/// specifically so it's directly testable without needing to bound or
/// time out an otherwise-endless loop.
async fn process_next(pool: &SqlitePool, registry: &JobRegistry) -> Result<bool, AppError> {
    let Some(job) = claim_next(pool).await? else {
        return Ok(false);
    };

    let result = match registry.handlers.get(&job.job_type) {
        Some(handler) => handler(job.payload.clone()).await,
        None => Err(AppError::Internal(Box::new(std::io::Error::other(
            format!("no handler registered for job type {:?}", job.job_type),
        )))),
    };

    match result {
        Ok(()) => {
            sqlx::query("DELETE FROM jobs WHERE id = ?")
                .bind(job.id)
                .execute(pool)
                .await
                .map_err(|source| AppError::Internal(Box::new(source)))?;
            tracing::info!(job_id = job.id, job_type = %job.job_type, "job processed");
        }
        Err(error) => {
            if job.attempts >= MAX_ATTEMPTS {
                tracing::error!(job_id = job.id, job_type = %job.job_type, attempts = job.attempts, %error, "job permanently failed");
                record_failure(pool, &job, &error.to_string()).await?;
                sqlx::query("DELETE FROM jobs WHERE id = ?")
                    .bind(job.id)
                    .execute(pool)
                    .await
                    .map_err(|source| AppError::Internal(Box::new(source)))?;
            } else {
                tracing::warn!(job_id = job.id, job_type = %job.job_type, attempts = job.attempts, %error, "job failed; scheduling retry");
                release_for_retry(pool, &job).await?;
            }
        }
    }

    Ok(true)
}

/// Runs forever, claiming and executing jobs one at a time until the
/// process is stopped (Ctrl+C / killed — no signal handling here, same as
/// Laravel's own `queue:work` without `--stop-when-empty`). A job whose
/// handler fails, or whose `job_type` has no registered handler, is
/// recorded in `failed_jobs` rather than retried or silently dropped.
pub async fn work(registry: JobRegistry) -> Result<(), AppError> {
    let pool = larust_orm::pool()?;
    ensure_tables(pool).await?;
    tracing::info!("queue worker started");

    let mut idle_interval = INITIAL_POLL_INTERVAL;
    loop {
        if process_next(pool, &registry).await? {
            // A busy queue should be drained without an artificial delay.
            idle_interval = INITIAL_POLL_INTERVAL;
        } else {
            tokio::time::sleep(idle_interval).await;
            idle_interval = idle_interval.saturating_mul(2).min(MAX_POLL_INTERVAL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch;
    use serde::{Deserialize, Serialize};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Serialize, Deserialize)]
    struct GreetJob {
        name: String,
    }

    impl Job for GreetJob {
        const JOB_TYPE: &'static str = "greet";

        async fn handle(&self) -> Result<(), AppError> {
            GREET_CALLS.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[derive(Serialize, Deserialize)]
    struct AlwaysFailsJob;

    impl Job for AlwaysFailsJob {
        const JOB_TYPE: &'static str = "always_fails";

        async fn handle(&self) -> Result<(), AppError> {
            Err(AppError::Internal(Box::new(std::io::Error::other(
                "deliberate test failure",
            ))))
        }
    }

    #[derive(Serialize, Deserialize)]
    struct UnregisteredJob;

    impl Job for UnregisteredJob {
        const JOB_TYPE: &'static str = "unregistered";

        async fn handle(&self) -> Result<(), AppError> {
            unreachable!("never registered, so this must never run")
        }
    }

    static GREET_CALLS: AtomicUsize = AtomicUsize::new(0);

    async fn connect_test_db() {
        let dir = tempfile::tempdir().unwrap().keep();
        let database_url = format!("sqlite://{}/test.sqlite", dir.display());
        larust_orm::connect(&database_url).await.unwrap();
    }

    #[tokio::test]
    async fn dispatch_and_work_round_trip_including_failures() {
        connect_test_db().await;
        let pool = larust_orm::pool().unwrap();

        dispatch(&GreetJob {
            name: "Alice".to_string(),
        })
        .await
        .unwrap();
        dispatch(&AlwaysFailsJob).await.unwrap();
        dispatch(&UnregisteredJob).await.unwrap();

        let registry = JobRegistry::new()
            .register::<GreetJob>()
            .register::<AlwaysFailsJob>();

        // Three jobs were dispatched; `process_next` claims exactly one
        // per call, oldest first (claim order == dispatch order).
        assert!(process_next(pool, &registry).await.unwrap());
        assert!(process_next(pool, &registry).await.unwrap());
        assert!(process_next(pool, &registry).await.unwrap());

        // Failed jobs are leased for a bounded retry instead of being lost.
        // Make their scheduled retry available immediately so this unit test
        // does not sleep through exponential backoff.
        for _ in 0..(MAX_ATTEMPTS - 1) {
            sqlx::query("UPDATE jobs SET available_at = 0 WHERE reserved_at IS NULL")
                .execute(pool)
                .await
                .unwrap();
            assert!(process_next(pool, &registry).await.unwrap());
            assert!(process_next(pool, &registry).await.unwrap());
        }
        // Terminal failures are recorded and removed after the final retry.
        assert!(!process_next(pool, &registry).await.unwrap());

        assert_eq!(
            GREET_CALLS.load(Ordering::SeqCst),
            1,
            "the registered, successful job must have actually run"
        );

        let failed: Vec<(String, String)> =
            sqlx::query_as("SELECT job_type, error FROM failed_jobs ORDER BY id")
                .fetch_all(pool)
                .await
                .unwrap();
        assert_eq!(
            failed.len(),
            2,
            "both the failing handler and the unregistered job type must land in failed_jobs after bounded retries"
        );
        assert_eq!(failed[0].0, "always_fails");
        assert!(failed[0].1.contains("deliberate test failure"));
        assert_eq!(failed[1].0, "unregistered");
        assert!(failed[1].1.contains("no handler registered"));

        let remaining: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM jobs")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(
            remaining.0, 0,
            "a claimed job (successful or failed) must not remain in the jobs table"
        );
    }

    #[tokio::test]
    #[should_panic(expected = "duplicate JobRegistry::register")]
    async fn registering_the_same_job_type_twice_panics() {
        let _ = JobRegistry::new()
            .register::<GreetJob>()
            .register::<GreetJob>();
    }
}
