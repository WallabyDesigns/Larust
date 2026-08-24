use larust_http::session::Session;
use larust_support::axum::http::StatusCode;
use larust_support::serde_json;
use larust_support::view;
use larust_support::view::View;
use larust_support::wire::WireComponent;
use larust_support::AppError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::events::PostCreated;
use crate::models::{NewPost, Post, User};

/// The post-creation *and* post-editing form as a single reactive
/// component — the second reference example for `@wire(...)`, alongside
/// `PostList`. One component handles both modes (Livewire's own usual
/// pattern) rather than a second near-duplicate template: `create.blade.xr`
/// mounts `@wire('post-form')` with no props, `edit.blade.xr` mounts
/// `@wire('post-form', { post_id: post.id })` — `mount` populates `title`/
/// `tags`/`content` from the existing post whenever `post_id` is present,
/// and `publish` below either creates a new post or updates the existing
/// one accordingly. `wire:model` on each field (deferred — synced once, on
/// submit, not on every keystroke) and `wire:submit="post"` on the
/// `<form>` itself intercept the native submit, dispatch the `post`
/// action, and — on success — have the client navigate via `call`'s
/// `Ok(Some(path))` redirect return. On validation failure, `errors` is set
/// on `self` and the component simply re-renders in place with those
/// messages — no redirect, no page reload, no HTTP error response the
/// client has to interpret.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PostForm {
    /// `None` in create mode; `Some(id)` in edit mode. Never trusted for
    /// authorization on its own — `publish` re-checks the post's actual
    /// `user_id` against the session's current user before writing
    /// anything, the same real boundary `PostController::update` enforces
    /// on the plain-HTML-form path.
    #[serde(default)]
    post_id: Option<i64>,
    title: String,
    tags: String,
    content: String,
    #[serde(default)]
    errors: HashMap<String, String>,
}

impl WireComponent for PostForm {
    const NAME: &'static str = "post-form";

    /// `mount` has no way to signal failure (it returns `Self`, not a
    /// `Result`), so an edit-mode mount for a post that doesn't exist, or
    /// that this viewer can't manage (not the owner, and no `manage-posts`
    /// permission — see `Post::can_manage`), just falls back to an empty
    /// create-mode form rather than silently leaking another user's draft
    /// — reaching this component in edit mode at all already requires the
    /// page-level GET `/posts/{id}/edit` to have let you through (see
    /// `PostController::edit`'s own `post.can_manage(&user)` check), so
    /// this is defense-in-depth, not the real authorization boundary;
    /// `publish` below is.
    async fn mount(session: &Session, props: &HashMap<String, serde_json::Value>) -> Self {
        let Some(post_id) = props.get("post_id").and_then(|v| v.as_i64()) else {
            return PostForm::default();
        };
        let Ok(Some(post)) = Post::find(post_id).await else {
            return PostForm::default();
        };
        let Ok(Some(viewer)) = larust_support::auth::user::<User>(session).await else {
            return PostForm::default();
        };
        let Ok(true) = post.can_manage(&viewer).await else {
            return PostForm::default();
        };

        let tags = post.tags().await.unwrap_or_default();
        let tags = tags
            .iter()
            .map(|tag| tag.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        PostForm {
            post_id: Some(post.id),
            title: post.title,
            tags,
            content: post.content,
            errors: HashMap::new(),
        }
    }

    async fn render(&self) -> View {
        let title_error = self.errors.get("title").cloned().unwrap_or_default();
        let content_error = self.errors.get("content").cloned().unwrap_or_default();
        let is_editing = self.post_id.is_some();
        view!("components.post-form", {
            title: self.title.clone(),
            tags: self.tags.clone(),
            content: self.content.clone(),
            title_error,
            content_error,
            is_editing,
        })
    }

    async fn call(
        &mut self,
        session: &Session,
        action: &str,
        _args: &serde_json::Value,
    ) -> Result<Option<String>, AppError> {
        match action {
            "post" => self.publish(session).await,
            other => Err(AppError::Http {
                status: StatusCode::NOT_FOUND,
                message: format!("component `{}` has no action `{other}`", Self::NAME),
            }),
        }
    }
}

impl PostForm {
    async fn publish(&mut self, session: &Session) -> Result<Option<String>, AppError> {
        self.validate();
        if !self.errors.is_empty() {
            return Ok(None);
        }

        // `wire:submit` reaching this component's action endpoint at all
        // already implies a session exists, but not that it's a *logged-in*
        // one — both `create.blade.xr` and `edit.blade.xr` are only ever
        // linked to from behind `require_auth` (see `demo/src/main.rs`'s
        // route group), so this should always resolve; treated as a real,
        // reportable error rather than an `.unwrap()` in case that ever
        // stops being true. The full `User`, not just its id, since
        // `update_existing` below needs it for `Post::can_manage`.
        let Some(viewer) = larust_support::auth::user::<User>(session).await? else {
            return Err(AppError::Http {
                status: StatusCode::UNAUTHORIZED,
                message: "you must be logged in to publish a post".to_string(),
            });
        };

        let content = larust_support::sanitize_rich_text(&self.content);

        let post = match self.post_id {
            Some(id) => self.update_existing(id, &viewer, content).await?,
            None => self.create_new(viewer.id, content).await?,
        };
        post.sync_tags_from_csv(&self.tags).await?;

        Ok(Some(format!("/posts/{}", post.id)))
    }

