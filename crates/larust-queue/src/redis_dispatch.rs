//! The `"redis"` `queue_driver` implementation's dispatch half - see
//! [`crate::redis_worker`] for the claim/lease/retry state machine this
//! feeds. `dispatch.rs` dispatches to this module or
//! [`crate::sql_dispatch`] based on `Config::queue_driver`.

use crate::redis_conn::connection;
use crate::Job;
use larust_core::AppError;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};

/// The immutable content of a dispatched job - stored once in `jobs:data`,
/// keyed by id, and never rewritten (unlike the SQL version's `jobs` row,
/// which the claim step mutates in place; Redis tracks claim state
/// separately, in `jobs:processing`/`jobs:attempts` - see
/// `redis_worker`'s own doc comment).
#[derive(Serialize, Deserialize)]
pub(crate) struct JobData {
    pub(crate) job_type: String,
    pub(crate) payload: String,
}

/// Serializes `job` to JSON and enqueues it - durable the moment this
/// returns `Ok`, independent of whether any `xr queue:work` process is
/// currently running to pick it up. `jobs:next_id` (`INCR`) mirrors the
/// SQL version's `AUTOINCREMENT`/`AUTO_INCREMENT`/`GENERATED ALWAYS AS
/// IDENTITY` primary key.
pub(crate) async fn dispatch<J: Job>(job: &J) -> Result<(), AppError> {
    let payload =
        serde_json::to_string(job).map_err(|source| AppError::Internal(Box::new(source)))?;
    let data = serde_json::to_string(&JobData {
        job_type: J::JOB_TYPE.to_string(),
        payload,
    })
    .map_err(|source| AppError::Internal(Box::new(source)))?;

    let mut conn = connection().await?;
    let id: i64 = conn
        .incr("jobs:next_id", 1)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    let _: () = conn
        .hset("jobs:data", id, &data)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    // LPUSH (producer) + RPOP (consumer, in `redis_worker::claim_next`) is
    // a standard FIFO pair - oldest dispatched job claimed first, the same
    // ordering guarantee the SQL version's `ORDER BY id` gives.
    let _: () = conn
        .lpush("jobs:pending", id)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    Ok(())
}
