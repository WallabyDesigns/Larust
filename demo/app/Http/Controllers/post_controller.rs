use larust_http::session::Session;
use larust_support::auth::{Auth, Policy};
use larust_support::axum::response::IntoResponse;
use larust_support::view;
use larust_support::AppError;

use crate::models::{NewPost, Post, User};
use crate::requests::StorePostRequest;

pub struct PostController;

impl PostController {
    /// The post listing itself — author/tag lookups, the live search
    /// filter, and per-viewer `can_manage` — now lives entirely in the
    /// `PostList` wire component (`app/Wire/post_list.rs`), mounted via
    /// `@wire('post-list')` in `posts.index`; this handler just renders the
    /// page shell around it.
    pub async fn index(session: Session) -> Result<impl IntoResponse, AppError> {
        let flash_success = session
            .remove::<String>("success")
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        let csrf_token = larust_http::csrf::token(&session).await;
        let is_authenticated = larust_support::auth::check(&session).await?;
        let nav_active = "posts";
        Ok(
            view!("posts.index", { session: &session, flash_success, csrf_token, is_authenticated, nav_active }),
        )
    }

    pub async fn create(session: Session) -> Result<impl IntoResponse, AppError> {
        let csrf_token = larust_http::csrf::token(&session).await;
        let is_authenticated = true;
        let nav_active = "create";
        Ok(view!("posts.create", { session: &session, csrf_token, is_authenticated, nav_active }))
    }

    pub async fn show(session: Session, post: Post) -> Result<impl IntoResponse, AppError> {
        let author_name = post
            .user()
            .await?
            .map(|author| author.name)
            .unwrap_or_else(|| "Unknown".to_string());
        let tags = post.tags().await?;
        let tag_names = tags
            .iter()
            .map(|tag| tag.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");

        // Public page, same as `index` — viewing doesn't require being
        // logged in, so this is an *optional* lookup (`auth::id`, not the
        // `Auth<User>` extractor `edit`/`update`/`destroy` use, which would
        // force a login redirect just to read a post).
        let current_user_id = larust_support::auth::id(&session).await?;
        let can_manage = current_user_id == Some(post.user_id);

        let csrf_token = larust_http::csrf::token(&session).await;
        let is_authenticated = larust_support::auth::check(&session).await?;
        let nav_active = "posts";
        Ok(view!("posts.show", {
            id: post.id,
            title: post.title,
            content: post.content,
            author_name,
            tag_names,
            can_manage,
            csrf_token,
            is_authenticated,
            nav_active,
        }))
    }

    /// The form itself — fields, tags, the Trix editor, validation, the
    /// actual save — is entirely the `PostForm` wire component (see
    /// `app/Wire/post_form.rs`), mounted via `@wire('post-form', {
    /// post_id: post.id })` in `posts.edit`; this handler only gates the
    /// page itself (`authorize_update`) and renders the shell around it.
    pub async fn edit(
        session: Session,
        Auth(user): Auth<User>,
        post: Post,
    ) -> Result<impl IntoResponse, AppError> {
        post.authorize_update(&user)?;
        let csrf_token = larust_http::csrf::token(&session).await;
        let is_authenticated = true;
        let nav_active = "posts";
        Ok(
            view!("posts.edit", { session: &session, post, csrf_token, is_authenticated, nav_active }),
        )
    }

    pub async fn update(
        session: Session,
        Auth(user): Auth<User>,
        post: Post,
        request: StorePostRequest,
    ) -> Result<impl IntoResponse, AppError> {
        post.authorize_update(&user)?;
        let validated = request.validated();
        let content = larust_support::sanitize_rich_text(&validated.content);
        larust_support::orm::sqlx::query("UPDATE posts SET title = ?, content = ? WHERE id = ?")
            .bind(validated.title)
            .bind(content)
            .bind(post.id)
            .execute(larust_support::orm::pool()?)
            .await
            .map_err(|error| AppError::Internal(Box::new(error)))?;
        post.sync_tags_from_csv(&validated.tags).await?;
        Ok(larust_support::redirect()
            .route("posts.index")?
            .with(&session, "success", "Post updated.")
            .await)
    }

    pub async fn destroy(
        session: Session,
        Auth(user): Auth<User>,
        post: Post,
    ) -> Result<impl IntoResponse, AppError> {
        post.authorize_delete(&user)?;
        larust_support::orm::sqlx::query("DELETE FROM post_tag WHERE post_id = ?")
            .bind(post.id)
            .execute(larust_support::orm::pool()?)
            .await
            .map_err(|error| AppError::Internal(Box::new(error)))?;
        Post::delete(post.id).await?;
        Ok(larust_support::redirect()
            .route("posts.index")?
            .with(&session, "success", "Post deleted.")
            .await)
    }

    pub async fn store(
        session: Session,
        Auth(user): Auth<User>,
        request: StorePostRequest,
    ) -> Result<impl IntoResponse, AppError> {
        Post::authorize_create(&user)?;
        let validated = request.validated();
        let content = larust_support::sanitize_rich_text(&validated.content);
        let post = Post::create(NewPost {
            user_id: user.id,
            title: validated.title,
            content,
        })
        .await?;

        post.sync_tags_from_csv(&validated.tags).await?;
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
