//! `Job` (shared, driver-agnostic) plus `dispatch()`'s runtime driver
//! dispatch — mirrors `larust_cache::store`'s own `match config
//! .cache_driver.as_str() { ... }` shape, itself modeled on
//! `larust_mail::send::deliver`'s `mail_driver` precedent.

use larust_core::AppError;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::future::Future;

/// A unit of deferred work. Implemented once per job type, the same
/// "app implements this once per thing" shape as `Policy<U>`/`Mailable`.
pub trait Job: Serialize + DeserializeOwned + Send + Sync + 'static {
    /// A stable, explicit, app-chosen tag — deliberately not
    /// `std::any::type_name::<Self>()`. That string isn't stable across a
    /// rename/refactor, and a row already sitting in the `jobs` table
    /// under the old name would silently stop matching any handler.
    const JOB_TYPE: &'static str;

    /// `-> impl Future<...> + Send` rather than a plain `async fn` — the
    /// exact spelling `larust_auth::Authenticatable::find_for_auth`
    /// already established (see `docs/GOTCHAS.md`) to avoid the
    /// async-fn-in-traits `Send`-propagation pitfall. Unlike `Mailable`'s
    /// methods, a job's `handle()` is inherently real async I/O (sending
    /// mail, calling an API, writing to the DB), so — unlike `Mailable` —
    /// there's no sidestepping this by staying synchronous.
    fn handle(&self) -> impl Future<Output = Result<(), AppError>> + Send;
}

/// Serializes `job` to JSON and enqueues it — durable the moment this
/// returns `Ok`, independent of whether any `xr queue:work` process is
/// currently running to pick it up.
pub async fn dispatch<J: Job>(job: &J) -> Result<(), AppError> {
    match crate::queue_driver() {
        "redis" => crate::redis_dispatch::dispatch(job).await,
        _ => crate::sql_dispatch::dispatch(job).await,
    }
}
