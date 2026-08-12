use larust_support::orm::sqlx;
use larust_support::Model;

use crate::models::User;

#[derive(Model, sqlx::FromRow)]
#[table("posts")]
#[belongs_to(User, foreign_key = "user_id")]
pub struct Post {
    #[primary_key]
    pub id: i64,
    pub user_id: i64,
    pub title: String,
}
