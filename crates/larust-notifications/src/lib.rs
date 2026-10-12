//! Durable, per-notifiable, read-tracked notifications - Laravel's
//! *database* notification channel - plus a compile-time-checked,
//! multi-channel delivery builder ([`to`]) covering the database and
//! mail channels together. Not Laravel's full `Notification`/`via()`/
//! `toMail()`/`toDatabase()`/`toBroadcast()` shape, and deliberately so.
//!
//! Laravel's `Notification` has *optional* per-channel render methods,
//! decided at runtime by `via($notifiable)`. There's no clean way to
//! express "this trait method is conditionally required based on another
//! method's runtime return value" in Rust without `Option`-returning
//! defaults - and this codebase's closest sibling traits (`Mailable`,
//! `larust_queue::Job`, `larust_auth::Authenticatable`) are all
//! zero-default-method traits, deliberately, specifically to force a
//! compile error on a real gap rather than a silently-never-implemented
//! one. Building Laravel's full shape here would be the first trait in
//! this codebase to break that convention - so [`to`]'s builder doesn't
//! decide channels at runtime from the notification itself either. Each
//! channel is opted into *explicitly*, at the call site
//! (`.database().mail()`), and each of those methods carries its own
//! real trait bound (`N: Notification` for `.database()`; `N:
//! MailNotification, U: MailNotifiable` for `.mail()`) - asking for a
//! channel a notification/notifiable doesn't actually implement is a
//! compile error pointing at the missing `impl`, not a channel silently
//! skipped at runtime:
//!
//! ```ignore
//! notification::to(&user, &InvoiceSent { invoice_id })
//!     .database()
//!     .mail()
//!     .send()
//!     .await?;
//! ```
//!
//! No broadcast channel yet - `larust-live::push` (`push::broadcast(
//! channel, html)`) already fully solves "push a live update"
//! independently, and it's already synchronous and infallible (see its
//! own doc comment), so there's nothing this builder would add by
//! wrapping it; call it alongside, same as before:
//!
//! ```ignore
//! notification::to(&user, &InvoiceSent { invoice_id }).database().mail().send().await?;
//! push::broadcast(&format!("notifications.{}", user.auth_id()), ...);
//! ```
//!
//! [`notify`] and [`notify_and_mail`] (the original, narrower two-channel
//! facade - [`notify`] and `mail()...send()` run *concurrently* via
//! `tokio::try_join!`, unlike [`to`]'s own sequential-per-channel
//! `send()`) both still exist unchanged, for existing callers.
//!
//! Re-exported through `larust_support::notification` (see
//! `crates/larust-support/src/lib.rs`) so generated apps depend only on
//! `larust-support`, never on this crate directly.

use larust_auth::{authorize, Authenticatable};
use larust_core::AppError;
use larust_mail::Mailable;
use larust_orm::Backend;
use serde::Serialize;
use sqlx::AnyPool;
use std::future::Future;
use std::pin::Pin;
use std::time::{SystemTime, UNIX_EPOCH};

/// A durable fact worth recording against a notifiable - Laravel's
/// `Notification` class, narrowed to just its database-channel shape.
/// Serializing `Self` *is* the stored `data` payload; there's no separate
/// render step the way [`Notification::to_database`] would imply if it
/// existed. `NOTIFICATION_TYPE` is the same explicit, stable, app-chosen
/// tag convention `larust_queue::Job::JOB_TYPE` already establishes -
/// deliberately not `std::any::type_name::<Self>()`, since that string
/// isn't stable across a rename and an already-stored row would silently
/// stop being attributable to its type.
///
/// No `DeserializeOwned` bound (unlike `Job`): nothing in this crate ever
/// reconstructs a concrete `Self` from a stored row - [`notifications_for`]
/// reads heterogeneous rows back across many different notification types
/// at once and can only sensibly return the type tag plus raw JSON,
/// matching Laravel's own `type`/`data` column split.
pub trait Notification: Serialize + Send + Sync {
    const NOTIFICATION_TYPE: &'static str;
}

