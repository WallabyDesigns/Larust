//! Durable, per-notifiable, read-tracked notifications — Laravel's
//! *database* notification channel specifically, not its full
//! `Notification`/`via()`/`toMail()`/`toDatabase()`/`toBroadcast()` shape.
//!
//! Laravel's `Notification` has *optional* per-channel render methods,
//! decided at runtime by `via($notifiable)`. There's no clean way to
//! express "this trait method is conditionally required based on another
//! method's runtime return value" in Rust without `Option`-returning
//! defaults — and this codebase's closest sibling traits (`Mailable`,
//! `larust_queue::Job`, `larust_auth::Authenticatable`) are all
//! zero-default-method traits, deliberately, specifically to force a
//! compile error on a real gap rather than a silently-never-implemented
//! one. Building Laravel's full shape here would be the first trait in
//! this codebase to break that convention.
//!
//! So this crate doesn't try to unify mail/broadcast delivery at all —
//! `larust-mail` (`mail().to(...).send()/.queue()`) and `larust-live::push`
//! (`push::broadcast(channel, html)`) already fully solve "send an email"
//! and "push a live update" independently; wrapping them here would add
//! indirection without adding capability. If a notification-worthy event
//! should also email or live-push someone, call those APIs directly,
//! alongside [`notify`], at the same call site:
//!
//! ```ignore
//! notify(&user, &InvoiceSent { invoice_id }).await?;                    // database
//! mail().to(&user.email).send(InvoiceSentMail { invoice_id }).await?;   // mail, if wanted
//! push::broadcast(&format!("notifications.{}", user.auth_id()), ...);  // broadcast, if wanted
//! ```
//!
//! Re-exported through `larust_support::notification` (see
//! `crates/larust-support/src/lib.rs`) so generated apps depend only on
//! `larust-support`, never on this crate directly.

use larust_auth::{authorize, Authenticatable};
use larust_core::AppError;
use serde::Serialize;
use sqlx::SqlitePool;
use std::time::{SystemTime, UNIX_EPOCH};

/// A durable fact worth recording against a notifiable — Laravel's
/// `Notification` class, narrowed to just its database-channel shape.
/// Serializing `Self` *is* the stored `data` payload; there's no separate
/// render step the way [`Notification::to_database`] would imply if it
/// existed. `NOTIFICATION_TYPE` is the same explicit, stable, app-chosen
/// tag convention `larust_queue::Job::JOB_TYPE` already establishes —
/// deliberately not `std::any::type_name::<Self>()`, since that string
/// isn't stable across a rename and an already-stored row would silently
/// stop being attributable to its type.
///
/// No `DeserializeOwned` bound (unlike `Job`): nothing in this crate ever
/// reconstructs a concrete `Self` from a stored row — [`notifications_for`]
/// reads heterogeneous rows back across many different notification types
/// at once and can only sensibly return the type tag plus raw JSON,
/// matching Laravel's own `type`/`data` column split.
pub trait Notification: Serialize + Send + Sync {
    const NOTIFICATION_TYPE: &'static str;
}

/// One stored row, as read back by [`notifications_for`]. `data` stays a
/// raw [`serde_json::Value`] rather than a concrete type — a single query
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
/// `larust-queue`'s `jobs`/`failed_jobs` establish — a plain
/// `CREATE TABLE IF NOT EXISTS`, no migration file and no explicit
/// startup call needed anywhere. Unlike either of those tables, this one
/// is genuinely filtered and sorted by a foreign-key-shaped column
/// (`notifiable_id`) at read time, so it also creates a matching index —
/// the first framework-owned table in this codebase that needs one.
///
/// Deliberately **not** memoized behind a `OnceCell` the way a first draft
/// of this function was: a `static` completion flag is process-wide, but
/// `larust_testing::test_transaction` swaps in a fresh, isolated database
/// *per test* within the same process — a real regression this shipped
/// with and a later test suite caught (a page exercising `unread_count`
/// for the first time started failing with "no such table: notifications"
/// once an *earlier* test in the same binary had already flipped the flag
/// against its own, different, since-discarded database). `IF NOT EXISTS`
/// makes re-running this on every call cheap enough in practice — a
/// schema lookup SQLite already has to do, no data scan — that giving up
/// the memoization is a better trade than reintroducing that failure mode.
async fn ensure_table(pool: &SqlitePool) -> Result<(), AppError> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS notifications (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            notifiable_id INTEGER NOT NULL, \
            notification_type TEXT NOT NULL, \
            data TEXT NOT NULL, \
            read_at INTEGER, \
            created_at INTEGER NOT NULL\
         )",
    )
    .execute(pool)
    .await
    .map_err(|source| AppError::Internal(Box::new(source)))?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_notifications_notifiable \
         ON notifications (notifiable_id, created_at DESC)",
    )
    .execute(pool)
    .await
    .map_err(|source| AppError::Internal(Box::new(source)))?;

    Ok(())
}

fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_secs() as i64
}

