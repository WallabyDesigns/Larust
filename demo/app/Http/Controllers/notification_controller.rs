use larust_http::session::Session;
use larust_support::auth::Auth;
use larust_support::axum::extract::Path;
use larust_support::axum::response::IntoResponse;
use larust_support::notification::{
    mark_all_as_read, mark_as_read, notifications_for, unread_count as unread_count_query,
    StoredNotification,
};
use larust_support::view;
use larust_support::AppError;

use crate::models::User;

/// The unread-notification badge every page's nav header shows — resolves
/// the current user from `session` itself (most pages only have a plain
/// `Session`, not an already-extracted `Auth<User>`) and returns `0` for
/// a logged-out visitor rather than erroring, matching `is_authenticated`'s
/// own "false, not a failure" treatment on public pages. Pages that
/// already extract `Auth(user): Auth<User>` (every auth-gated one) call
/// `larust_support::notification::unread_count(&user)` directly instead —
/// this exists for the public/mixed-audience pages that don't.
pub async fn unread_count_for(session: &Session) -> Result<i64, AppError> {
    match larust_support::auth::user::<User>(session).await? {
        Some(user) => unread_count_query(&user).await,
        None => Ok(0),
    }
}

const NOTIFICATIONS_PER_PAGE: i64 = 20;

/// A small, template-friendly view of one stored notification —
/// `StoredNotification::data` is a raw `serde_json::Value` (it can hold
/// any `Notification` type's payload), so this is where that heterogeneity
/// gets resolved into something `{{ }}`/`@foreach` can read plain fields
/// off. Today the only notification type this app ever writes is
/// `PostPublished` (`app/Notifications/post_published.rs`); anything else
/// falls back to a generic line rather than guessing at fields it doesn't
/// have — the same "app decides how to display unrecognized types"
/// contract `larust-notifications`'s own doc comment describes.
struct NotificationView {
    id: i64,
    message: String,
    post_id: i64,
    is_unread: bool,
}

fn to_view(stored: StoredNotification) -> NotificationView {
    let (message, post_id) = match stored.notification_type.as_str() {
        "post_published" => {
            let title = stored.data["title"].as_str().unwrap_or("your post");
            let post_id = stored.data["post_id"].as_i64().unwrap_or(0);
            (format!("Your post \"{title}\" was published."), post_id)
        }
        other => (format!("New notification ({other})."), 0),
    };
    NotificationView {
        id: stored.id,
        message,
        post_id,
        is_unread: stored.read_at.is_none(),
    }
}

pub struct NotificationController;

impl NotificationController {
    pub async fn index(
        session: Session,
        Auth(user): Auth<User>,
    ) -> Result<impl IntoResponse, AppError> {
        let stored = notifications_for(&user, NOTIFICATIONS_PER_PAGE).await?;
        // The real, unbounded count — not derived from the (at most 20-row)
        // `stored` list above, which would silently undercount past that
        // limit. Same call every other page's nav badge makes.
        let unread_count = unread_count_query(&user).await?;
        // Computed before the `@foreach` below consumes `notifications` by
        // value — matching `post-list.blade.xr`'s own `post_count`
        // precedent, not a post-loop `.is_empty()` call on an already-moved
        // list.
        let total_count = stored.len() as i64;
        let notifications: Vec<NotificationView> = stored.into_iter().map(to_view).collect();
        let csrf_token = larust_http::csrf::token(&session).await;
        let is_authenticated = true;
        let nav_active = "notifications";
        Ok(view!("notifications.index", {
            notifications,
            unread_count,
            total_count,
            csrf_token,
            is_authenticated,
            nav_active,
        }))
    }

    pub async fn mark_read(
        Auth(user): Auth<User>,
        Path(id): Path<i64>,
    ) -> Result<impl IntoResponse, AppError> {
        mark_as_read(&user, id).await?;
        larust_support::redirect().route("notifications.index")
    }

    pub async fn mark_all_read(Auth(user): Auth<User>) -> Result<impl IntoResponse, AppError> {
        mark_all_as_read(&user).await?;
        larust_support::redirect().route("notifications.index")
    }
}