    async fn create_new(&self, user_id: i64, content: String) -> Result<Post, AppError> {
        let post = Post::create(NewPost {
            user_id,
            title: self.title.clone(),
            content,
        })
        .await?;

        larust_support::event::dispatch(PostCreated {
            post_id: post.id,
            title: post.title.clone(),
            user_id: post.user_id,
        })
        .await;

        Ok(post)
    }

    /// `Post::can_manage` — the same check `PostController::update`'s own
    /// `post.can_manage(&user)` enforces on the plain-HTML-form path (owner,
    /// or a `Role::Moderator`'s `manage-posts` permission), the real
    /// authorization boundary for this wire-based save path.
    async fn update_existing(
        &self,
        id: i64,
        viewer: &User,
        content: String,
    ) -> Result<Post, AppError> {
        let post = Post::find(id).await?.ok_or(AppError::NotFound)?;
        if !post.can_manage(viewer).await? {
            return Err(AppError::Http {
                status: StatusCode::FORBIDDEN,
                message: "you don't have permission to edit this post".to_string(),
            });
        }

        larust_support::orm::sqlx::query("UPDATE posts SET title = ?, content = ? WHERE id = ?")
            .bind(self.title.clone())
            .bind(content)
            .bind(post.id)
            .execute(larust_support::orm::pool()?)
            .await
            .map_err(|error| AppError::Internal(Box::new(error)))?;

        Ok(post)
    }

    /// The same constraints `StorePostRequest` enforces on the plain-HTML-
    /// form path (`title` required, `content` required, both length-capped)
    /// — re-checked here by hand rather than reused directly, since
    /// `#[derive(FormRequest)]` validates an incoming HTTP request body,
    /// not an already-deserialized component's own fields. Populates
    /// `self.errors`; callers check `self.errors.is_empty()` afterward.
    fn validate(&mut self) {
        self.errors.clear();
        if self.title.trim().is_empty() {
            self.errors
                .insert("title".to_string(), "Title is required.".to_string());
        } else if self.title.len() > 255 {
            self.errors.insert(
                "title".to_string(),
                "Title must be 255 characters or fewer.".to_string(),
            );
        }
        if self.content.trim().is_empty() {
            self.errors
                .insert("content".to_string(), "Content is required.".to_string());
        } else if self.content.len() > 50_000 {
            self.errors
                .insert("content".to_string(), "Content is too long.".to_string());
        }
    }
}
