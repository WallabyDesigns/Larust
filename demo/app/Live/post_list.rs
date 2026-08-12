use larust_http::session::Session;
use larust_support::axum::http::StatusCode;
use larust_support::live::LiveComponent;
use larust_support::serde_json;
use larust_support::view;
use larust_support::view::View;
use larust_support::AppError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::models::Post;

/// A single post row, already joined with its author/tags/per-viewer
/// permissions — assembled fresh on every render by `matching_posts`, not
/// part of `PostList`'s own serialized state (`query`/`viewer_id` are the
/// only state that needs to survive between syncs; everything else is
/// cheap to recompute and would otherwise go stale the moment another
/// visitor published, edited, or deleted a post).
struct PostRow {
    id: i64,
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
    /// `LiveComponent::mount`'s own doc comment for why this is cached
    /// here rather than re-derived on every `render()`.
    #[serde(default)]
    viewer_id: Option<i64>,
    /// Each post's per-row delete `<form>` needs a real `@csrf` token —
    /// `render()` doesn't receive `session` (nothing else it does needs
    /// it), so this is fetched once at `mount()` time instead. Valid for
    /// this token's whole lifetime regardless: `csrf::token` only ever
    /// generates a *new* token when the session doesn't already have one,
    /// so this is the exact same token a normal, non-live page render
    /// would have produced, not a second, competing one.
    #[serde(default)]
    csrf_token: String,
}

impl LiveComponent for PostList {
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
/// happens in Rust rather than the database — a plain demo helper matching
/// this reference app's scale, not meant to demonstrate query performance
/// (`larust_orm::QueryBuilder` has no `LIKE`-style filter yet).
async fn matching_posts(query: &str, viewer_id: Option<i64>) -> Vec<PostRow> {
    let posts = Post::all().await.unwrap_or_default();
    let authors = Post::load_user(&posts).await.unwrap_or_default();
    let needle = query.trim().to_lowercase();

    let mut rows = Vec::new();
    for post in posts {
        if !needle.is_empty() && !post.title.to_lowercase().contains(&needle) {
            continue;
        }
        let author_name = authors
            .get(&post.user_id)
            .map(|user| user.name.clone())
            .unwrap_or_else(|| "Unknown".to_string());
        let tags = post.tags().await.unwrap_or_default();
        let tag_names = tags
            .iter()
            .map(|tag| tag.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let can_manage = viewer_id == Some(post.user_id);

        rows.push(PostRow {
            id: post.id,
            title: post.title,
            author_name,
            tag_names,
            can_manage,
        });
    }
    rows
}
