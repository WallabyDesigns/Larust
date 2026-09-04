use larust_support::auth::Auth;
use larust_support::axum::extract::Query;
use larust_support::axum::response::IntoResponse;
use larust_support::AppError;
use serde::Deserialize;

use crate::models::{Comment, NewComment, Post, User};
use crate::requests::StoreCommentRequest;

pub struct CommentController;

/// A random id the show template generates once per page load (`Math.
/// random()`, not tied to the logged-in user at all) - the only way the
/// client can tell "this is the exact tab that sent this" apart from
/// "this is the same *account*, in a different tab," which matters here
/// specifically because two open tabs logged in as the same user is a
/// completely normal way to actually exercise this feature (it's how
/// this was tested), not an edge case to ignore.
#[derive(Deserialize)]
pub struct TypingQuery {
    tab_id: String,
}

impl CommentController {
    /// No `wire:` reactivity here - a plain POST + redirect back to the
    /// post page, same shape as `PostController::store`. The "no reload
    /// needed" half of the live-comments story isn't this handler's job at
    /// all: it's `larust_support::reverb::broadcast_event` below, which
    /// pushes the new comment to every *other* open tab on this post's
    /// page over `posts.{post_id}.comments` - the submitting browser gets
    /// there the ordinary way (redirect), everyone else's tab gets there
    /// via `LarustReverb.channel(...).listen('CommentCreated', ...)`
    /// (`resources/views/posts/show.blade.xr`) appending the same data as
    /// a DOM node with no page load at all.
    pub async fn store(
        Auth(user): Auth<User>,
        post: Post,
        request: StoreCommentRequest,
    ) -> Result<impl IntoResponse, AppError> {
        let validated = request.validated();
        let comment = Comment::create(NewComment {
            post_id: post.id,
            user_id: user.id,
            body: validated.body,
        })
        .await?;

        larust_support::reverb::broadcast_event(
            &format!("posts.{}.comments", post.id),
            "CommentCreated",
            &larust_support::serde_json::json!({
                "id": comment.id,
                "author": user.name,
                "author_id": user.id,
                "body": comment.body,
            }),
        )?;

        larust_support::redirect().to(&format!("/posts/{}", post.id))
    }

    /// The comment's own author, or a moderator (`Comment::can_manage`).
    /// Same plain-POST-then-redirect shape as `store` - the submitting
    /// tab gets there via the redirect, every other open tab gets there
    /// via the `CommentDeleted` broadcast below, which the client removes
    /// the matching `[data-comment-id]` node for.
    pub async fn destroy(
        Auth(user): Auth<User>,
        comment: Comment,
    ) -> Result<impl IntoResponse, AppError> {
        larust_support::auth::authorize(comment.can_manage(&user).await?)?;
        let post_id = comment.post_id;
        let comment_id = comment.id;
        Comment::delete(comment_id).await?;

        larust_support::reverb::broadcast_event(
            &format!("posts.{post_id}.comments"),
            "CommentDeleted",
            &larust_support::serde_json::json!({ "id": comment_id }),
        )?;

        larust_support::redirect().to(&format!("/posts/{post_id}"))
    }

    /// No client -> server WebSocket messages exist in this framework's
    /// push mechanisms by design (`larust_reverb::socket` ignores inbound
    /// frames) - so "X is typing" has to be a real HTTP round trip: the
    /// typing browser POSTs here, and this just re-broadcasts it over the
    /// same `posts.{post_id}.comments` channel comments already use (no
    /// new channel needed). Client-side throttled to at most once every
    /// 2s (see the show template's own `<script>`) - the only spam guard;
    /// no server-side rate limiting added here, matching this app's
    /// existing "web routes: CSRF only, no throttle" convention.
    pub async fn typing(
        Auth(user): Auth<User>,
        post: Post,
        Query(params): Query<TypingQuery>,
    ) -> Result<impl IntoResponse, AppError> {
        larust_support::reverb::broadcast_event(
            &format!("posts.{}.comments", post.id),
            "UserTyping",
            &larust_support::serde_json::json!({
                "author": user.name,
                "author_id": user.id,
                "tab_id": params.tab_id,
            }),
        )?;
        Ok(larust_support::axum::http::StatusCode::NO_CONTENT)
    }
}
