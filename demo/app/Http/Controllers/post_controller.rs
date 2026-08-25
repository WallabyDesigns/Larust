use larust_http::session::Session;
use larust_support::auth::{Auth, Policy};
use larust_support::axum::extract::Query;
use larust_support::axum::response::IntoResponse;
use larust_support::preferences::CookieJar;
use larust_support::view;
use larust_support::AppError;
use serde::Deserialize;

use crate::controllers::unread_count_for;
use crate::models::{Comment, NewPost, Post, User};
use crate::permissions::Permission;
use crate::requests::StorePostRequest;

pub struct PostController;

/// A comment plus its author's display name — same "flatten a `belongs_to`
/// lookup onto a small per-view struct" pattern as this file's own
/// `tags`/`author_name` handling in `show` below (`view!`'s `@foreach`
/// binds one identifier per iteration, no tuple destructuring).
struct CommentWithAuthor {
    id: i64,
    author_id: i64,
    author_name: String,
    /// The author's avatar initial (first letter, uppercased) — computed
    /// here rather than in the template, since the template's `{{ }}`
    /// expressions haven't been proven to support a `.chars().next()`
    /// chain anywhere else in this codebase; simple field access is the
    /// only shape used elsewhere, so this stays safely inside that shape.
    author_initial: String,
    body: String,
    /// Whether *this specific viewer* may delete this comment — computed
    /// server-side (`Comment::can_manage`), same reasoning as `Post`'s own
    /// `can_manage` field below: only correct for this initial render, not
    /// for a comment appended live afterward (see `show.blade.xr`'s own
    /// `window.__currentUserId` handling for that case).
    can_delete: bool,
}

/// `?tag=...` on `/posts` — a real, shareable/bookmarkable URL for "every
/// post tagged X", not just an ephemeral client-side toggle. `wire:click`
/// has no way to pass an argument yet (see `docs/ARCHITECTURE.md`'s own
/// "explicitly deferred" note on `@wire`), so a tag chip is a plain
/// `<a href="/posts?tag=...">` rather than a `wire:click` call — this is
/// what it lands on. `#[serde(default)]` so a bare `/posts` (no query
/// string at all) still deserializes instead of erroring.
#[derive(Deserialize)]
pub struct IndexQuery {
    #[serde(default)]
    tag: String,
}

impl PostController {
    /// The post listing itself — author/tag lookups, the live search/tag
    /// filter, and per-viewer `can_manage` — now lives entirely in the
    /// `PostList` wire component (`app/Wire/post_list.rs`), mounted via
    /// `@wire('post-list', { tag: tag })` in `posts.index`; this handler
    /// just renders the page shell around it and forwards the initial
    /// `?tag=` value so a shared/bookmarked filtered link renders already
    /// filtered on first paint, with no JS round-trip needed.
    pub async fn index(
        session: Session,
        cookies: CookieJar,
        Query(params): Query<IndexQuery>,
    ) -> Result<impl IntoResponse, AppError> {
        let flash_success = session
            .remove::<String>("success")
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        let csrf_token = larust_http::csrf::token(&session).await;
        let is_authenticated = larust_support::auth::check(&session).await?;
        let unread_count = unread_count_for(&session).await?;
        let nav_active = "posts";
        let tag = params.tag;
        Ok(
            view!("posts.index", { session: &session, cookies: &cookies, flash_success, csrf_token, is_authenticated, unread_count, nav_active, tag }),
        )
    }

    pub async fn create(
        session: Session,
        cookies: CookieJar,
    ) -> Result<impl IntoResponse, AppError> {
        let csrf_token = larust_http::csrf::token(&session).await;
        let is_authenticated = true;
        let unread_count = unread_count_for(&session).await?;
        let nav_active = "create";
        Ok(
            view!("posts.create", { session: &session, cookies: &cookies, csrf_token, is_authenticated, unread_count, nav_active }),
        )
    }

