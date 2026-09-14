---
title: Events, Queues & Scheduling
parent: Digging Deeper
nav_order: 3
---

# Events, Queues & Scheduling
{: .no_toc }

1. TOC
{:toc}

## Events & Listeners

In-process, synchronous pub/sub - no persistence, no queue involved
unless a listener explicitly dispatches one. Any `Clone + Send + Sync +
'static` type is automatically an `Event` - there's no trait to
implement:

```rust
#[derive(Clone)]
pub struct PostCreated {
    pub post_id: i64,
    pub title: String,
    pub user_id: i64,
}
```

Register a listener once, at boot (before `serve()`):

```rust
larust_support::event::listeners()
    .on::<PostCreated, _, _>(|event: PostCreated| async move {
        larust_support::tracing::info!(post_id = event.post_id, "post created");
    });
```

Dispatch it from wherever the thing actually happened:

```rust
larust_support::event::dispatch(PostCreated {
    post_id: post.id, title: post.title.clone(), user_id: post.user_id,
}).await;
```

Every registered listener for that event type runs; there's no return
value and no way for a listener to cancel the event or affect the
dispatcher's own return.

## Queues

Durable, persisted jobs - dispatching survives even if nothing is
currently running to pick the job up. A `Job` is a serializable struct
with a stable type tag and a `handle()` method:

```rust
#[derive(Serialize, Deserialize)]
pub struct NotifyPostCreatedJob {
    pub post_id: i64,
}

impl Job for NotifyPostCreatedJob {
    const JOB_TYPE: &'static str = "notify_post_created";

    async fn handle(&self) -> Result<(), AppError> {
        larust_support::tracing::info!(post_id = self.post_id, "notifying");
        Ok(())
    }
}
```

```rust
larust_support::queue::dispatch(&NotifyPostCreatedJob { post_id: post.id }).await?;
```

`JOB_TYPE` is a stable, hand-chosen string - deliberately not
`std::any::type_name::<Self>()`, which isn't stable across a
rename/refactor. A row already sitting in the `jobs` table under the old
type name would silently stop matching any handler if this were left to
reflection.

```
# .env
QUEUE_DRIVER=database   # default - the same connection DB_CONNECTION points at
# QUEUE_DRIVER=redis    # REDIS_URL=redis://127.0.0.1:6379
```

### Running workers

```bash
xr queue:work
```

Claims and processes jobs until stopped. On the `database` driver, a job
is claimed via an atomic `DELETE ... RETURNING`-shaped race-safe write -
two worker processes racing for the same job, one wins, the other moves
on to the next. A failing job retries with exponential backoff up to a
fixed attempt count, then lands in a `failed_jobs` table rather than
disappearing silently.

### Registering job types

`main.rs`'s `queue:work` branch builds a `JobRegistry` naming every job
type your app dispatches:

```rust
let registry = larust_support::queue::JobRegistry::new()
    .register::<larust_support::mail::MailJob>()   // built in - powers Mail's .queue()
    .register::<crate::jobs::NotifyPostCreatedJob>();
larust_support::queue::work(registry).await
```

A job type dispatched but never registered here fails loudly (a real,
by-design gap, not a silent no-op) - the worker has no way to deserialize
a payload it doesn't know the shape of.

## Task Scheduling

`Schedule` is Laravel's own scheduling vocabulary - `routes/console.rs`
declares tasks, `xr schedule:work` runs whatever's due, once a second:

```rust
pub fn schedule() -> Schedule {
    Schedule::new()
        .daily(|| async {
            let count = Post::query().count().await?;
            larust_support::tracing::info!(count, "daily post count");
            Ok(())
        })
        .hourly(|| async { cleanup_expired_sessions().await })
        .cron("0 */15 * * * *", || async { poll_external_api().await })
}
```

`.every_minute(...)`/`.hourly(...)`/`.daily(...)`/`.daily_at("13:00",
...)`/`.weekly(...)`/`.monthly(...)`/`.cron(expr, ...)` - a task is a
plain closure, run in the same process that declared it (unlike a `Job`,
there's no serialization boundary, so it can close over anything).

{: .note }
`.cron(...)` uses the `cron` crate's own **7-field** dialect (seconds,
minutes, hours, day-of-month, month, day-of-week, year) - not Laravel's
classic 5-field Unix cron. An easy mismatch to assume away if you're
translating a Laravel schedule directly.

```bash
xr schedule:work
```

{: .warning }
**Not safe to run as more than one process against the same app** by
default - there's no coordination step at all, so two `schedule:work`
processes both run every due task, silently duplicating side effects.
Opt a specific task into cross-process safety explicitly:

```rust
Schedule::new()
    .name("send-weekly-digest")   // required before .on_one_server()
    .weekly(send_weekly_digest)
    .on_one_server()
```

The first process to win a race-safe `INSERT` for a given `(task name,
scheduled instant)` runs it; every other process racing for the same pair
loses and skips it. `.name(...)` is required first - a closure has no
identity of its own to key that row on.

## Next

[File Storage & Cache](../../digging-deeper/file-storage-and-cache).
