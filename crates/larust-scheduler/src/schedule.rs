use chrono::{DateTime, Utc};
use larust_core::AppError;
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::time::Duration;

type BoxedTask =
    Box<dyn Fn() -> Pin<Box<dyn Future<Output = Result<(), AppError>> + Send>> + Send + Sync>;

/// A set of recurring tasks, declared once (typically inline in the
/// generated app's own `schedule:work` branch — see `docs/ARCHITECTURE.md`'s
/// "Scheduler" section) and handed to [`work`]. Consuming, `Self`-returning
/// builder — the same "build a registry, then run it" shape as
/// `larust_queue::JobRegistry`.
#[must_use]
pub struct Schedule {
    entries: Vec<(cron::Schedule, BoxedTask)>,
}

impl Default for Schedule {
    fn default() -> Self {
        Self::new()
    }
}

impl Schedule {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Escape hatch for any frequency the fluent methods below don't cover
    /// directly (e.g. `"0 */5 * * * * *"` for every 5 minutes). Uses the
    /// `cron` crate's own **7-field extended dialect** — seconds, minutes,
    /// hours, day-of-month, month, day-of-week, year — not Laravel's
    /// classic 5-field Unix cron format.
    ///
    /// Panics on an invalid expression: this is a startup-time
    /// configuration mistake, not a runtime condition — the same
    /// fail-loud-immediately precedent `larust_queue::JobRegistry::
    /// register`'s duplicate-`JOB_TYPE` panic already establishes, rather
    /// than surfacing as a confusing "the scheduler silently never runs
    /// this task" bug discovered much later.
    pub fn cron<F, Fut>(mut self, expr: &str, task: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), AppError>> + Send + 'static,
    {
        let parsed = cron::Schedule::from_str(expr).unwrap_or_else(|error| {
            panic!(
                "invalid cron expression {expr:?}: {error} -- Schedule::cron uses the `cron` \
                 crate's 7-field dialect (sec min hour day-of-month month day-of-week year), \
                 not the classic 5-field Unix cron format"
            )
        });
        self.entries
            .push((parsed, Box::new(move || Box::pin(task()))));
        self
    }

    /// Runs `task` once every minute, at second 0.
    pub fn every_minute<F, Fut>(self, task: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), AppError>> + Send + 'static,
    {
        self.cron("0 * * * * * *", task)
    }

    /// Runs `task` once every hour, on the hour.
    pub fn hourly<F, Fut>(self, task: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), AppError>> + Send + 'static,
    {
        self.cron("0 0 * * * * *", task)
    }

    /// Runs `task` once a day, at midnight UTC.
    pub fn daily<F, Fut>(self, task: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), AppError>> + Send + 'static,
    {
        self.cron("0 0 0 * * * *", task)
    }

    /// Runs `task` once a day at the given `"HH:MM"` UTC time. Panics on a
    /// malformed or out-of-range value — same fail-loud-at-startup
    /// precedent as [`Self::cron`]'s invalid-expression panic.
    pub fn daily_at<F, Fut>(self, time: &str, task: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), AppError>> + Send + 'static,
    {
        let (hour, minute) = parse_hh_mm(time);
        self.cron(&format!("0 {minute} {hour} * * * *"), task)
    }

    /// Runs `task` once a week, Sunday at midnight UTC.
    pub fn weekly<F, Fut>(self, task: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), AppError>> + Send + 'static,
    {
        self.cron("0 0 0 * * Sun *", task)
    }

    /// Runs `task` once a month, on the 1st at midnight UTC.
    pub fn monthly<F, Fut>(self, task: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), AppError>> + Send + 'static,
    {
        self.cron("0 0 0 1 * * *", task)
    }
}

fn parse_hh_mm(time: &str) -> (u32, u32) {
    let (hour_str, minute_str) = time.split_once(':').unwrap_or_else(|| {
        panic!("invalid time {time:?} for Schedule::daily_at -- expected \"HH:MM\"")
    });
    let hour: u32 = hour_str.parse().unwrap_or_else(|_| {
        panic!("invalid hour in {time:?} for Schedule::daily_at -- expected \"HH:MM\"")
    });
    let minute: u32 = minute_str.parse().unwrap_or_else(|_| {
        panic!("invalid minute in {time:?} for Schedule::daily_at -- expected \"HH:MM\"")
    });
    assert!(
        hour < 24,
        "hour out of range in {time:?} for Schedule::daily_at -- expected 00-23"
    );
    assert!(
        minute < 60,
        "minute out of range in {time:?} for Schedule::daily_at -- expected 00-59"
    );
    (hour, minute)
}

/// Runs every task in `schedule` whose cron expression matches `at`,
/// sequentially, in registration order. Not `pub` — directly unit-testable
/// against a fixed instant, no real clock/sleep involved, the same role
/// `larust_queue::worker::process_next` plays for the queue worker.
///
/// Sequential, not concurrent: a slow task delays a same-tick sibling
/// (and the next tick's own check, since `work()` awaits this whole call
/// before ticking again) — but this also means a task can never overlap
/// *with itself* across ticks for free, a safer default than
/// concurrent-by-default would be without an explicit
/// `withoutOverlapping()`-equivalent. A task returning `Err` is logged and
/// does not stop the others due this tick, matching both
/// `larust_events::dispatch`'s "a listener that can fail should log its
/// own error rather than short-circuit the others" and the queue worker's
/// "a failed job is recorded, the loop continues."
async fn run_due(schedule: &Schedule, at: DateTime<Utc>) {
    for (cron_schedule, task) in &schedule.entries {
        if !cron_schedule.includes(at) {
            continue;
        }
        match task().await {
            Ok(()) => {
                tracing::info!(cron = cron_schedule.source(), "scheduled task ran");
            }
            Err(error) => {
                tracing::error!(
                    cron = cron_schedule.source(),
                    %error,
                    "scheduled task failed"
                );
            }
        }
    }
}

