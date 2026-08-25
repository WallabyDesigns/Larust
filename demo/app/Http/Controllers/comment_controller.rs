use larust_support::auth::Auth;
use larust_support::axum::response::IntoResponse;
use larust_support::AppError;

use crate::models::{Comment, NewComment, Post, User};
use crate::requests::StoreCommentRequest;

pub struct CommentController;

impl CommentController {
    /// No `wire:` reactivity here — a plain POST + redirect back to the
    /// post page, same shape as `PostController::store`. The "no reload
    /// needed" half of the live-comments story isn't this handler's job at
    /// all: it's `larust_support::reverb::broadcast_event` below, which
    /// pushes the new comment to every *other* open tab on this post's
    /// page over `posts.{post_id}.comments` — the submitting browser gets
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
                "author": user.name,
                "body": comment.body,
            }),
        )?;

        larust_support::redirect().to(&format!("/posts/{}", post.id))
    }
}