/// One stored row, as read back by [`notifications_for`]. `data` stays a
/// raw [`serde_json::Value`] rather than a concrete type - a single query
/// reads rows for many different [`Notification`] types at once; the app
/// is expected to match on `notification_type` to interpret `data` if it
/// needs more than generic display.
#[derive(Debug, Clone)]
pub struct StoredNotification {
    pub id: i64,
    pub notification_type: String,
    pub data: serde_json::Value,
    pub read_at: Option<i64>,
    pub created_at: i64,
}

/// Same lazy self-bootstrap idiom `larust-cache`'s `cache_items` and
/// `larust-queue`'s `jobs`/`failed_jobs` establish - a plain
/// `CREATE TABLE IF NOT EXISTS`, no migration file and no explicit
/// startup call needed anywhere. Unlike either of those tables, this one
/// is genuinely filtered and sorted by a foreign-key-shaped column
/// (`notifiable_id`) at read time, so it also creates a matching index -
/// the first framework-owned table in this codebase that needs one.
///
/// Deliberately **not** memoized behind a `OnceCell` the way a first draft
/// of this function was: a `static` completion flag is process-wide, but
/// `larust_testing::test_transaction` swaps in a fresh, isolated database
/// *per test* within the same process - a real regression this shipped
/// with and a later test suite caught (a page exercising `unread_count`
/// for the first time started failing with "no such table: notifications"
/// once an *earlier* test in the same binary had already flipped the flag
/// against its own, different, since-discarded database). `IF NOT EXISTS`
/// makes re-running this on every call cheap enough in practice - a
/// schema lookup SQLite already has to do, no data scan - that giving up
/// the memoization is a better trade than reintroducing that failure mode.
async fn ensure_table(pool: &AnyPool) -> Result<(), AppError> {
    let create_table = match larust_orm::backend() {
        Backend::Sqlite => {
            "CREATE TABLE IF NOT EXISTS notifications (\
                id INTEGER PRIMARY KEY AUTOINCREMENT, \
                notifiable_id INTEGER NOT NULL, \
                notification_type TEXT NOT NULL, \
                data TEXT NOT NULL, \
                read_at INTEGER, \
                created_at INTEGER NOT NULL\
             )"
        }
        // `VARCHAR`, not MySQL's own `TEXT` - confirmed empirically (a
        // real, live MySQL server) that `sqlx`'s `Any` driver maps every
        // MySQL `TEXT`-family column to its own generic `Blob` kind
        // unconditionally, and `Decode<Any> for String` only accepts
        // `Text`-kind values - so a `TEXT` column here would fail to
        // decode back as `String` at all (only `CHAR`/`VARCHAR` map to
        // `Any`'s `Text` kind; see `larust-http::session`'s own
        // `AnySessionStore::migrate` for the same finding in more detail).
        // `data` gets a generous cap (`VARCHAR(4000)`) rather than an
        // arbitrary-size one - a real, documented trade-off for a large
        // notification payload.
        Backend::MySql => {
            "CREATE TABLE IF NOT EXISTS notifications (\
                id INTEGER PRIMARY KEY AUTO_INCREMENT, \
                notifiable_id INTEGER NOT NULL, \
                notification_type VARCHAR(255) NOT NULL, \
                data VARCHAR(4000) NOT NULL, \
                read_at INTEGER, \
                created_at INTEGER NOT NULL\
             )"
        }
        // Postgres has native, unbounded `TEXT` - same shape as SQLite's
        // own arm.
        Backend::Postgres => {
            "CREATE TABLE IF NOT EXISTS notifications (\
                id INTEGER GENERATED ALWAYS AS IDENTITY PRIMARY KEY, \
                notifiable_id INTEGER NOT NULL, \
                notification_type TEXT NOT NULL, \
                data TEXT NOT NULL, \
                read_at INTEGER, \
                created_at INTEGER NOT NULL\
             )"
        }
    };
    sqlx::query(create_table)
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    // MySQL's `CREATE INDEX` has no `IF NOT EXISTS` clause (unlike its
    // `CREATE TABLE IF NOT EXISTS`, which is standard) - this function
    // runs on every call (see its own doc comment on why it's
    // deliberately not memoized), so on MySQL every call after the first
    // hits and tolerates the "already exists" error instead.
    match larust_orm::backend() {
        Backend::Sqlite | Backend::Postgres => {
            sqlx::query(
                "CREATE INDEX IF NOT EXISTS idx_notifications_notifiable \
                 ON notifications (notifiable_id, created_at DESC)",
            )
            .execute(pool)
            .await
            .map_err(|source| AppError::Internal(Box::new(source)))?;
        }
        Backend::MySql => {
            if let Err(error) = sqlx::query(
                "CREATE INDEX idx_notifications_notifiable \
                 ON notifications (notifiable_id, created_at DESC)",
            )
            .execute(pool)
            .await
            {
                let duplicate_key = matches!(&error, sqlx::Error::Database(database)
                    if database.message().contains("Duplicate key name"));
                if !duplicate_key {
                    return Err(AppError::Internal(Box::new(error)));
                }
            }
        }
    }

    Ok(())
}

fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_secs() as i64
}

/// Records `notification` against `notifiable` - Laravel's
/// `$user->notify(new InvoiceSent($invoice))`, narrowed to the database
/// channel (see this crate's own doc comment for why mail/broadcast
/// aren't wrapped here).
pub async fn notify<U: Authenticatable, N: Notification>(
    notifiable: &U,
    notification: &N,
) -> Result<(), AppError> {
    let pool = larust_orm::pool()?;
    ensure_table(pool).await?;

    let data = serde_json::to_string(notification)
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    let backend = larust_orm::backend();
    let insert_sql = format!(
        "INSERT INTO notifications (notifiable_id, notification_type, data, created_at) \
         VALUES ({}, {}, {}, {})",
        larust_orm::placeholder(backend, 1),
        larust_orm::placeholder(backend, 2),
        larust_orm::placeholder(backend, 3),
        larust_orm::placeholder(backend, 4),
    );
    sqlx::query(&insert_sql)
        .bind(notifiable.auth_id())
        .bind(N::NOTIFICATION_TYPE)
        .bind(data)
        .bind(now_unix_secs())
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    Ok(())
}

/// Runs [`notify`] and `mail().to(email).send(mailable)` concurrently -
/// sugar for the common "record it and email it" case, not a new
/// dispatch mechanism (see this crate's own module doc comment). Both
/// calls still exist, still fully typed, still independently callable on
/// their own; this only removes the sequencing cost of awaiting them one
/// after the other, via `tokio::try_join!`. Fails with whichever error
/// occurs first if either side fails - the other side is not rolled
/// back, matching this crate's own `notify`'s "one plain `INSERT`, no
/// transaction" posture (a partially-delivered notification, e.g.
/// recorded but not emailed, is treated the same accepted-tradeoff way
/// `larust-mail`'s own `.queue()` already treats a similar gap).
pub async fn notify_and_mail<U, N, M>(
    notifiable: &U,
    notification: &N,
    email: &str,
    mailable: M,
) -> Result<(), AppError>
where
    U: Authenticatable + Sync,
    N: Notification + Sync,
    M: Mailable,
{
    tokio::try_join!(
        notify(notifiable, notification),
        larust_mail::mail().to(email).send(mailable),
    )?;
    Ok(())
}

