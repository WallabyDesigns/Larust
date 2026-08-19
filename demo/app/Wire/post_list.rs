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
/// part of `PostList`'s own serialized state (`query`/`viewer_id` are the
/// only state that needs to survive between syncs; everything else is
/// cheap to recompute and would otherwise go stale the moment another
/// visitor published, edited, or deleted a post).

#[derive(sqlx::FromRow)]
struct PostRow {
    id: i64,
    user_id: i64,
    title: String,
    author_name: String,
    tag_names: String,
    can_manage: bool,
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
    /// Captured once, at `mount()`, from the real session — see
    /// `WireComponent::mount`'s own doc comment for why this is cached
    /// here rather than re-derived on every `render()`.
    #[serde(default)]
    viewer_id: Option<i64>,
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
        let viewer_id = larust_support::auth::id(session).await.ok().flatten();
        let csrf_token = larust_http::csrf::token(session).await;
        PostList {
            query,
            viewer_id,
            csrf_token,
        }
    }

    async fn render(&self) -> View {
        let rows = matching_posts(&self.query, self.viewer_id).await;
        let post_count = rows.len();
        view!("components.post-list", {
            query: self.query.clone(),
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
            other => Err(AppError::Http {
                status: StatusCode::NOT_FOUND,
                message: format!("component `{}` has no action `{other}`", Self::NAME),
            }),
        }
    }
}

/// An empty query returns every post (the plain, unfiltered Journal
/// listing) — unlike a dedicated search box, this component *is* the
/// listing, so "no query yet" must show something, not nothing. Filtering
/// Filtering, joining, and tag aggregation all happen in SQLite. This avoids
/// loading every post and issuing one relation query per rendered row on each
/// debounced live-search update.
async fn matching_posts(query: &str, viewer_id: Option<i64>) -> Vec<PostRow> {
    let needle = query.trim();
    let sql = r#"
        SELECT
            posts.id,
            posts.user_id,
            posts.title,
            COALESCE(users.name, 'Unknown') AS author_name,
            COALESCE(GROUP_CONCAT('#' || tags.name, ', '), '') AS tag_names,
            0 AS can_manage
        FROM posts
        LEFT JOIN users ON users.id = posts.user_id
        LEFT JOIN post_tag ON post_tag.post_id = posts.id
        LEFT JOIN tags ON tags.id = post_tag.tag_id
        WHERE (? = '' OR lower(posts.title) LIKE '%' || lower(?) || '%')
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
        row.can_manage = viewer_id == Some(row.user_id);
    }
    rows
}
