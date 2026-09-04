use larust_support::orm::sqlx;
use larust_support::AppError;
use larust_support::Model;

use crate::models::User;
use crate::permissions::Permission;

#[derive(Model, sqlx::FromRow)]
#[table("comments")]
#[belongs_to(User, foreign_key = "user_id")]
pub struct Comment {
    #[primary_key]
    pub id: i64,
    pub post_id: i64,
    pub user_id: i64,
    pub body: String,
}

impl Comment {
    /// Same shape as `Post::can_manage` - the comment's own author, or a
    /// `Role::Moderator` via the same `manage-posts` permission posts
    /// already use. Not a distinct permission: a moderator cleaning up a
    /// post's comments is the same "manage this content" authority
    /// already modeled, not a separate capability worth its own
    /// permission for this demo.
    pub async fn can_manage(&self, user: &User) -> Result<bool, AppError> {
        if self.user_id == user.id {
            return Ok(true);
        }
        larust_support::permission::has_permission_to(user, Permission::ManagePosts).await
    }
}