/// A notification that also knows how to render itself as mail -
/// Laravel's `Notification::toMail()`, as a real trait method rather
/// than a runtime-dispatched one (see this crate's own module doc
/// comment for why). `Mail: Send + 'static` isn't this trait being
/// stricter than [`Mailable`] itself needs - it's what lets
/// [`PendingNotification::mail`] box the delivery future as `dyn Future
/// <...> + Send` regardless of what a specific `Mailable` impl happens
/// to capture.
pub trait MailNotification: Send + Sync {
    type Mail: Mailable + Send + 'static;
    fn to_mail(&self) -> Self::Mail;
}

/// A notifiable that knows its own mail address - Laravel's
/// `routeNotificationForMail()`, narrowed to the common case (one,
/// always-available address) rather than Laravel's full per-notification
/// override mechanism.
pub trait MailNotifiable: Authenticatable {
    fn notification_email(&self) -> String;
}

/// One channel's delivery, already built and type-erased - see
/// [`PendingNotification`]'s own doc comment for why this is what makes
/// `.database()`/`.mail()`/`.send()` work as three independently-typed
/// steps instead of one call needing every channel's bounds at once.
type BoxedDelivery<'a> = Pin<Box<dyn Future<Output = Result<(), AppError>> + Send + 'a>>;

/// A multi-channel notification being assembled - build with [`to`], add
/// channels with `.database()`/`.mail()`, run them with `.send()`. Each
/// channel method builds and immediately boxes its own delivery future
/// *while* its own bound (`N: Notification`, or `N: MailNotification, U:
/// MailNotifiable`) is known, erasing it to a plain `BoxedDelivery<'a>`
/// before storing it - which is what lets `.send()` itself need no bound
/// on `U`/`N` at all, regardless of how many or which channels were
/// actually selected.
pub struct PendingNotification<'a, U, N> {
    notifiable: &'a U,
    notification: &'a N,
    channels: Vec<BoxedDelivery<'a>>,
}

/// Starts building a multi-channel delivery for `notification`, aimed at
/// `notifiable` - see [`PendingNotification`]'s own doc comment.
pub fn to<'a, U, N>(notifiable: &'a U, notification: &'a N) -> PendingNotification<'a, U, N> {
    PendingNotification {
        notifiable,
        notification,
        channels: Vec::new(),
    }
}

impl<'a, U, N> PendingNotification<'a, U, N> {
    /// Adds the database channel - Laravel's `toDatabase()`, via
    /// [`notify`].
    #[must_use]
    pub fn database(mut self) -> Self
    where
        U: Authenticatable,
        N: Notification,
    {
        self.channels
            .push(Box::pin(notify(self.notifiable, self.notification)));
        self
    }

    /// Adds the mail channel - Laravel's `toMail()`, via
    /// [`MailNotification::to_mail`] and [`MailNotifiable::
    /// notification_email`]. The boxed future captures only the owned
    /// email string and the owned `Mail` value it builds right away, not
    /// a borrow of `self` - so its own `Send`-ness depends only on
    /// `MailNotification::Mail: Send`, already required by that trait.
    #[must_use]
    pub fn mail(mut self) -> Self
    where
        U: MailNotifiable,
        N: MailNotification,
    {
        let email = self.notifiable.notification_email();
        let mailable = self.notification.to_mail();
        self.channels.push(Box::pin(async move {
            larust_mail::mail().to(email).send(mailable).await
        }));
        self
    }

    /// Runs every selected channel, in the order they were chosen,
    /// sequentially - not concurrently the way [`notify_and_mail`]'s
    /// fixed two channels are, since an arbitrary-length list of
    /// channels has no single natural `tokio::try_join!`-style shape,
    /// and sequential delivery keeps which channel a failure came from
    /// unambiguous. No channel selected at all is logged and treated as
    /// `Ok(())`, not a silent no-op left completely unremarked - the
    /// same "nothing to do, but say so" posture elsewhere in this
    /// framework reserves for a genuinely empty operation.
    pub async fn send(self) -> Result<(), AppError> {
        if self.channels.is_empty() {
            tracing::warn!(
                "notification::to(...).send() called with no channel selected \
                 (.database()/.mail()) - nothing was sent"
            );
            return Ok(());
        }
        for channel in self.channels {
            channel.await?;
        }
        Ok(())
    }
}

