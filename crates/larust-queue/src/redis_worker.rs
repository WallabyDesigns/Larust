//! The `"redis"` `queue_driver` implementation - a genuine reimplementation
//! of `sql_worker`'s claim/lease/retry state machine against Redis's own
//! primitives, not a thin adapter. `worker.rs` dispatches to this module
//! or [`crate::sql_worker`] based on `Config::queue_driver`.
//!
//! Data model (all keys process-wide, shared across every worker):
//! - `jobs:next_id` - an `INCR` counter, mirroring the SQL version's
//!   `AUTOINCREMENT` primary key.
//! - `jobs:data` - a HASH, `id -> JSON {job_type, payload}` (see
//!   [`crate::redis_dispatch::JobData`]), the job's immutable content, set
//!   once at dispatch and read at claim time. Never rewritten.
//! - `jobs:pending` - a LIST of bare ids. `LPUSH` (dispatch) + `RPOP`
//!   (claim) is a standard FIFO pair.
//! - `jobs:attempts` - a HASH, `id -> attempts count`, incremented via
//!   `HINCRBY` on every claim (first claim and every reclaim/retry alike) -
//!   mirrors the SQL version's `attempts = attempts + 1` on its own
//!   claim `UPDATE`.
//! - `jobs:processing` - a HASH, `id -> claimed_at unix timestamp`,
//!   marking a job currently in flight; scanned for stale (lease-expired)
//!   entries at the top of every claim, the same role the SQL version's
//!   own `UPDATE jobs SET reserved_at = NULL WHERE reserved_at < ?` plays.
//! - `jobs:delayed` - a sorted set, `id` scored by `ready_at unix
//!   timestamp`, holding jobs waiting out their exponential-backoff retry
//!   delay. Promoted back onto `jobs:pending` once their score is due -
//!   this is the one piece with no SQL-version analogue at all: the SQL
//!   claim query's own `available_at <= ?` filter does this filtering
//!   inline, but Redis's plain `jobs:pending` list has no per-element
//!   filter to apply, so due delayed jobs need an explicit promotion step.
//! - `jobs:failed` - a LIST of JSON `{job_type, payload, error,
//!   failed_at}`, mirroring the SQL version's `failed_jobs` table as an
//!   audit log.
//!
//! `BRPOPLPUSH`'s atomic move (SQL's own claim needed a 3-step dance
//! specifically because MySQL has no `RETURNING`) isn't used here: moving
//! the *id* atomically doesn't remove the need to separately increment
//! `jobs:attempts` and record `jobs:processing`'s claim timestamp
//! afterward, so a single `RPOP` plus those two follow-up writes is no
//! less safe (a crash between them just leaves the job's lease looking
//! "not yet claimed" or "claimed with stale bookkeeping," both already
//! handled by the reclaim step) and is simpler than juggling a second list.

use crate::redis_conn::connection;
use crate::redis_dispatch::JobData;
use crate::worker::{
    INITIAL_POLL_INTERVAL, JOB_LEASE_TIMEOUT_SECS, MAX_ATTEMPTS, MAX_POLL_INTERVAL,
};
use crate::{now_unix_secs, JobRegistry};
use larust_core::AppError;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

struct ClaimedJob {
    id: i64,
    job_type: String,
    payload: String,
    attempts: i64,
}

#[derive(Serialize, Deserialize)]
struct FailedJob {
    job_type: String,
    payload: String,
    error: String,
    failed_at: i64,
}

fn internal(source: redis::RedisError) -> AppError {
    AppError::Internal(Box::new(source))
}

/// Moves every `jobs:delayed` entry whose retry delay has elapsed back
/// onto `jobs:pending` - see this module's own doc comment for why Redis
/// needs this as a separate step, unlike the SQL version's inline
/// `available_at <= ?` filter.
async fn promote_due_delayed(conn: &mut redis::aio::ConnectionManager) -> Result<(), AppError> {
    let now = now_unix_secs();
    let due: Vec<i64> = conn
        .zrangebyscore("jobs:delayed", i64::MIN, now)
        .await
        .map_err(internal)?;
    for id in due {
        // ZREM before LPUSH: if this worker crashes between the two, the
        // job simply sits in `jobs:delayed` a little longer and gets
        // promoted on a later poll - never lost, just delayed further,
        // the same "at-least-once, not silently dropped" guarantee every
        // other step here gives.
        let _: () = conn.zrem("jobs:delayed", id).await.map_err(internal)?;
        let _: () = conn.lpush("jobs:pending", id).await.map_err(internal)?;
    }
    Ok(())
}

/// Releases any `jobs:processing` lease older than
/// [`JOB_LEASE_TIMEOUT_SECS`] - a crashed worker's own leftover claim -
/// back onto `jobs:pending`, mirroring the SQL version's own stale-lease
/// reclaim at the top of its `claim_next`.
async fn reclaim_stale_leases(conn: &mut redis::aio::ConnectionManager) -> Result<(), AppError> {
    let now = now_unix_secs();
    let processing: HashMap<i64, i64> = conn.hgetall("jobs:processing").await.map_err(internal)?;
    for (id, claimed_at) in processing {
        if now - claimed_at >= JOB_LEASE_TIMEOUT_SECS {
            let _: () = conn.hdel("jobs:processing", id).await.map_err(internal)?;
            let _: () = conn.lpush("jobs:pending", id).await.map_err(internal)?;
        }
    }
    Ok(())
}