/// Records `notification` against `notifiable` — Laravel's
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

    sqlx::query(
        "INSERT INTO notifications (notifiable_id, notification_type, data, created_at) \
         VALUES (?, ?, ?, ?)",
    )
    .bind(notifiable.auth_id())
    .bind(N::NOTIFICATION_TYPE)
    .bind(data)
    .bind(now_unix_secs())
    .execute(pool)
    .await
    .map_err(|source| AppError::Internal(Box::new(source)))?;

    Ok(())
}

/// Reads `notifiable`'s own notifications, newest first, capped at
/// `limit` rows. `limit` is caller-supplied rather than a framework-picked
/// constant — the same real precedent `larust_orm::QueryBuilder::
/// paginate(per_page)` already sets in this crate family — making an
/// unbounded query structurally impossible rather than merely
/// policy-discouraged. No cursor/`before_id` pagination in v1, the same
/// documented gap `paginate` itself carries.
pub async fn notifications_for<U: Authenticatable>(
    notifiable: &U,
    limit: i64,
) -> Result<Vec<StoredNotification>, AppError> {
    let pool = larust_orm::pool()?;
    ensure_table(pool).await?;

    let rows: Vec<(i64, String, String, Option<i64>, i64)> = sqlx::query_as(
        "SELECT id, notification_type, data, read_at, created_at FROM notifications \
         WHERE notifiable_id = ? ORDER BY created_at DESC, id DESC LIMIT ?",
    )
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
/// marked read — the number a notification-bell UI would badge with.
pub async fn unread_count<U: Authenticatable>(notifiable: &U) -> Result<i64, AppError> {
    let pool = larust_orm::pool()?;
    ensure_table(pool).await?;

    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM notifications WHERE notifiable_id = ? AND read_at IS NULL",
    )
    .bind(notifiable.auth_id())
    .fetch_one(pool)
    .await
    .map_err(|source| AppError::Internal(Box::new(source)))?;

    Ok(count)
}

/// Marks one notification read, verifying it actually belongs to
/// `notifiable` first — the same "does this row belong to the acting
/// user?" question `larust_auth::Policy<U>::update`/`delete` already
/// answer, so this reuses [`larust_auth::authorize`] rather than
/// reinventing the check: a mismatched owner is a loud
/// `AppError::Http{FORBIDDEN, ..}`, matching how e.g. updating someone
/// else's post already responds today, not a silent no-op (silently
/// collapsing "not yours" into `Ok(())` is the right call for an
/// *authentication*-state ambiguity like "am I logged in at all" —
/// `larust_auth::guard`'s own precedent — but this is an *authorization*
/// question about a specific id someone is deliberately trying to act on,
/// a different question with a different, louder, established answer in
/// this codebase). A nonexistent id is `AppError::NotFound`, kept
/// distinct from the mismatched-owner case.
///
/// Marking an already-read notification read again is a legal no-op, not
/// a distinct error — there's no meaningful "double read" state worth
/// rejecting.
pub async fn mark_as_read<U: Authenticatable>(
    notifiable: &U,
    notification_id: i64,
) -> Result<(), AppError> {
    let pool = larust_orm::pool()?;
    ensure_table(pool).await?;

    let owner: Option<(i64,)> =
        sqlx::query_as("SELECT notifiable_id FROM notifications WHERE id = ?")
            .bind(notification_id)
            .fetch_optional(pool)
            .await
            .map_err(|source| AppError::Internal(Box::new(source)))?;

    let Some((owner_id,)) = owner else {
        return Err(AppError::NotFound);
    };
    authorize(owner_id == notifiable.auth_id())?;

    sqlx::query("UPDATE notifications SET read_at = ? WHERE id = ?")
        .bind(now_unix_secs())
        .bind(notification_id)
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    Ok(())
}

/// Marks every one of `notifiable`'s own notifications read. No ownership
/// check needed the way [`mark_as_read`] needs one — `WHERE notifiable_id
/// = ?` already makes touching another notifiable's rows structurally
/// impossible.
pub async fn mark_all_as_read<U: Authenticatable>(notifiable: &U) -> Result<(), AppError> {
    let pool = larust_orm::pool()?;
    ensure_table(pool).await?;

    sqlx::query("UPDATE notifications SET read_at = ? WHERE notifiable_id = ? AND read_at IS NULL")
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

    let owner: Option<(i64,)> =
        sqlx::query_as("SELECT notifiable_id FROM notifications WHERE id = ?")
            .bind(notification_id)
            .fetch_optional(pool)
            .await
            .map_err(|source| AppError::Internal(Box::new(source)))?;

    let Some((owner_id,)) = owner else {
        return Err(AppError::NotFound);
    };
    authorize(owner_id == notifiable.auth_id())?;

    sqlx::query("DELETE FROM notifications WHERE id = ?")
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

    sqlx::query("DELETE FROM notifications WHERE notifiable_id = ?")
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

        // mark_as_read rejects a mismatched notifiable — the ownership
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
    }
}