/// Reads `notifiable`'s own notifications, newest first, capped at
/// `limit` rows. `limit` is caller-supplied rather than a framework-picked
/// constant - the same real precedent `larust_orm::QueryBuilder::
/// paginate(per_page)` already sets in this crate family - making an
/// unbounded query structurally impossible rather than merely
/// policy-discouraged. No cursor/`before_id` pagination in v1, the same
/// documented gap `paginate` itself carries.
pub async fn notifications_for<U: Authenticatable>(
    notifiable: &U,
    limit: i64,
) -> Result<Vec<StoredNotification>, AppError> {
    let pool = larust_orm::pool()?;
    ensure_table(pool).await?;

    let backend = larust_orm::backend();
    let select_sql = format!(
        "SELECT id, notification_type, data, read_at, created_at FROM notifications \
         WHERE notifiable_id = {} ORDER BY created_at DESC, id DESC LIMIT {}",
        larust_orm::placeholder(backend, 1),
        larust_orm::placeholder(backend, 2),
    );
    let rows: Vec<(i64, String, String, Option<i64>, i64)> = sqlx::query_as(&select_sql)
        .bind(notifiable.auth_id())
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    rows.into_iter()
        .map(|(id, notification_type, data, read_at, created_at)| {
            Ok(StoredNotification {
                id,
                notification_type,
                data: serde_json::from_str(&data)
                    .map_err(|source| AppError::Internal(Box::new(source)))?,
                read_at,
                created_at,
            })
        })
        .collect()
}

