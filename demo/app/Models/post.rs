use larust_support::orm::sqlx;
use larust_support::AppError;
use larust_support::Model;

use crate::models::{Comment, Tag, User};
use crate::permissions::Permission;

#[derive(Model, sqlx::FromRow)]
#[table("posts")]
#[belongs_to(User, foreign_key = "user_id")]
#[belongs_to_many(
    Tag,
    through = "post_tag",
    foreign_key = "post_id",
    related_pivot_key = "tag_id"
)]
#[has_many(Comment, foreign_key = "post_id")]
pub struct Post {
    #[primary_key]
    pub id: i64,
    pub user_id: i64,
    pub title: String,
    pub content: String,
}

impl Post {
    /// The real authorization check for editing/deleting this post -
    /// `PostPolicy::update`/`delete` (`app/Policies/post_policy.rs`) stay
    /// synchronous and ownership-only (`self.user_id == user.id`), by
    /// design (`Policy`'s own methods can't be `async`); this is the
    /// async layer on top that also lets a `Role::Moderator` manage a
    /// post they don't own, via `larust-permissions`. Controllers call
    /// this instead of `post.authorize_update(&user)`/`authorize_delete`
    /// directly.
    pub async fn can_manage(&self, user: &User) -> Result<bool, AppError> {
        if self.user_id == user.id {
            return Ok(true);
        }
        larust_support::permission::has_permission_to(user, Permission::ManagePosts).await
    }

    /// Replaces this post's tag set with the comma-separated names in
    /// `tags_csv`, case-insensitively deduped: `sync_tags` inserts one
    /// pivot row per id and the pivot table's primary key is
    /// `(post_id, tag_id)`, so a repeated name (e.g. "rust, rust") would
    /// otherwise hit a UNIQUE-constraint error instead of just meaning one
    /// tag. Shared by `PostController::store`/`update` and the reactive
    /// `PostForm` wire component (`app/Wire/post_form.rs`) - a post's tags
    /// are fully replaced by whatever was submitted, same as Laravel's own
    /// `sync()`, not merged with what was there before.
    pub async fn sync_tags_from_csv(&self, tags_csv: &str) -> Result<(), AppError> {
        let mut seen = std::collections::HashSet::new();
        let tag_names: Vec<String> = tags_csv
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_lowercase)
            .filter(|name| seen.insert(name.clone()))
            .collect();
        let pool = larust_support::orm::pool()?;
        let mut tx = pool
            .begin()
            .await
            .map_err(|error| AppError::Internal(Box::new(error)))?;

        let mut tag_ids = Vec::with_capacity(tag_names.len());
        for name in tag_names {
            larust_support::orm::sqlx::query("INSERT OR IGNORE INTO tags (name) VALUES (?)")
                .bind(&name)
                .execute(&mut *tx)
                .await
                .map_err(|error| AppError::Internal(Box::new(error)))?;
            let (tag_id,): (i64,) =
                larust_support::orm::sqlx::query_as("SELECT id FROM tags WHERE name = ?")
                    .bind(&name)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|error| AppError::Internal(Box::new(error)))?;
            tag_ids.push(tag_id);
        }

        larust_support::orm::sqlx::query("DELETE FROM post_tag WHERE post_id = ?")
            .bind(self.id)
            .execute(&mut *tx)
            .await
            .map_err(|error| AppError::Internal(Box::new(error)))?;
        for tag_id in tag_ids {
            larust_support::orm::sqlx::query(
                "INSERT INTO post_tag (post_id, tag_id) VALUES (?, ?)",
            )
            .bind(self.id)
            .bind(tag_id)
            .execute(&mut *tx)
            .await
            .map_err(|error| AppError::Internal(Box::new(error)))?;
        }

        tx.commit()
            .await
            .map_err(|error| AppError::Internal(Box::new(error)))
    }
}
