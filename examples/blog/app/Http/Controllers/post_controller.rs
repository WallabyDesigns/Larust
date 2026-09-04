use larust_http::session::Session;
use larust_support::auth::{Auth, Policy};
use larust_support::axum::response::IntoResponse;
use larust_support::view;
use larust_support::AppError;

use crate::models::{NewPost, Post, User};
use crate::requests::StorePostRequest;

/// A post plus its author's display name - `view!`'s `@foreach` binds a
/// single identifier per iteration (no tuple destructuring), so the
/// author name a `belongs_to` lookup resolves is flattened onto a small
/// per-view struct rather than passing `(Post, String)` pairs.
struct PostWithAuthor {
    title: String,
    author_name: String,
}

pub struct PostController;

impl PostController {
    pub async fn index(session: Session) -> Result<impl IntoResponse, AppError> {
        // A cheap aggregate, cached separately from the assembled list
        // below - invalidated by `store` (the only handler that changes
        // the total).
        let post_count: i64 = larust_support::cache::remember(
            "posts.count",
            std::time::Duration::from_secs(60),
            || async {
                let (count,): (i64,) =
                    larust_support::orm::sqlx::query_as("SELECT COUNT(*) FROM posts")
                        .fetch_one(larust_support::orm::pool()?)
                        .await
                        .map_err(|error| AppError::Internal(Box::new(error)))?;
                Ok(count)
            },
        )
        .await?;

        let posts = Post::all().await?;

        // Batch-loaded (eager) rather than one `post.user()` lookup per
        // post - `Post::load_user` is `#[belongs_to(...)]`'s generated
        // batch loader, fetching every author in one query instead of one
        // query per post.
        let authors = Post::load_user(&posts).await?;
        let posts_with_author: Vec<PostWithAuthor> = posts
            .into_iter()
            .map(|post| {
                let author_name = authors
                    .get(&post.user_id)
                    .map(|user| user.name.clone())
                    .unwrap_or_else(|| "Unknown".to_string());
                PostWithAuthor {
                    title: post.title,
                    author_name,
                }
            })
            .collect();

        let flash_success = session
            .remove::<String>("success")
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        Ok(view!("posts.index", { posts: posts_with_author, post_count, flash_success }))
    }

    pub async fn create(session: Session) -> Result<impl IntoResponse, AppError> {
        let csrf_token = larust_http::csrf::token(&session).await;
        Ok(view!("posts.create", { csrf_token }))
    }

    pub async fn show(post: Post) -> String {
        format!("{} (id {})", post.title, post.id)
    }

    pub async fn store(
        session: Session,
        Auth(user): Auth<User>,
        request: StorePostRequest,
    ) -> Result<impl IntoResponse, AppError> {
        Post::authorize_create(&user)?;
        let validated = request.validated();
        let post = Post::create(NewPost {
            user_id: user.id,
            title: validated.title,
        })
        .await?;
        larust_support::cache::forget("posts.count").await?;
        larust_support::event::dispatch(crate::events::PostCreated {
            post_id: post.id,
            title: post.title.clone(),
            user_id: post.user_id,
        })
        .await;

        Ok(larust_support::redirect()
            .route("posts.index")?
            .with(
                &session,
                "success",
                format!("Post \"{}\" (id {}) created.", post.title, post.id),
            )
            .await)
    }
}