/// The count of `notifiable`'s own notifications that have never been
/// marked read - the number a notification-bell UI would badge with.
pub async fn unread_count<U: Authenticatable>(notifiable: &U) -> Result<i64, AppError> {
    let pool = larust_orm::pool()?;
    ensure_table(pool).await?;

    let select_sql = format!(
        "SELECT COUNT(*) FROM notifications WHERE notifiable_id = {} AND read_at IS NULL",
        larust_orm::placeholder(larust_orm::backend(), 1)
    );
    let (count,): (i64,) = sqlx::query_as(&select_sql)
        .bind(notifiable.auth_id())
        .fetch_one(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    Ok(count)
}

/// Marks one notification read, verifying it actually belongs to
/// `notifiable` first - the same "does this row belong to the acting
/// user?" question `larust_auth::Policy<U>::update`/`delete` already
/// answer, so this reuses [`larust_auth::authorize`] rather than
/// reinventing the check: a mismatched owner is a loud
/// `AppError::Http{FORBIDDEN, ..}`, matching how e.g. updating someone
/// else's post already responds today, not a silent no-op (silently
/// collapsing "not yours" into `Ok(())` is the right call for an
/// *authentication*-state ambiguity like "am I logged in at all" -
/// `larust_auth::guard`'s own precedent - but this is an *authorization*
/// question about a specific id someone is deliberately trying to act on,
/// a different question with a different, louder, established answer in
/// this codebase). A nonexistent id is `AppError::NotFound`, kept
/// distinct from the mismatched-owner case.
///
/// Marking an already-read notification read again is a legal no-op, not
/// a distinct error - there's no meaningful "double read" state worth
/// rejecting.
pub async fn mark_as_read<U: Authenticatable>(
    notifiable: &U,
    notification_id: i64,
) -> Result<(), AppError> {
    let pool = larust_orm::pool()?;
    ensure_table(pool).await?;

    let backend = larust_orm::backend();
    let select_owner_sql = format!(
        "SELECT notifiable_id FROM notifications WHERE id = {}",
        larust_orm::placeholder(backend, 1)
    );
    let owner: Option<(i64,)> = sqlx::query_as(&select_owner_sql)
        .bind(notification_id)
        .fetch_optional(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    let Some((owner_id,)) = owner else {
        return Err(AppError::NotFound);
    };
    authorize(owner_id == notifiable.auth_id())?;

    let update_sql = format!(
        "UPDATE notifications SET read_at = {} WHERE id = {}",
        larust_orm::placeholder(backend, 1),
        larust_orm::placeholder(backend, 2),
    );
    sqlx::query(&update_sql)
        .bind(now_unix_secs())
        .bind(notification_id)
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    Ok(())
}

/// Marks every one of `notifiable`'s own notifications read. No ownership
/// check needed the way [`mark_as_read`] needs one - `WHERE notifiable_id
/// = ?` already makes touching another notifiable's rows structurally
/// impossible.
pub async fn mark_all_as_read<U: Authenticatable>(notifiable: &U) -> Result<(), AppError> {
    let pool = larust_orm::pool()?;
    ensure_table(pool).await?;

    let backend = larust_orm::backend();
    let sql = format!(
        "UPDATE notifications SET read_at = {} WHERE notifiable_id = {} AND read_at IS NULL",
        larust_orm::placeholder(backend, 1),
        larust_orm::placeholder(backend, 2),
    );
    sqlx::query(&sql)
        .bind(now_unix_secs())
        .bind(notifiable.auth_id())
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    Ok(())
}

/// Permanently removes one notification, after verifying it belongs to the
/// acting notifiable. This mirrors [`mark_as_read`]'s ownership behavior:
/// missing rows are `NotFound`, while another user's row is `FORBIDDEN`.
pub async fn delete_notification<U: Authenticatable>(
    notifiable: &U,
    notification_id: i64,
) -> Result<(), AppError> {
    let pool = larust_orm::pool()?;
    ensure_table(pool).await?;

    let backend = larust_orm::backend();
    let select_owner_sql = format!(
        "SELECT notifiable_id FROM notifications WHERE id = {}",
        larust_orm::placeholder(backend, 1)
    );
    let owner: Option<(i64,)> = sqlx::query_as(&select_owner_sql)
        .bind(notification_id)
        .fetch_optional(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    let Some((owner_id,)) = owner else {
        return Err(AppError::NotFound);
    };
    authorize(owner_id == notifiable.auth_id())?;

    let delete_sql = format!(
        "DELETE FROM notifications WHERE id = {}",
        larust_orm::placeholder(backend, 1)
    );
    sqlx::query(&delete_sql)
        .bind(notification_id)
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    Ok(())
}

/// Permanently removes every notification owned by `notifiable`.
pub async fn clear_notifications<U: Authenticatable>(notifiable: &U) -> Result<(), AppError> {
    let pool = larust_orm::pool()?;
    ensure_table(pool).await?;

    let sql = format!(
        "DELETE FROM notifications WHERE notifiable_id = {}",
        larust_orm::placeholder(larust_orm::backend(), 1)
    );
    sqlx::query(&sql)
        .bind(notifiable.auth_id())
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    struct TestUser {
        id: i64,
    }

    impl Authenticatable for TestUser {
        fn auth_id(&self) -> i64 {
            self.id
        }

        async fn find_for_auth(_id: i64) -> Result<Option<Self>, AppError> {
            unreachable!("not exercised by these tests")
        }
    }

    #[derive(Serialize, Deserialize)]
    struct Greeting {
        message: String,
    }

    impl Notification for Greeting {
        const NOTIFICATION_TYPE: &'static str = "greeting";
    }

    async fn connect_test_db() {
        let dir = tempfile::tempdir().unwrap().keep();
        let database_url = format!("sqlite://{}/test.sqlite", dir.display());
        larust_orm::connect(&database_url).await.unwrap();
    }

    async fn greet<U: Authenticatable>(notifiable: &U, message: &str) {
        notify(
            notifiable,
            &Greeting {
                message: message.to_string(),
            },
        )
        .await
        .unwrap();
    }

    /// All scenarios share one test function, not several: `larust_orm::
    /// connect()` sets a process-wide pool exactly once (a second call in
    /// the same test binary errors with "connect() called more than
    /// once"), the same singleton-per-process constraint this codebase's
    /// other test suites (`larust-mail`'s `queue_job.rs`, `larust-mail`'s
    /// `fake.rs`) already document and work around. Each scenario uses
    /// its own, disjoint set of notifiable ids so they can't interfere
    /// with each other despite sharing one table/connection.
    #[tokio::test]
    async fn notifications_crate_behaves_correctly_across_every_scenario() {
        connect_test_db().await;

        // notify + notifications_for round trip, newest first.
        let alice = TestUser { id: 1 };
        let bob = TestUser { id: 2 };
        greet(&alice, "first").await;
        greet(&alice, "second").await;
        greet(&bob, "for bob").await;

        let alice_notifications = notifications_for(&alice, 10).await.unwrap();
        assert_eq!(alice_notifications.len(), 2);
        assert_eq!(alice_notifications[0].data["message"], "second");
        assert_eq!(alice_notifications[1].data["message"], "first");
        assert_eq!(alice_notifications[0].notification_type, "greeting");

        // unread_count reflects reads.
        let carol = TestUser { id: 3 };
        greet(&carol, "a").await;
        greet(&carol, "b").await;
        greet(&carol, "c").await;
        assert_eq!(unread_count(&carol).await.unwrap(), 3);
        let first_id = notifications_for(&carol, 10).await.unwrap()[2].id;
        mark_as_read(&carol, first_id).await.unwrap();
        assert_eq!(unread_count(&carol).await.unwrap(), 2);

        // mark_as_read is idempotent.
        let dave = TestUser { id: 4 };
        greet(&dave, "hi").await;
        let dave_id = notifications_for(&dave, 1).await.unwrap()[0].id;
        mark_as_read(&dave, dave_id).await.unwrap();
        mark_as_read(&dave, dave_id).await.unwrap();
        assert_eq!(unread_count(&dave).await.unwrap(), 0);

        // mark_as_read rejects a mismatched notifiable - the ownership
        // guarantee this feature hinges on.
        let erin = TestUser { id: 5 };
        let frank = TestUser { id: 6 };
        greet(&erin, "erin's").await;
        let erin_notification_id = notifications_for(&erin, 1).await.unwrap()[0].id;
        match mark_as_read(&frank, erin_notification_id).await {
            Err(AppError::Http { status, .. }) => {
                assert_eq!(status, larust_core::axum::http::StatusCode::FORBIDDEN);
            }
            Err(other) => panic!("expected AppError::Http{{FORBIDDEN, ..}}, got {other:?}"),
            Ok(()) => panic!("expected AppError::Http{{FORBIDDEN, ..}}, got Ok(())"),
        }
        assert_eq!(
            unread_count(&erin).await.unwrap(),
            1,
            "erin's notification must still be unread"
        );

        // mark_as_read returns NotFound for a nonexistent id, distinct
        // from the mismatched-owner case above.
        assert!(matches!(
            mark_as_read(&erin, 999_999).await,
            Err(AppError::NotFound)
        ));

        // mark_all_as_read only touches the caller's own rows.
        let grace = TestUser { id: 7 };
        let heidi = TestUser { id: 8 };
        greet(&grace, "grace's").await;
        greet(&heidi, "heidi's").await;
        mark_all_as_read(&grace).await.unwrap();
        assert_eq!(unread_count(&grace).await.unwrap(), 0);
        assert_eq!(unread_count(&heidi).await.unwrap(), 1);

        // Deletion follows the same strict ownership model and clear-all
        // remains scoped to the current notifiable.
        let judy = TestUser { id: 10 };
        let karl = TestUser { id: 11 };
        greet(&judy, "remove me").await;
        greet(&karl, "keep me").await;
        let judy_id = notifications_for(&judy, 1).await.unwrap()[0].id;
        delete_notification(&judy, judy_id).await.unwrap();
        assert!(notifications_for(&judy, 10).await.unwrap().is_empty());
        clear_notifications(&judy).await.unwrap();
        assert_eq!(notifications_for(&karl, 10).await.unwrap().len(), 1);

        // notifications_for respects the caller-supplied limit.
        let ivan = TestUser { id: 9 };
        for i in 0..5 {
            greet(&ivan, &format!("message {i}")).await;
        }
        assert_eq!(notifications_for(&ivan, 2).await.unwrap().len(), 2);

        // notify_and_mail records both sides - `Mail::fake()` records
        // instead of dispatching for real (no `mail_driver`/SMTP config
        // needed in this test), same tool `larust-mail`'s own tests use.
        larust_mail::fake();
        let liam = TestUser { id: 12 };
        notify_and_mail(
            &liam,
            &Greeting {
                message: "concurrent".to_string(),
            },
            "liam@example.com",
            WelcomeMail,
        )
        .await
        .unwrap();

        let liam_notifications = notifications_for(&liam, 10).await.unwrap();
        assert_eq!(liam_notifications.len(), 1);
        assert_eq!(liam_notifications[0].data["message"], "concurrent");
        larust_mail::assert_sent::<WelcomeMail>(|sent| {
            sent.to == vec!["liam@example.com".to_string()] && sent.subject == "Welcome"
        });

        // The `to(...)` builder: both channels chained actually deliver.
        let mia = TestUser { id: 100 };
        to(
            &mia,
            &Greeting {
                message: "builder database+mail".to_string(),
            },
        )
        .database()
        .mail()
        .send()
        .await
        .unwrap();
        let mia_notifications = notifications_for(&mia, 10).await.unwrap();
        assert_eq!(mia_notifications.len(), 1);
        assert_eq!(
            mia_notifications[0].data["message"],
            "builder database+mail"
        );
        larust_mail::assert_sent::<GreetingMail>(|sent| {
            sent.to == vec!["user100@example.com".to_string()]
                && sent.subject == "You have a greeting"
        });

        // Only the channels actually chained run - a mail-only
        // notification never writes a database row.
        let noah = TestUser { id: 101 };
        to(
            &noah,
            &Greeting {
                message: "mail only".to_string(),
            },
        )
        .mail()
        .send()
        .await
        .unwrap();
        assert!(notifications_for(&noah, 10).await.unwrap().is_empty());
        larust_mail::assert_sent::<GreetingMail>(|sent| {
            sent.to == vec!["user101@example.com".to_string()]
        });

        // No channel selected at all is not an error.
        let olive = TestUser { id: 102 };
        to(
            &olive,
            &Greeting {
                message: "nobody asked".to_string(),
            },
        )
        .send()
        .await
        .unwrap();
        assert!(notifications_for(&olive, 10).await.unwrap().is_empty());
    }

    struct WelcomeMail;

    impl Mailable for WelcomeMail {
        fn subject(&self) -> String {
            "Welcome".to_string()
        }

        fn html_body(&self) -> String {
            "<p>hi</p>".to_string()
        }
    }

    impl MailNotifiable for TestUser {
        fn notification_email(&self) -> String {
            format!("user{}@example.com", self.id)
        }
    }

    impl MailNotification for Greeting {
        type Mail = GreetingMail;

        fn to_mail(&self) -> Self::Mail {
            GreetingMail {
                message: self.message.clone(),
            }
        }
    }

    struct GreetingMail {
        message: String,
    }

    impl Mailable for GreetingMail {
        fn subject(&self) -> String {
            "You have a greeting".to_string()
        }

        fn html_body(&self) -> String {
            format!("<p>{}</p>", self.message)
        }
    }
}
