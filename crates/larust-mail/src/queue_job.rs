//! The already-rendered form of an email, enqueued by
//! `MailBuilder::queue` - see that method's own doc comment for the full
//! design rationale (why the typed `Mailable` itself can't be queued, and
//! the deliberate deviation from Laravel's re-resolve-on-worker
//! semantics).

use crate::send::deliver;
use larust_core::AppError;
use larust_queue::Job;
use serde::{Deserialize, Serialize};

/// Framework-owned - fields are `pub(crate)`, so this is only ever built
/// by `MailBuilder::queue`, never constructed (or queued) directly by app
/// code. `registry.register::<larust_support::mail::MailJob>()` is a
/// real registration, no different from any app-defined `Job`'s - it's
/// just that `xr new`'s scaffold writes that line into every generated
/// app's `queue:work` branch by default, rather than leaving it as a
/// hint the app author must remember to add.
#[derive(Serialize, Deserialize)]
pub struct MailJob {
    pub(crate) to: Vec<String>,
    pub(crate) subject: String,
    pub(crate) html_body: String,
}

impl Job for MailJob {
    // `__larust_`-prefixed, matching this codebase's existing convention
    // for framework-owned internal identifiers (`/__larust_wire/...`,
    // `/__larust_push/{channel}`) - low collision risk against an app's
    // own hand-chosen `JOB_TYPE` strings.
    const JOB_TYPE: &'static str = "__larust_queued_mail";

    async fn handle(&self) -> Result<(), AppError> {
        deliver(&self.to, &self.subject, &self.html_body).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use larust_queue::{work, JobRegistry};
    use std::time::Duration;

    async fn connect_test_db() {
        let dir = tempfile::tempdir().unwrap().keep();
        let database_url = format!("sqlite://{}/test.sqlite", dir.display());
        larust_orm::connect(&database_url).await.unwrap();
    }

    /// `deliver`'s log-driver branch reads `larust_core::config()`, which
    /// panics if `Application::new()` was never called in this process.
    /// Safe to call here: an empty config value, so nothing pulls in
    /// unexpected settings - this defaults to `mail_driver = "log"` - no
    /// network ever touched by these tests.
    fn ensure_config() {
        let _ = larust_core::Application::new(|| serde_json::json!({}));
    }

    /// Both scenarios share one test function, not two: `larust_orm::
    /// connect()` sets a process-wide pool exactly once (a second call in
    /// the same test binary errors with "connect() called more than
    /// once"), the same singleton-per-process constraint `fake.rs`'s own
    /// tests document for `FAKE_SENT`. Each phase's own `work()` loop
    /// (which never returns on its own) is `.abort()`-ed before the next
    /// phase starts a different one against the same `jobs` table, so the
    /// two workers never race each other over the same row.
    #[tokio::test]
    async fn queued_mail_delivers_when_registered_and_fails_loudly_when_not() {
        ensure_config();
        connect_test_db().await;
        let pool = larust_orm::pool().unwrap();

        // Phase 1: a registered worker claims and delivers the job.
        larust_queue::dispatch(&MailJob {
            to: vec!["someone@example.test".to_string()],
            subject: "Hello".to_string(),
            html_body: "<p>Hi</p>".to_string(),
        })
        .await
        .unwrap();

        let registry = JobRegistry::new().register::<MailJob>();
        let worker = tokio::spawn(work(registry));
        // `work()` polls every 500ms when idle, but the job is already
        // present the moment it starts, so it's claimed on the very
        // first iteration - comfortably bounded well under that interval.
        tokio::time::sleep(Duration::from_millis(200)).await;
        worker.abort();

        let jobs: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM jobs")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(jobs.0, 0, "the queued mail job should have been claimed");

        let failed: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM failed_jobs")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(
            failed.0, 0,
            "a registered MailJob delivered via the log driver should never fail"
        );

        // Phase 2: an unregistered worker records "no handler" instead of
        // silently delivering - proves `.queue()`'d mail isn't magically
        // delivered without the app explicitly registering `MailJob`.
        larust_queue::dispatch(&MailJob {
            to: vec!["someone@example.test".to_string()],
            subject: "Hello".to_string(),
            html_body: "<p>Hi</p>".to_string(),
        })
        .await
        .unwrap();

        let registry = JobRegistry::new();
        let worker = tokio::spawn(work(registry));
        tokio::time::sleep(Duration::from_millis(200)).await;
        worker.abort();

        let failed: (String,) =
            sqlx::query_as("SELECT error FROM failed_jobs ORDER BY id DESC LIMIT 1")
                .fetch_one(pool)
                .await
                .unwrap();
        assert!(failed.0.contains("no handler registered"));
    }
}
