use larust_support::repository::Repository;
use larust_support::Model;

#[derive(Model, sqlx::FromRow, Debug, PartialEq, Clone)]
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

    let remaining_id = all_after_delete[0].id;
    let updated = Post::update(
        remaining_id,
        NewPost {
            title: "Second post, edited".to_string(),
        },
    )
    .await
    .unwrap();
    assert_eq!(updated.id, remaining_id);
    assert_eq!(updated.title, "Second post, edited");
    let refetched = Post::find(remaining_id).await.unwrap().unwrap();
    assert_eq!(refetched.title, "Second post, edited");

    // `Repository<Post>` conformance, via the generic `AnyRepository<T>`
    // marker `#[derive(Model)]` implements it for - proves a
    // `#[derive(Model)]` struct is usable through the storage-agnostic
    // trait, not just through its own inherent methods.
    let repository = larust_support::orm::AnyRepository::<Post>::new();
    let via_repository = repository.find(remaining_id).await.unwrap();
    assert_eq!(via_repository, Some(refetched.clone()));

    let created_via_repository = repository
        .create(Post {
            id: 0,
            title: "Created via Repository".to_string(),
        })
        .await
        .unwrap();
    assert_eq!(created_via_repository.title, "Created via Repository");

    let updated_via_repository = repository
        .update(
            created_via_repository.id,
            Post {
                id: 0,
                title: "Updated via Repository".to_string(),
            },
        )
        .await
        .unwrap();
    assert_eq!(updated_via_repository.title, "Updated via Repository");

    let queried_via_repository = repository
        .query(Post::query().where_eq(Post::TITLE, "Updated via Repository"))
        .await
        .unwrap();
    assert_eq!(queried_via_repository.len(), 1);
    assert_eq!(queried_via_repository[0].id, created_via_repository.id);

    repository.delete(created_via_repository.id).await.unwrap();
    assert_eq!(
        repository.find(created_via_repository.id).await.unwrap(),
        None
    );
}
