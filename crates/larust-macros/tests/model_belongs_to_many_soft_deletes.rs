//! Proves `#[belongs_to_many(...)]`'s lazy accessor respects the related
//! model's own `#[soft_deletes]` - see `belongs_to_many.rs`'s own doc
//! comment for why this has to be a runtime check (`<Related as
//! Model>::SOFT_DELETE_COLUMN`), not something the macro can see at
//! expansion time.

use larust_support::Model;

#[derive(Model, sqlx::FromRow, Debug, PartialEq, Clone)]
#[table("posts")]
#[belongs_to_many(
    Tag,
    through = "post_tag",
    foreign_key = "post_id",
    related_pivot_key = "tag_id"
)]
pub struct Post {
    #[primary_key]
    pub id: i64,
    pub title: String,
}

#[derive(Model, sqlx::FromRow, Debug, PartialEq, Clone)]
#[table("tags")]
#[soft_deletes]
pub struct Tag {
    #[primary_key]
    pub id: i64,
    pub name: String,
    pub deleted_at: Option<i64>,
}

#[tokio::test]
async fn belongs_to_many_excludes_a_soft_deleted_related_row() {
    let db_dir = tempfile::tempdir().unwrap();
    let database_url = format!("sqlite://{}/test.sqlite", db_dir.path().display());
    larust_support::orm::connect(&database_url).await.unwrap();

    let migrations_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        migrations_dir.path().join("0001_create_tables.sql"),
        "CREATE TABLE posts (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL); \
         CREATE TABLE tags (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, deleted_at INTEGER); \
         CREATE TABLE post_tag (post_id INTEGER NOT NULL, tag_id INTEGER NOT NULL, PRIMARY KEY (post_id, tag_id));",
    )
    .unwrap();
    larust_support::orm::migrate(migrations_dir.path())
        .await
        .unwrap();

    let post = Post::create(NewPost {
        title: "Hello, Larust".to_string(),
    })
    .await
    .unwrap();
    let rust_tag = Tag::create(NewTag {
        name: "rust".to_string(),
    })
    .await
    .unwrap();
    let web_tag = Tag::create(NewTag {
        name: "web".to_string(),
    })
    .await
    .unwrap();
    post.attach_tag(rust_tag.id).await.unwrap();
    post.attach_tag(web_tag.id).await.unwrap();
    assert_eq!(post.tags().await.unwrap().len(), 2);

    // Soft-deleting a related row - the pivot row is untouched, but the
    // accessor must stop returning it, the same way a direct
    // `Tag::query()` would exclude it.
    Tag::delete(web_tag.id).await.unwrap();
    let tags = post.tags().await.unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].id, rust_tag.id);

    // Restoring it makes it visible through the accessor again.
    Tag::restore(web_tag.id).await.unwrap();
    let mut tags_after_restore = post.tags().await.unwrap();
    tags_after_restore.sort_by_key(|t| t.id);
    assert_eq!(tags_after_restore.len(), 2);
}