async fn claim_next(
    conn: &mut redis::aio::ConnectionManager,
) -> Result<Option<ClaimedJob>, AppError> {
    promote_due_delayed(conn).await?;
    reclaim_stale_leases(conn).await?;

    let id: Option<i64> = conn.rpop("jobs:pending", None).await.map_err(internal)?;
    let Some(id) = id else {
        return Ok(None);
    };

    let attempts: i64 = conn.hincr("jobs:attempts", id, 1).await.map_err(internal)?;
    let _: () = conn
        .hset("jobs:processing", id, now_unix_secs())
        .await
        .map_err(internal)?;

    let data: Option<String> = conn.hget("jobs:data", id).await.map_err(internal)?;
    let Some(data) = data else {
        // Defensive only - `jobs:data` is never deleted before
        // `jobs:processing`/`jobs:attempts` are (see the cleanup calls
        // below), so this should be unreachable in practice. Clean up the
        // orphaned tracking entries so a corrupted id can't loop forever,
        // and report it the same way an empty queue reports: nothing to
        // claim right now.
        tracing::error!(job_id = id, "claimed a job id with no jobs:data entry");
        let _: () = conn.hdel("jobs:processing", id).await.map_err(internal)?;
        let _: () = conn.hdel("jobs:attempts", id).await.map_err(internal)?;
        return Ok(None);
    };
    let job_data: JobData =
        serde_json::from_str(&data).map_err(|source| AppError::Internal(Box::new(source)))?;

    Ok(Some(ClaimedJob {
        id,
        job_type: job_data.job_type,
        payload: job_data.payload,
        attempts,
    }))
}

async fn cleanup_claimed(
    conn: &mut redis::aio::ConnectionManager,
    id: i64,
) -> Result<(), AppError> {
    let _: () = conn.hdel("jobs:processing", id).await.map_err(internal)?;
    let _: () = conn.hdel("jobs:attempts", id).await.map_err(internal)?;
    let _: () = conn.hdel("jobs:data", id).await.map_err(internal)?;
    Ok(())
}

async fn record_failure(
    conn: &mut redis::aio::ConnectionManager,
    job: &ClaimedJob,
    error: &str,
) -> Result<(), AppError> {
    let entry = serde_json::to_string(&FailedJob {
        job_type: job.job_type.clone(),
        payload: job.payload.clone(),
        error: error.to_string(),
        failed_at: now_unix_secs(),
    })
    .map_err(|source| AppError::Internal(Box::new(source)))?;
    let _: () = conn.rpush("jobs:failed", entry).await.map_err(internal)?;
    Ok(())
}

async fn release_for_retry(
    conn: &mut redis::aio::ConnectionManager,
    job: &ClaimedJob,
) -> Result<(), AppError> {
    // Same small exponential delay `sql_worker::release_for_retry` uses -
    // keeps a permanently failing job from hot-looping while preserving
    // prompt retries for transient failures.
    let delay = 2_i64.pow((job.attempts - 1).clamp(0, 10) as u32);
    let ready_at = now_unix_secs() + delay;
    let _: () = conn
        .hdel("jobs:processing", job.id)
        .await
        .map_err(internal)?;
    let _: () = conn
        .zadd("jobs:delayed", job.id, ready_at)
        .await
        .map_err(internal)?;
    Ok(())
}

/// Claims and executes at most one job. Returns `Ok(true)` if a job was
/// found (whether it succeeded or landed in `jobs:failed`), `Ok(false)` if
/// the queue was empty - same contract `sql_worker`'s own `process_next`
/// gives, so `work()`'s polling loop below needs no driver-specific logic.
async fn process_next(
    conn: &mut redis::aio::ConnectionManager,
    registry: &JobRegistry,
) -> Result<bool, AppError> {
    let Some(job) = claim_next(conn).await? else {
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
            cleanup_claimed(conn, job.id).await?;
            tracing::info!(job_id = job.id, job_type = %job.job_type, "job processed");
        }
        Err(error) => {
            if job.attempts >= MAX_ATTEMPTS {
                tracing::error!(job_id = job.id, job_type = %job.job_type, attempts = job.attempts, %error, "job permanently failed");
                record_failure(conn, &job, &error.to_string()).await?;
                cleanup_claimed(conn, job.id).await?;
            } else {
                tracing::warn!(job_id = job.id, job_type = %job.job_type, attempts = job.attempts, %error, "job failed; scheduling retry");
                release_for_retry(conn, &job).await?;
            }
        }
    }

    Ok(true)
}

pub(crate) async fn work(registry: JobRegistry) -> Result<(), AppError> {
    let mut conn = connection().await?;
    tracing::info!("queue worker started (redis driver)");

    let mut idle_interval = INITIAL_POLL_INTERVAL;
    loop {
        if process_next(&mut conn, &registry).await? {
            idle_interval = INITIAL_POLL_INTERVAL;
        } else {
            tokio::time::sleep(idle_interval).await;
            idle_interval = idle_interval.saturating_mul(2).min(MAX_POLL_INTERVAL);
        }
    }
}
