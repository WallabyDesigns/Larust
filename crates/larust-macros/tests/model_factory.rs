use larust_support::orm::Factory;
use larust_support::Model;

#[derive(Model, sqlx::FromRow, Debug, PartialEq, Clone)]
#[table("posts")]
pub struct Post {
    #[primary_key]
    pub id: i64,
    pub title: String,
    pub user_id: i64,
}

pub struct PostFactory;

impl Factory for PostFactory {
    type Model = Post;

    fn definition(seq: u64) -> NewPost {
        NewPost {
            title: format!("Post {seq}"),
            user_id: 0,
        }
    }
}

#[tokio::test]
async fn factory_builds_fake_rows_and_optionally_persists_them() {
    // `make()`/`make_many()` touch no database at all.
    let unsaved = PostFactory::new().make();
    assert!(unsaved.title.starts_with("Post "));
    assert_eq!(unsaved.user_id, 0);

    let overridden = PostFactory::new()
        .state(|p| NewPost { user_id: 42, ..p })
        .make();
    assert_eq!(overridden.user_id, 42);

    let many = PostFactory::new().make_many(3);
    assert_eq!(many.len(), 3);
    // Each call advances the shared sequence counter, so titles differ.
    assert_ne!(many[0].title, many[1].title);
    assert_ne!(many[1].title, many[2].title);

    // `create()`/`create_many()` do need a real database.
    let db_dir = tempfile::tempdir().unwrap();
    let database_url = format!("sqlite://{}/test.sqlite", db_dir.path().display());
    larust_support::orm::connect(&database_url).await.unwrap();

    let migrations_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        migrations_dir.path().join("0001_create_posts.sql"),
        "CREATE TABLE posts (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, user_id INTEGER NOT NULL);",
    )
    .unwrap();
    larust_support::orm::migrate(migrations_dir.path())
        .await
        .unwrap();

    let created = PostFactory::new()
        .state(|p| NewPost { user_id: 7, ..p })
        .create()
        .await
        .unwrap();
    assert_eq!(created.user_id, 7);
    assert!(created.title.starts_with("Post "));

    let created_many = PostFactory::new().create_many(2).await.unwrap();
    assert_eq!(created_many.len(), 2);
    assert_ne!(created_many[0].id, created_many[1].id);

    // Every row created above (1 + 2 = 3) really landed in the database.
    let all = Post::all().await.unwrap();
    assert_eq!(all.len(), 3);
}
