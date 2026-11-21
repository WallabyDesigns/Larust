//! Automatic, periodic app-level self-maintenance - distinct from `xr
//! clean` (which only cleans the local build cache, `target/`, not
//! anything inside a running app). Session expiry already cleans itself
//! (see `larust_http::session`'s own hourly background task) and deploy
//! release slots are already pruned after every `xr deploy` (`xr-cli`'s
//! own `release_slots::prune`) - neither needs anything from this module.

use larust_core::AppError;

/// Runs every self-clean maintenance task this framework ships: sweeping
/// expired cache rows ([`crate::cache::sweep_expired`]) and pruning old
/// failed-job history ([`crate::queue::prune_failed_jobs`]). Intended to be
/// registered as a recurring `larust_support::schedule` task - see the
/// scaffold template's own default `routes/console.rs` for the standard
/// registration every newly-generated app ships with.
///
/// Both sub-tasks run regardless of whether the other fails - a cache
/// hiccup shouldn't prevent failed-job pruning, or vice versa. Each
/// failure is logged individually before any error is propagated, so the
/// specific cause stays visible even though the scheduler's own `run_due`
/// already logs the task as a whole failing.
pub async fn clean() -> Result<(), AppError> {
    let cache_result = crate::cache::sweep_expired().await;
    if let Err(error) = &cache_result {
        tracing::warn!(%error, "self-clean: cache sweep failed");
    }

    let queue_result = crate::queue::prune_failed_jobs().await;
    if let Err(error) = &queue_result {
        tracing::warn!(%error, "self-clean: failed-job prune failed");
    }

    cache_result?;
    queue_result?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn connect_test_db() {
        let dir = tempfile::tempdir().unwrap().keep();
        let database_url = format!("sqlite://{}/test.sqlite", dir.display());
        larust_orm::connect(&database_url).await.unwrap();
    }

    #[tokio::test]
    async fn clean_succeeds_when_both_sub_tasks_succeed() {
        connect_test_db().await;

        crate::cache::put("some-key", &"value", std::time::Duration::from_secs(60))
            .await
            .unwrap();

        clean().await.unwrap();
    }
}
