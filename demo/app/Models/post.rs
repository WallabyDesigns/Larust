use larust_support::orm::sqlx;
use larust_support::AppError;
use larust_support::Model;

use crate::models::{NewTag, Tag, User};

#[derive(Model, sqlx::FromRow)]
#[table("posts")]
#[belongs_to(User, foreign_key = "user_id")]
#[belongs_to_many(
    Tag,
    through = "post_tag",
    foreign_key = "post_id",
    related_pivot_key = "tag_id"
)]
pub struct Post {
    #[primary_key]
    pub id: i64,
    pub user_id: i64,
    pub title: String,
    pub content: String,
}

impl Post {
    /// Replaces this post's tag set with the comma-separated names in
    /// `tags_csv`, case-insensitively deduped: `sync_tags` inserts one
    /// pivot row per id and the pivot table's primary key is
    /// `(post_id, tag_id)`, so a repeated name (e.g. "rust, rust") would
    /// otherwise hit a UNIQUE-constraint error instead of just meaning one
    /// tag. Shared by `PostController::store`/`update` and the reactive
    /// `PostForm` wire component (`app/Wire/post_form.rs`) — a post's tags
    /// are fully replaced by whatever was submitted, same as Laravel's own
    /// `sync()`, not merged with what was there before.
    pub async fn sync_tags_from_csv(&self, tags_csv: &str) -> Result<(), AppError> {
        let mut seen = std::collections::HashSet::new();
        let tag_names: Vec<&str> = tags_csv
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .filter(|name| seen.insert(name.to_lowercase()))
            .collect();
        let mut tag_ids = Vec::with_capacity(tag_names.len());
        for name in tag_names {
            tag_ids.push(Self::find_or_create_tag(name).await?.id);
        }
        self.sync_tags(&tag_ids).await
    }

    /// Not race-safe under concurrent requests creating the same
    /// brand-new tag name (a `first()` miss followed by a `create()` on
    /// two overlapping requests both trying to insert the same `UNIQUE`
    /// name) — acceptable for a demo app; a real one would want an
    /// `INSERT OR IGNORE`-then-`SELECT` pattern instead.
    async fn find_or_create_tag(name: &str) -> Result<Tag, AppError> {
        if let Some(existing) = Tag::query().where_eq(Tag::NAME, name).first().await? {
            return Ok(existing);
        }
        Tag::create(NewTag {
            name: name.to_string(),
        })
        .await
    }
}
