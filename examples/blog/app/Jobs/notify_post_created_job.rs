use larust_support::queue::Job;
use serde::{Deserialize, Serialize};

/// Enqueued by the `PostCreated` listener in `main.rs` — no real external
/// system touched, matching `WelcomeMail`'s `log` driver as "the safe,
/// zero-setup default that still exercises the real end-to-end path":
/// dispatch → a real row in the `jobs` table → `xr queue:work` claiming
/// and running it for real.
#[derive(Serialize, Deserialize)]
pub struct NotifyPostCreatedJob {
    pub post_id: i64,
}

impl Job for NotifyPostCreatedJob {
    const JOB_TYPE: &'static str = "notify_post_created";

    async fn handle(&self) -> Result<(), larust_support::AppError> {
        larust_support::tracing::info!(post_id = self.post_id, "notifying about new post");
        Ok(())
    }
}
