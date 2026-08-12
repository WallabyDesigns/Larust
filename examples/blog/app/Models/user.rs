use larust_support::orm::sqlx;
use larust_support::{AppError, Model};

use crate::models::Post;

#[derive(Model, sqlx::FromRow)]
#[table("users")]
#[has_many(Post, foreign_key = "user_id")]
pub struct User {
    #[primary_key]
    pub id: i64,
    pub name: String,
    pub email: String,
    pub password_hash: String,
}

impl larust_support::auth::Authenticatable for User {
    fn auth_id(&self) -> i64 {
        self.id
    }

    async fn find_for_auth(id: i64) -> Result<Option<Self>, AppError> {
        Self::find(id).await
    }
}