    pub async fn show(
        session: Session,
        cookies: CookieJar,
        post: Post,
    ) -> Result<impl IntoResponse, AppError> {
        let author_name = post
            .user()
            .await?
            .map(|author| author.name)
            .unwrap_or_else(|| "Unknown".to_string());
        // `(name, href)` pairs, not a flattened `", "`-joined string — a
        // tag here is a real link to `/posts?tag=...` (the same filtered
        // listing a list-view tag chip lands on, see `PostList`'s own
        // `TagLink`), not inert text. `href` is percent-encoded since a tag
        // name is free-form (`Post::sync_tags_from_csv` only lowercases/
        // trims it).
        let tags: Vec<(String, String)> = post
            .tags()
            .await?
            .into_iter()
            .map(|tag| {
                let encoded: String =
                    form_urlencoded::byte_serialize(tag.name.as_bytes()).collect();
                (tag.name, format!("/posts?tag={encoded}"))
            })
            .collect();

        // Public page, same as `index` — viewing doesn't require being
        // logged in, so this is an *optional* lookup (`auth::user`, not the
        // `Auth<User>` extractor `edit`/`update`/`destroy` use, which would
        // force a login redirect just to read a post). `can_manage` drives
        // whether the Edit/Delete controls render at all, so it has to be
        // the same `Post::can_manage` check those routes actually enforce
        // (owner, or a `Role::Moderator`'s `manage-posts` permission) —
        // not just ownership, or a moderator would never see the controls
        // despite being allowed to use them.
        let viewer = larust_support::auth::user::<User>(&session).await?;
        let can_manage = match &viewer {
            Some(viewer) => post.can_manage(viewer).await?,
            None => false,
        };

        // Live-updated by `CommentController::store`'s
        // `reverb::broadcast_event` for every *other* open tab on this
        // page — this initial load is only what already existed when the
        // page was requested.
        let comments = post.comments().await?;
        let comment_authors = Comment::load_user(&comments).await?;
        // Checked once here, not via `comment.can_manage(viewer)` inside
        // the loop below — that would re-run a full permission query (a
        // 3-way JOIN, see `larust_support::permission::has_permission_to`)
        // once per comment for any non-owner viewer, a real N+1 a page
        // with many comments would otherwise pay on every load. Ownership
        // is still checked per comment (a plain field comparison, no
        // query), just not the moderator fallback.
        let is_moderator = match &viewer {
            Some(viewer) => {
                larust_support::permission::has_permission_to(viewer, Permission::ManagePosts)
                    .await?
            }
            None => false,
        };
        let comments: Vec<CommentWithAuthor> = comments
            .into_iter()
            .map(|comment| {
                let author_name = comment_authors
                    .get(&comment.user_id)
                    .map(|user| user.name.clone())
                    .unwrap_or_else(|| "Unknown".to_string());
                let author_initial = author_name
                    .chars()
                    .next()
                    .map(|c| c.to_uppercase().to_string())
                    .unwrap_or_else(|| "?".to_string());
                let can_delete = match &viewer {
                    Some(viewer) => comment.user_id == viewer.id || is_moderator,
                    None => false,
                };
                CommentWithAuthor {
                    id: comment.id,
                    author_id: comment.user_id,
                    author_name,
                    author_initial,
                    body: comment.body,
                    can_delete,
                }
            })
            .collect();

        let csrf_token = larust_http::csrf::token(&session).await;
        let is_authenticated = larust_support::auth::check(&session).await?;
        let unread_count = unread_count_for(&session).await?;
        let nav_active = "posts";
        // A real, common `@js(...)` use case (Laravel's own docs show the
        // same pattern) — pushing a structured event onto a client-side
        // analytics queue. `post_analytics` is built here, server-side,
        // from real data (never trust a client to report its own page
        // view honestly), and `@js(...)` is what makes handing it to the
        // browser as safely-escaped JSON a one-liner in the template
        // instead of hand-rolled JSON-escaping.
        let post_analytics = larust_support::serde_json::json!({
            "id": post.id,
            "title": post.title,
            "tags": tags.iter().map(|(name, _)| name).collect::<Vec<_>>(),
        });
        // Embedded once via `@js(...)` (`window.__currentUserId`) so the
        // client can decide delete-visibility and suppress-your-own-
        // typing-indicator for anything that arrives *after* this initial
        // render, over the WebSocket — see `show.blade.xr`'s own comment
        // on why that can't be a server-computed `bool` the way
        // `can_delete`/`can_manage` are for what's already on the page.
        let current_user_id = viewer.as_ref().map(|user| user.id);
        Ok(view!("posts.show", {
            cookies: &cookies,
            id: post.id,
            title: post.title,
            content: post.content,
            author_name,
            tags,
            comments,
            can_manage,
            current_user_id,
            csrf_token,
            is_authenticated,
            unread_count,
            nav_active,
            post_analytics,
        }))
    }

    /// The form itself — fields, tags, the Trix editor, validation, the
    /// actual save — is entirely the `PostForm` wire component (see
    /// `app/Wire/post_form.rs`), mounted via `@wire('post-form', {
    /// post_id: post.id })` in `posts.edit`; this handler only gates the
    /// page itself (`Post::can_manage` — the post's own author, or a
    /// `Role::Moderator`'s `manage-posts` permission) and renders the
    /// shell around it.
    pub async fn edit(
        session: Session,
        cookies: CookieJar,
        Auth(user): Auth<User>,
        post: Post,
    ) -> Result<impl IntoResponse, AppError> {
        larust_support::auth::authorize(post.can_manage(&user).await?)?;
        let csrf_token = larust_http::csrf::token(&session).await;
        let is_authenticated = true;
        let unread_count = larust_support::notification::unread_count(&user).await?;
        let nav_active = "posts";
        Ok(
            view!("posts.edit", { session: &session, cookies: &cookies, post, csrf_token, is_authenticated, unread_count, nav_active }),
        )
    }

    pub async fn update(
        session: Session,
        Auth(user): Auth<User>,
        post: Post,
        request: StorePostRequest,
    ) -> Result<impl IntoResponse, AppError> {
        larust_support::auth::authorize(post.can_manage(&user).await?)?;
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
        larust_support::auth::authorize(post.can_manage(&user).await?)?;
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
