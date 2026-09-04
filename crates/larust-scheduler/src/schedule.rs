use chrono::{DateTime, Utc};
use larust_core::AppError;
use larust_orm::Backend;
use sqlx::AnyPool;
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::time::Duration;

type BoxedTask =
    Box<dyn Fn() -> Pin<Box<dyn Future<Output = Result<(), AppError>> + Send>> + Send + Sync>;

struct Entry {
    cron: cron::Schedule,
    task: BoxedTask,
    name: Option<String>,
    /// See [`Schedule::on_one_server`].
    on_one_server: bool,
}

/// A set of recurring tasks, declared once (typically inline in the
/// generated app's own `schedule:work` branch - see `docs/ARCHITECTURE.md`'s
/// "Scheduler" section) and handed to [`work`]. Consuming, `Self`-returning
/// builder - the same "build a registry, then run it" shape as
/// `larust_queue::JobRegistry`.
#[must_use]
pub struct Schedule {
    entries: Vec<Entry>,
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
    /// `cron` crate's own **7-field extended dialect** - seconds, minutes,
    /// hours, day-of-month, month, day-of-week, year - not Laravel's
    /// classic 5-field Unix cron format.
    ///
    /// Panics on an invalid expression: this is a startup-time
    /// configuration mistake, not a runtime condition - the same
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
        self.entries.push(Entry {
            cron: parsed,
            task: Box::new(move || Box::pin(task())),
            name: None,
            on_one_server: false,
        });
        self
    }

    /// Names the most recently registered task - same "applies to
    /// whatever was just registered" convention `larust_http::Router::
    /// name` already uses for routes. Required before
    /// [`Self::on_one_server`]: a task closure has no identity of its own
    /// to derive a stable cross-process lock key from (unlike
    /// `larust_queue::Job::JOB_TYPE`, a compile-time constant on a real
    /// type) - matching Laravel's own requirement that a closure-based
    /// scheduled task be named before `->onOneServer()` can be used.
    ///
    /// # Panics
    /// If called before registering any task at all.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.entries
            .last_mut()
            .expect("Schedule::name called before registering any task")
            .name = Some(name.into());
        self
    }

    /// Marks the most recently registered task so at most one `xr
    /// schedule:work` process actually runs it for each due occurrence,
    /// even when multiple processes are running against the same app
    /// (e.g. a rolling deploy's brief overlap) - Laravel's own
    /// `->onOneServer()`. Backed by a small self-bootstrapping
    /// `scheduler_locks` table (no migration file, same lazy `CREATE
    /// TABLE IF NOT EXISTS` idiom `larust-notifications`'s `ensure_table`
    /// establishes): the first process whose claim attempt for a given
    /// `(task name, due instant)` pair wins an `INSERT` actually runs the
    /// task; every other process racing for the same pair loses the
    /// `INSERT` to a unique-constraint violation and skips it - the same
    /// "claim by winning a race-safe write, treat losing as a normal
    /// no-op" shape `larust_queue::sql_worker::claim_next` already uses
    /// for job claiming, not an advisory-lock/session-primitive approach
    /// (SQLite has no equivalent to Postgres's `pg_advisory_lock`/MySQL's
    /// `GET_LOCK()` at all, so a plain row claim is the only mechanism
    /// portable across every backend this framework supports).
    ///
    /// A task NOT marked this way keeps today's original behavior
    /// unchanged: whichever process's own tick lands on a due instant
    /// just runs it, no coordination, no database dependency added -
    /// still correct for a genuinely single-process deployment, which
    /// remains the default.
    ///
    /// A claim-attempt database error is treated as "don't run this
    /// occurrence," not "run it anyway": the documented failure mode
    /// this mechanism exists to prevent (silent duplicate side effects)
    /// is worse than an occasional missed run under a database outage.
    ///
    /// # Panics
    /// If the most recently registered task has no name yet (see
    /// [`Self::name`]) - the same fail-loud-at-registration-time
    /// precedent every other `Schedule` builder method already
    /// establishes for a startup-time configuration mistake.
    pub fn on_one_server(mut self) -> Self {
        let entry = self
            .entries
            .last_mut()
            .expect("Schedule::on_one_server called before registering any task");
        assert!(
            entry.name.is_some(),
            "Schedule::on_one_server requires .name(...) first -- a task closure has no \
             identity of its own to derive a cross-process lock key from"
        );
        entry.on_one_server = true;
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
    /// malformed or out-of-range value - same fail-loud-at-startup
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

/// How long a claimed `scheduler_locks` row is kept before a later claim
/// attempt for the same task prunes it - bounds the table's growth for a
/// frequent `.on_one_server()` task (e.g. `.every_minute()`) rather than
/// letting it accumulate one row per occurrence forever. Pruning is
/// scoped to the task's own rows (uses the composite primary key's
/// leading column) and runs opportunistically, right before that same
/// task's next claim attempt - there is no separate maintenance job.
const LOCK_RETENTION_SECS: i64 = 7 * 24 * 60 * 60;

/// Creates the `scheduler_locks` table if it doesn't exist yet - no
/// migration file, plain `CREATE TABLE IF NOT EXISTS`, matching
/// `larust-notifications`'s `ensure_table`. Deliberately **not**
/// memoized behind a process-wide "already ensured" flag the way
/// `larust_queue::ensure_tables` is: that memoization only pays off for
/// a hot-path call, and this only runs when an `.on_one_server()` task
/// is actually due (rare - hourly/daily by far the common case), while a
/// memoized flag would risk the exact regression `larust-notifications`'s
/// own doc comment describes - a flag set by one test's database
/// surviving into a later test against a *different* one.
async fn ensure_lock_table(pool: &AnyPool) -> Result<(), AppError> {
    let create_table = match larust_orm::backend() {
        Backend::Sqlite => {
            "CREATE TABLE IF NOT EXISTS scheduler_locks (\
                task_name TEXT NOT NULL, \
                scheduled_for_unix INTEGER NOT NULL, \
                PRIMARY KEY (task_name, scheduled_for_unix)\
             )"
        }
        // `VARCHAR`, not `TEXT` - same `sqlx::Any`-driver decode gap
        // `larust_queue::ensure_tables`'s own doc comment documents in
        // full for MySQL specifically (Postgres/SQLite have no such
        // gap).
        Backend::MySql => {
            "CREATE TABLE IF NOT EXISTS scheduler_locks (\
                task_name VARCHAR(255) NOT NULL, \
                scheduled_for_unix BIGINT NOT NULL, \
                PRIMARY KEY (task_name, scheduled_for_unix)\
             )"
        }
        Backend::Postgres => {
            "CREATE TABLE IF NOT EXISTS scheduler_locks (\
                task_name TEXT NOT NULL, \
                scheduled_for_unix BIGINT NOT NULL, \
                PRIMARY KEY (task_name, scheduled_for_unix)\
             )"
        }
    };
    sqlx::query(create_table)
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    Ok(())
}

/// Attempts to claim `task_name`'s occurrence due at `at` - `Ok(true)` if
/// this call won the claim (the caller should run the task), `Ok(false)`
/// if another process already claimed this exact `(task_name, at)` pair
/// first (the caller should skip it). See [`Schedule::on_one_server`]'s
/// own doc comment for the full design rationale.
async fn claim(task_name: &str, at: DateTime<Utc>) -> Result<bool, AppError> {
    let pool = larust_orm::pool()?;
    ensure_lock_table(pool).await?;

    let backend = larust_orm::backend();
    let scheduled_for = at.timestamp();

    let prune_sql = format!(
        "DELETE FROM scheduler_locks WHERE task_name = {} AND scheduled_for_unix < {}",
        larust_orm::placeholder(backend, 1),
        larust_orm::placeholder(backend, 2),
    );
    sqlx::query(&prune_sql)
        .bind(task_name)
        .bind(scheduled_for - LOCK_RETENTION_SECS)
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    let claim_sql = format!(
        "INSERT INTO scheduler_locks (task_name, scheduled_for_unix) VALUES ({}, {})",
        larust_orm::placeholder(backend, 1),
        larust_orm::placeholder(backend, 2),
    );
    match sqlx::query(&claim_sql)
        .bind(task_name)
        .bind(scheduled_for)
        .execute(pool)
        .await
    {
        Ok(_) => Ok(true),
        // A losing claimer hits this exact task+occurrence's composite
        // primary key already existing - the normal "someone else got
        // there first" outcome, not an error.
        Err(sqlx::Error::Database(database)) if database.is_unique_violation() => Ok(false),
        Err(source) => Err(AppError::Internal(Box::new(source))),
    }
}

/// Runs every task in `schedule` whose cron expression matches `at`,
/// sequentially, in registration order. Not `pub` - directly unit-testable
/// against a fixed instant, no real clock/sleep involved, the same role
/// `larust_queue::worker::process_next` plays for the queue worker.
///
/// Sequential, not concurrent: a slow task delays a same-tick sibling
/// (and the next tick's own check, since `work()` awaits this whole call
/// before ticking again) - but this also means a task can never overlap
/// *with itself* across ticks for free, a safer default than
/// concurrent-by-default would be without an explicit
/// `withoutOverlapping()`-equivalent. A task returning `Err` is logged and
/// does not stop the others due this tick, matching both
/// `larust_events::dispatch`'s "a listener that can fail should log its
/// own error rather than short-circuit the others" and the queue worker's
/// "a failed job is recorded, the loop continues."
async fn run_due(schedule: &Schedule, at: DateTime<Utc>) {
    for entry in &schedule.entries {
        if !entry.cron.includes(at) {
            continue;
        }

        if entry.on_one_server {
            // Guaranteed `Some` by `Schedule::on_one_server`'s own
            // registration-time assertion.
            let name = entry.name.as_deref().unwrap();
            match claim(name, at).await {
                Ok(true) => {}
                Ok(false) => {
                    tracing::info!(
                        task = name,
                        "scheduled task already claimed by another process this occurrence, \
                         skipping"
                    );
                    continue;
                }
                Err(error) => {
                    tracing::error!(
                        task = name,
                        %error,
                        "failed to claim scheduled task lock, skipping this occurrence"
                    );
                    continue;
                }
            }
        }

        match (entry.task)().await {
            Ok(()) => {
                tracing::info!(cron = entry.cron.source(), name = ?entry.name, "scheduled task ran");
            }
            Err(error) => {
                tracing::error!(
                    cron = entry.cron.source(),
                    name = ?entry.name,
                    %error,
                    "scheduled task failed"
                );
            }
        }
    }
}

const TICK_INTERVAL: Duration = Duration::from_secs(1);

/// Runs forever, checking every registered task against the current time
/// once a second (matching seconds - the `cron` crate's own native
/// precision, even though every fluent method above only offers
/// minute-or-coarser granularity) and running whichever are due. No direct
/// test exists for this function - the same posture `larust_queue::
/// worker::work`'s own doc comment takes; only `run_due` is tested.
///
/// Uses `MissedTickBehavior::Skip`: if a task blocks this loop for, say, 90
/// seconds, anything due during that window silently does not run - it is
/// **not** queued up and burst-fired afterward. This matches Laravel's own
/// `schedule:run` behavior, not just a Rust-idiom default: Laravel's own
/// scheduler is invoked once a minute by an external cron entry with no
/// catch-up mechanism either, if that invocation's own process is still
/// busy.
///
/// **Running more than one process against the same app is safe only for
/// tasks explicitly marked [`Schedule::on_one_server`].** Any other task
/// still has no claim/lock step at all - it just checks an in-memory
/// `Schedule` against the wall clock, so two `xr schedule:work` processes
/// watching the same app both run every one of *those* due tasks, every
/// time, silently duplicating side effects (e.g. sending the same email
/// twice). This is the same shape Laravel itself shipped: its scheduler
/// ran single-process-only for a long time before `->onOneServer()`
/// existed, and even now a task must opt in explicitly - nothing makes
/// this safe by default, in either framework, because a coordinated
/// claim costs a database round trip a genuinely single-process
/// deployment (still the common case) shouldn't have to pay for tasks
/// that don't need it.
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
        assert_eq!(schedule.entries[0].cron.source(), "0 * * * * * *");
    }

    #[test]
    fn hourly_produces_the_expected_cron_source() {
        let schedule = Schedule::new().hourly(|| async { Ok(()) });
        assert_eq!(schedule.entries[0].cron.source(), "0 0 * * * * *");
    }

    #[test]
    fn daily_produces_the_expected_cron_source() {
        let schedule = Schedule::new().daily(|| async { Ok(()) });
        assert_eq!(schedule.entries[0].cron.source(), "0 0 0 * * * *");
    }

    #[test]
    fn weekly_produces_the_expected_cron_source() {
        let schedule = Schedule::new().weekly(|| async { Ok(()) });
        assert_eq!(schedule.entries[0].cron.source(), "0 0 0 * * Sun *");
    }

    #[test]
    fn monthly_produces_the_expected_cron_source() {
        let schedule = Schedule::new().monthly(|| async { Ok(()) });
        assert_eq!(schedule.entries[0].cron.source(), "0 0 0 1 * * *");
    }

    #[test]
    fn daily_at_parses_hh_mm_into_the_expected_cron_source() {
        let schedule = Schedule::new().daily_at("13:07", || async { Ok(()) });
        assert_eq!(schedule.entries[0].cron.source(), "0 7 13 * * * *");
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

    #[test]
    #[should_panic(expected = "requires .name(...) first")]
    fn on_one_server_panics_without_a_name_first() {
        let _ = Schedule::new().daily(|| async { Ok(()) }).on_one_server();
    }

    #[test]
    #[should_panic(expected = "called before registering any task")]
    fn name_panics_before_any_task_is_registered() {
        let _ = Schedule::new().name("too-early");
    }

    #[test]
    fn name_and_on_one_server_apply_to_the_most_recently_registered_task() {
        let schedule = Schedule::new()
            .daily(|| async { Ok(()) })
            .hourly(|| async { Ok(()) })
            .name("hourly-task")
            .on_one_server();

        assert_eq!(schedule.entries[0].name, None);
        assert!(!schedule.entries[0].on_one_server);
        assert_eq!(schedule.entries[1].name.as_deref(), Some("hourly-task"));
        assert!(schedule.entries[1].on_one_server);
    }

    // Connects once (`larust_orm::connect` errors on a second call in the
    // same process - see `larust_queue::sql_worker`'s own `connect_test_db`
    // for the identical constraint) and exercises every `claim`/`.
    // on_one_server` scenario sequentially against that one database,
    // matching `sql_worker.rs`'s own established workaround for this
    // limitation rather than trying to split into several independently
    // DB-connected test functions.
    #[tokio::test]
    async fn on_one_server_claims_exactly_once_per_task_and_occurrence() {
        let dir = tempfile::tempdir().unwrap().keep();
        let database_url = format!("sqlite://{}/test.sqlite", dir.display());
        larust_orm::connect(&database_url).await.unwrap();

        let at = Utc.with_ymd_and_hms(2026, 8, 15, 9, 30, 0).unwrap();
        let later = Utc.with_ymd_and_hms(2026, 8, 15, 10, 30, 0).unwrap();

        // First claimer for a given (task, occurrence) wins.
        assert!(claim("report-task", at).await.unwrap());
        // A second claim attempt for the exact same (task, occurrence)
        // loses - this is the whole mechanism: a losing claimer must see
        // `Ok(false)`, not an error, since losing a race is the expected,
        // routine outcome for every process but the first.
        assert!(!claim("report-task", at).await.unwrap());
        // A different task at the same instant is unaffected - the lock
        // key is the pair, not just the timestamp.
        assert!(claim("other-task", at).await.unwrap());
        // The same task at a different occurrence is unaffected either -
        // the lock key is the pair, not just the task name.
        assert!(claim("report-task", later).await.unwrap());

        // End-to-end through `run_due`: an `.on_one_server()` task whose
        // occurrence is already claimed does not run; one that hasn't
        // been claimed yet does.
        let already_claimed_calls = Arc::new(AtomicUsize::new(0));
        let calls = Arc::clone(&already_claimed_calls);
        let unclaimed_calls = Arc::new(AtomicUsize::new(0));
        let calls2 = Arc::clone(&unclaimed_calls);

        let schedule = Schedule::new()
            .cron("0 0 11 * * * *", move || {
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            })
            .name("report-task")
            .on_one_server()
            .cron("0 0 11 * * * *", move || {
                let calls = Arc::clone(&calls2);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            })
            .name("fresh-task")
            .on_one_server();

        let eleven = Utc.with_ymd_and_hms(2026, 8, 15, 11, 0, 0).unwrap();
        claim("report-task", eleven).await.unwrap(); // pre-claim by "another process"
        run_due(&schedule, eleven).await;

        assert_eq!(
            already_claimed_calls.load(Ordering::SeqCst),
            0,
            "a task whose occurrence was already claimed elsewhere must not run"
        );
        assert_eq!(
            unclaimed_calls.load(Ordering::SeqCst),
            1,
            "a task whose occurrence was not yet claimed must run exactly once"
        );
    }
}