const TICK_INTERVAL: Duration = Duration::from_secs(1);

/// Runs forever, checking every registered task against the current time
/// once a second (matching seconds — the `cron` crate's own native
/// precision, even though every fluent method above only offers
/// minute-or-coarser granularity) and running whichever are due. No direct
/// test exists for this function — the same posture `larust_queue::
/// worker::work`'s own doc comment takes; only `run_due` is tested.
///
/// Uses `MissedTickBehavior::Skip`: if a task blocks this loop for, say, 90
/// seconds, anything due during that window silently does not run — it is
/// **not** queued up and burst-fired afterward. This matches Laravel's own
/// `schedule:run` behavior, not just a Rust-idiom default: Laravel's own
/// scheduler is invoked once a minute by an external cron entry with no
/// catch-up mechanism either, if that invocation's own process is still
/// busy.
///
/// **Not safe to run as more than one process against the same app.**
/// Unlike `xr queue:work` (whose claim step is atomic under SQLite's
/// writer serialization, making multiple worker processes a supported
/// scaling story), this function has no claim/lock step at all — it just
/// checks an in-memory `Schedule` against the wall clock. Two
/// `xr schedule:work` processes watching the same app will both run every
/// due task, every time, silently duplicating side effects (e.g. sending
/// the same email twice) rather than sharing the work. A documented v1
/// gap — Laravel itself only solved this with `onOneServer()` well after
/// its own initial scheduler design — but a more consequential one than
/// most gaps in this codebase, since the failure mode is silent duplicate
/// side effects, not a crash or a missed run.
pub async fn work(schedule: Schedule) -> Result<(), AppError> {
    let mut interval = tokio::time::interval(TICK_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tracing::info!("scheduler worker started");
    loop {
        interval.tick().await;
        run_due(&schedule, Utc::now()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn schedule_is_send_and_sync() {
        assert_send_sync::<Schedule>();
    }

    #[test]
    fn every_minute_produces_the_expected_cron_source() {
        let schedule = Schedule::new().every_minute(|| async { Ok(()) });
        assert_eq!(schedule.entries[0].0.source(), "0 * * * * * *");
    }

    #[test]
    fn hourly_produces_the_expected_cron_source() {
        let schedule = Schedule::new().hourly(|| async { Ok(()) });
        assert_eq!(schedule.entries[0].0.source(), "0 0 * * * * *");
    }

    #[test]
    fn daily_produces_the_expected_cron_source() {
        let schedule = Schedule::new().daily(|| async { Ok(()) });
        assert_eq!(schedule.entries[0].0.source(), "0 0 0 * * * *");
    }

    #[test]
    fn weekly_produces_the_expected_cron_source() {
        let schedule = Schedule::new().weekly(|| async { Ok(()) });
        assert_eq!(schedule.entries[0].0.source(), "0 0 0 * * Sun *");
    }

    #[test]
    fn monthly_produces_the_expected_cron_source() {
        let schedule = Schedule::new().monthly(|| async { Ok(()) });
        assert_eq!(schedule.entries[0].0.source(), "0 0 0 1 * * *");
    }

    #[test]
    fn daily_at_parses_hh_mm_into_the_expected_cron_source() {
        let schedule = Schedule::new().daily_at("13:07", || async { Ok(()) });
        assert_eq!(schedule.entries[0].0.source(), "0 7 13 * * * *");
    }

    #[test]
    #[should_panic(expected = "invalid time")]
    fn daily_at_panics_on_a_malformed_time() {
        let _ = Schedule::new().daily_at("not-a-time", || async { Ok(()) });
    }

    #[test]
    #[should_panic(expected = "hour out of range")]
    fn daily_at_panics_on_an_out_of_range_hour() {
        let _ = Schedule::new().daily_at("24:00", || async { Ok(()) });
    }

    #[test]
    #[should_panic(expected = "invalid cron expression")]
    fn cron_panics_on_an_invalid_expression() {
        let _ = Schedule::new().cron("not a cron expression", || async { Ok(()) });
    }

    #[tokio::test]
    async fn run_due_fires_a_task_exactly_at_a_matching_instant_not_a_neighboring_one() {
        let matching_calls = Arc::new(AtomicUsize::new(0));
        let calls = Arc::clone(&matching_calls);
        let schedule = Schedule::new().cron("0 30 9 * * * *", move || {
            let calls = Arc::clone(&calls);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        });

        let matching = Utc.with_ymd_and_hms(2026, 8, 15, 9, 30, 0).unwrap();
        let not_matching = Utc.with_ymd_and_hms(2026, 8, 15, 9, 31, 0).unwrap();

        run_due(&schedule, not_matching).await;
        assert_eq!(matching_calls.load(Ordering::SeqCst), 0);

        run_due(&schedule, matching).await;
        assert_eq!(matching_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_failing_task_does_not_block_a_same_tick_sibling() {
        let good_calls = Arc::new(AtomicUsize::new(0));
        let calls = Arc::clone(&good_calls);

        let schedule = Schedule::new()
            .cron("0 0 12 * * * *", || async {
                Err(AppError::Internal(Box::new(std::io::Error::other(
                    "deliberate test failure",
                ))))
            })
            .cron("0 0 12 * * * *", move || {
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            });

        let at = Utc.with_ymd_and_hms(2026, 8, 15, 12, 0, 0).unwrap();
        run_due(&schedule, at).await;

        assert_eq!(
            good_calls.load(Ordering::SeqCst),
            1,
            "the second task should still have run despite the first failing"
        );
    }
}
