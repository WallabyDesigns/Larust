use larust_http::session::Session;
use larust_support::axum::http::StatusCode;
use larust_support::orm::sqlx;
use larust_support::serde_json;
use larust_support::view;
use larust_support::view::View;
use larust_support::wire::WireComponent;
use larust_support::AppError;
use larust_support::WithLoop;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A single post row, already joined with its author/tags/per-viewer
/// permissions — assembled fresh on every render by `matching_posts`, not
/// part of `PostList`'s own serialized state (`query`/`tag`/`viewer_id` are
/// the only state that needs to survive between syncs; everything else is
/// cheap to recompute and would otherwise go stale the moment another
/// visitor published, edited, or deleted a post).

#[derive(sqlx::FromRow)]
struct PostRow {
    id: i64,
    user_id: i64,
    title: String,
    author_name: String,
    /// Raw `', '`-joined tag names straight off `GROUP_CONCAT` — never
    /// rendered directly (that was the bug: the whole list view showed one
    /// flat, unclickable string). [`tags`](PostRow::tags) is what templates
    /// actually use; this column only exists because `sqlx::FromRow` needs
    /// something to bind `GROUP_CONCAT`'s own single output column to.
    tag_names: String,
    can_manage: bool,
    /// Computed from `tag_names` right after fetch (see the loop at the end
    /// of `matching_posts`) — one clickable chip per tag, each linking to
    /// `/posts?tag=...` (`PostController::index`'s own query param) so
    /// clicking a tag actually filters the listing instead of being inert
    /// decoration.
    #[sqlx(skip)]
    tags: Vec<TagLink>,
}

struct TagLink {
    name: String,
    /// Percent-encoded via `form_urlencoded` — a tag name is free-form text
    /// (`Post::sync_tags_from_csv` only lowercases/trims it, nothing stops
    /// a space, `&`, or `#` from ending up in one), so it can't be spliced
    /// into a query string unescaped.
    href: String,
}

/// The Journal's post listing *and* its live search, as one component —
/// the reference example for embedding `wire:model.live` directly into an
/// existing listing page as a filter, rather than as a separate page.
/// Replaces what used to be `PostController::index`'s own static
/// `@foreach`-rendered grid; that controller method now just renders the
/// page shell and mounts this.
#[derive(Debug, Serialize, Deserialize)]
pub struct PostList {
    query: String,
    /// The active tag filter, if any — empty means unfiltered, the same
    /// "empty string, not `Option`" convention `query` already uses.
    /// Initially set from `PostController::index`'s own `?tag=` query
    /// param (via the `tag` prop `posts.index.blade.xr` mounts this with);
    /// [`WireComponent::call`]'s `"clear_tag"` action clears it from
    /// inside an already-mounted component without a page reload.
    #[serde(default)]
    tag: String,
    /// Captured once, at `mount()`, from the real session — see
    /// `WireComponent::mount`'s own doc comment for why this is cached
    /// here rather than re-derived on every `render()`.
    #[serde(default)]
    viewer_id: Option<i64>,
    /// Whether the viewer can manage *any* post, not just their own — a
    /// `Role::Moderator`'s `manage-posts` permission (see `Post::can_
    /// manage`). Checked once here, at `mount()`, rather than per-row in
    /// `matching_posts` (a DB round trip per rendered row would be a real
    /// N+1 for a listing page); a plain ownership compare stays per-row
    /// since it's free (`row.user_id == viewer_id`, no query needed).
    #[serde(default)]
    can_manage_any: bool,
    /// Each post's per-row delete `<form>` needs a real `@csrf` token —
    /// `render()` doesn't receive `session` (nothing else it does needs
    /// it), so this is fetched once at `mount()` time instead. Valid for
    /// this token's whole lifetime regardless: `csrf::token` only ever
    /// generates a *new* token when the session doesn't already have one,
    /// so this is the exact same token a normal, non-wire page render
    /// would have produced, not a second, competing one.
    #[serde(default)]
    csrf_token: String,
}

impl WireComponent for PostList {
    const NAME: &'static str = "post-list";

    async fn mount(session: &Session, props: &HashMap<String, serde_json::Value>) -> Self {
        let query = props
            .get("query")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();
        let tag = props
            .get("tag")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();
        let viewer = larust_support::auth::user::<crate::models::User>(session)
            .await
            .ok()
            .flatten();
        let viewer_id = viewer.as_ref().map(|viewer| viewer.id);
        let can_manage_any = match &viewer {
            Some(viewer) => larust_support::permission::has_permission_to(
                viewer,
                crate::permissions::Permission::ManagePosts,
            )
            .await
            .unwrap_or(false),
            None => false,
        };
        let csrf_token = larust_http::csrf::token(session).await;
        PostList {
            query,
            tag,
            viewer_id,
            can_manage_any,
            csrf_token,
        }
    }

