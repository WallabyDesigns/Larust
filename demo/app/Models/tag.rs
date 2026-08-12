use larust_support::orm::sqlx;
use larust_support::Model;

#[derive(Model, sqlx::FromRow)]
#[table("tags")]
pub struct Tag {
    #[primary_key]
    pub id: i64,
    pub name: String,
}
