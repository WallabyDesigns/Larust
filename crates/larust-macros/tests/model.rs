use larust_support::Model;

#[derive(Model, sqlx::FromRow, Debug, PartialEq)]
#[table("posts")]
pub struct Post {
    #[primary_key]
    pub id: i64,
    pub title: String,
}

#[tokio::test]
async fn model_crud_round_trip_against_real_sqlite() {
    let db_dir = tempfile::tempdir().unwrap();
    let database_url = format!("sqlite://{}/test.sqlite", db_dir.path().display());
    larust_support::orm::connect(&database_url).await.unwrap();

    let migrations_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        migrations_dir.path().join("0001_create_posts.sql"),
        "CREATE TABLE posts (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL);",
    )
    .unwrap();
    larust_support::orm::migrate(migrations_dir.path())
        .await
        .unwrap();

    assert_eq!(Post::TABLE, "posts");
    assert_eq!(Post::ID, "id");
    assert_eq!(Post::TITLE, "title");

    let created = Post::create(NewPost {
        title: "Hello, Larust".to_string(),
    })
    .await
    .unwrap();
    assert_eq!(created.id, 1);
    assert_eq!(created.title, "Hello, Larust");

    let found = Post::find(created.id).await.unwrap();
    assert_eq!(found, Some(created));

    let missing = Post::find(999).await.unwrap();
    assert_eq!(missing, None);

    Post::create(NewPost {
        title: "Second post".to_string(),
    })
    .await
    .unwrap();
    let all = Post::all().await.unwrap();
    assert_eq!(all.len(), 2);

    Post::delete(1).await.unwrap();
    let all_after_delete = Post::all().await.unwrap();
    assert_eq!(all_after_delete.len(), 1);
    assert_eq!(all_after_delete[0].title, "Second post");
}
