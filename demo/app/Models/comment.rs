use larust_support::orm::sqlx;
use larust_support::Model;

use crate::models::User;

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
