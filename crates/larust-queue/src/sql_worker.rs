//! The `"database"` (default) `queue_driver` implementation — unchanged
//! from before Redis support existed. `worker.rs` dispatches to this
//! module or [`crate::redis_worker`] based on `Config::queue_driver`.

use crate::worker::INITIAL_POLL_INTERVAL;
use crate::worker::{JOB_LEASE_TIMEOUT_SECS, MAX_ATTEMPTS, MAX_POLL_INTERVAL};
use crate::{ensure_tables, now_unix_secs, JobRegistry};
use larust_core::AppError;
use sqlx::AnyPool;

struct ClaimedJob {
    id: i64,
    job_type: String,
    payload: String,
    attempts: i64,
}

/// Atomically leases the oldest available row. A crashed worker leaves its
/// lease behind temporarily; a later claim releases stale leases first, so
/// work is at-least-once rather than silently lost.
///
/// Used to be one `UPDATE ... WHERE id = (SELECT ...) RETURNING ...`
/// statement — simpler, and still race-safe on SQLite — but MySQL has no
/// `RETURNING` clause at all, so this is now a portable 3-step claim used
/// identically on both backends (no branching needed, since nothing here
/// is backend-specific once `RETURNING` is gone):
///
/// 1. Find a candidate id (a plain, unlocked `SELECT`).
/// 2. Try to claim it with a conditional `UPDATE ... WHERE id = ? AND
///    reserved_at IS NULL` — the `AND reserved_at IS NULL` guard is what
///    keeps this race-safe: if a second worker's own claim attempt on the
///    same candidate loses the race, its `rows_affected()` comes back
///    `0`, not `1`, because the first worker's `UPDATE` already cleared
///    that condition.
/// 3. Only if step 2 actually won (`rows_affected() == 1`), fetch the
///    full row. If it lost (`0`), report the same "nothing to claim"
///    result step 1 finding nothing would — a caller can't tell the
///    difference between "empty queue" and "lost the race for the one
///    candidate," and doesn't need to: both mean "try again next poll."
async fn claim_next(pool: &AnyPool) -> Result<Option<ClaimedJob>, AppError> {
    let backend = larust_orm::backend();
    let now = now_unix_secs();
    let release_stale_sql = format!(
        "UPDATE jobs SET reserved_at = NULL WHERE reserved_at < {}",
        larust_orm::placeholder(backend, 1)
    );
    sqlx::query(&release_stale_sql)
        .bind(now - JOB_LEASE_TIMEOUT_SECS)
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    let select_candidate_sql = format!(
        "SELECT id FROM jobs WHERE reserved_at IS NULL AND available_at <= {} ORDER BY id LIMIT 1",
        larust_orm::placeholder(backend, 1)
    );
    let candidate: Option<(i64,)> = sqlx::query_as(&select_candidate_sql)
        .bind(now)
        .fetch_optional(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    let Some((id,)) = candidate else {
        return Ok(None);
    };

    let claim_sql = format!(
        "UPDATE jobs SET reserved_at = {}, attempts = attempts + 1 \
         WHERE id = {} AND reserved_at IS NULL",
        larust_orm::placeholder(backend, 1),
        larust_orm::placeholder(backend, 2),
    );
    let claimed = sqlx::query(&claim_sql)
        .bind(now)
        .bind(id)
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    if claimed.rows_affected() != 1 {
        // Lost the race for this one candidate to another worker between
        // the SELECT and this UPDATE — same "nothing to claim right now"
        // outcome as an empty queue.
        return Ok(None);
    }

    let select_claimed_sql = format!(
        "SELECT id, job_type, payload, attempts FROM jobs WHERE id = {}",
        larust_orm::placeholder(backend, 1)
    );
    let row: Option<(i64, String, String, i64)> = sqlx::query_as(&select_claimed_sql)
        .bind(id)
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

async fn record_failure(pool: &AnyPool, job: &ClaimedJob, error: &str) -> Result<(), AppError> {
    let backend = larust_orm::backend();
    let sql = format!(
        "INSERT INTO failed_jobs (job_type, payload, error, failed_at) VALUES ({}, {}, {}, {})",
        larust_orm::placeholder(backend, 1),
        larust_orm::placeholder(backend, 2),
        larust_orm::placeholder(backend, 3),
        larust_orm::placeholder(backend, 4),
    );
    sqlx::query(&sql)
        .bind(&job.job_type)
        .bind(&job.payload)
        .bind(error)
        .bind(now_unix_secs())
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    Ok(())
}

async fn release_for_retry(pool: &AnyPool, job: &ClaimedJob) -> Result<(), AppError> {
    // Small exponential delay keeps a permanently failing job from hot-looping
    // while preserving prompt retries for transient failures.
    let delay = 2_i64.pow((job.attempts - 1).clamp(0, 10) as u32);
    let backend = larust_orm::backend();
    let sql = format!(
        "UPDATE jobs SET reserved_at = NULL, available_at = {} WHERE id = {}",
        larust_orm::placeholder(backend, 1),
        larust_orm::placeholder(backend, 2),
    );
    sqlx::query(&sql)
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
async fn process_next(pool: &AnyPool, registry: &JobRegistry) -> Result<bool, AppError> {
    let Some(job) = claim_next(pool).await? else {
        return Ok(false);
    };

    let result = match registry.handlers.get(&job.job_type) {
        Some(handler) => handler(job.payload.clone()).await,
        None => Err(AppError::Internal(Box::new(std::io::Error::other(
            format!("no handler registered for job type {:?}", job.job_type),
        )))),
    };

    let delete_job_sql = format!(
        "DELETE FROM jobs WHERE id = {}",
        larust_orm::placeholder(larust_orm::backend(), 1)
    );

    match result {
        Ok(()) => {
            sqlx::query(&delete_job_sql)
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
                sqlx::query(&delete_job_sql)
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

pub(crate) async fn work(registry: JobRegistry) -> Result<(), AppError> {
    let pool = larust_orm::pool()?;
    ensure_tables(pool).await?;
    tracing::info!("queue worker started (database driver)");

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
    use crate::Job;
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
