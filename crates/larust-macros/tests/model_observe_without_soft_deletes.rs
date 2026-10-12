//! Covers the `deleting`/`deleted` snapshot branch `model_observe.rs`
//! doesn't: a model with `#[observe(...)]` but *no* `#[soft_deletes]`,
//! where the pre-fetch must use `find`, not `find_with_trashed` (which
//! wouldn't even exist on this model - see `larust_orm::SoftDeletes`'s
//! own doc comment for why that's gated at compile time).

use larust_support::AppError;
use larust_support::Model;
use std::sync::Mutex;

static CALLS: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn record(event: String) {
    CALLS.lock().unwrap().push(event);
}

#[derive(Model, sqlx::FromRow, Debug, PartialEq, Clone)]
#[table("posts")]
#[observe(deleting = PostObserver::deleting, deleted = PostObserver::deleted)]
pub struct Post {
    #[primary_key]
    pub id: i64,
    pub title: String,
}

struct PostObserver;

impl PostObserver {
    async fn deleting(post: &Post) -> Result<(), AppError> {
        record(format!("deleting:{}", post.title));
        Ok(())
    }

    async fn deleted(post: &Post) -> Result<(), AppError> {
        record(format!("deleted:{}", post.title));
        Ok(())
    }
}

#[tokio::test]
async fn deleting_and_deleted_fire_on_a_hard_delete_model_without_soft_deletes() {
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

    let post = Post::create(NewPost {
        title: "Real delete".to_string(),
    })
    .await
    .unwrap();

    Post::delete(post.id).await.unwrap();
    assert_eq!(
        *CALLS.lock().unwrap(),
        vec![
            "deleting:Real delete".to_string(),
            "deleted:Real delete".to_string()
        ]
    );
    // A real delete, not a soft one - the row is actually gone.
    assert_eq!(Post::all().await.unwrap().len(), 0);
}