    async fn render(&self) -> View {
        let rows =
            matching_posts(&self.query, &self.tag, self.viewer_id, self.can_manage_any).await;
        let post_count = rows.len();
        view!("components.post-list", {
            query: self.query.clone(),
            tag: self.tag.clone(),
            posts: rows,
            post_count,
            csrf_token: self.csrf_token.clone(),
        })
    }

    async fn call(
        &mut self,
        _session: &Session,
        action: &str,
        _args: &serde_json::Value,
    ) -> Result<Option<String>, AppError> {
        match action {
            "clear_search" => {
                self.query.clear();
                Ok(None)
            }
            "clear_tag" => {
                self.tag.clear();
                Ok(None)
            }
            other => Err(AppError::Http {
                status: StatusCode::NOT_FOUND,
                message: format!("component `{}` has no action `{other}`", Self::NAME),
            }),
        }
    }
}

/// An empty query/tag returns every post (the plain, unfiltered Journal
/// listing) — unlike a dedicated search box, this component *is* the
/// listing, so "no filter yet" must show something, not nothing. Filtering,
/// joining, and tag aggregation all happen in SQLite. This avoids loading
/// every post and issuing one relation query per rendered row on each
/// debounced live-search update.
///
/// The tag filter is a separate `EXISTS` subquery, not a condition on the
/// same `tags`/`post_tag` join the display-side `GROUP_CONCAT` uses —
/// filtering "posts that have tag X" by adding a `WHERE tags.name = X` on
/// that join would also silently drop every *other* tag those posts have
/// from the aggregated `tag_names` column (the join itself would only ever
/// produce the one matching row per post). The subquery answers a yes/no
/// membership question without touching what the outer join aggregates.
async fn matching_posts(
    query: &str,
    tag: &str,
    viewer_id: Option<i64>,
    can_manage_any: bool,
) -> Vec<PostRow> {
    let needle = query.trim();
    let tag_needle = tag.trim();
    let sql = r#"
        SELECT
            posts.id,
            posts.user_id,
            posts.title,
            COALESCE(users.name, 'Unknown') AS author_name,
            COALESCE(GROUP_CONCAT(tags.name, ', '), '') AS tag_names,
            0 AS can_manage
        FROM posts
        LEFT JOIN users ON users.id = posts.user_id
        LEFT JOIN post_tag ON post_tag.post_id = posts.id
        LEFT JOIN tags ON tags.id = post_tag.tag_id
        WHERE (? = '' OR lower(posts.title) LIKE '%' || lower(?) || '%')
          AND (? = '' OR EXISTS (
                SELECT 1 FROM post_tag pt
                JOIN tags t ON t.id = pt.tag_id
                WHERE pt.post_id = posts.id AND lower(t.name) = lower(?)
              ))
        GROUP BY posts.id, posts.user_id, posts.title, users.name
        ORDER BY posts.id DESC
        LIMIT ?
    "#;

    let pool = match larust_support::orm::pool() {
        Ok(pool) => pool,
        Err(error) => {
            larust_support::tracing::error!(%error, "database pool is unavailable for post list");
            return Vec::new();
        }
    };
    let posts_per_page = crate::config::blog::config()["posts_per_page"]
        .as_i64()
        .unwrap_or(25);
    let mut rows = match sqlx::query_as::<_, PostRow>(sql)
        .bind(needle)
        .bind(needle)
        .bind(tag_needle)
        .bind(tag_needle)
        .bind(posts_per_page)
        .fetch_all(pool)
        .await
    {
        Ok(rows) => rows,
        Err(error) => {
            larust_support::tracing::error!(%error, "failed to load post list");
            return Vec::new();
        }
    };

    for row in &mut rows {
        row.can_manage = can_manage_any || viewer_id == Some(row.user_id);
        row.tags = row
            .tag_names
            .split(", ")
            .filter(|name| !name.is_empty())
            .map(|name| {
                let encoded: String = form_urlencoded::byte_serialize(name.as_bytes()).collect();
                TagLink {
                    name: name.to_string(),
                    href: format!("/posts?tag={encoded}"),
                }
            })
            .collect();
    }
    rows
}
