//! End-to-end proof that `#[soft_deletes]` (Laravel's `SoftDeletes` trait)
//! actually filters/un-filters through the real generated codegen,
//! against a real SQLite database - mirrors `model.rs`'s own
//! `model_crud_round_trip_against_real_sqlite` in shape.

use larust_support::Model;

#[derive(Model, sqlx::FromRow, Debug, PartialEq, Clone)]
#[table("posts")]
#[soft_deletes]
pub struct Post {
    #[primary_key]
    pub id: i64,
    pub title: String,
    pub deleted_at: Option<i64>,
}

#[tokio::test]
async fn soft_deletes_filter_and_unfilter_through_the_real_generated_codegen() {
    let db_dir = tempfile::tempdir().unwrap();
    let database_url = format!("sqlite://{}/test.sqlite", db_dir.path().display());
    larust_support::orm::connect(&database_url).await.unwrap();

    let migrations_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        migrations_dir.path().join("0001_create_posts.sql"),
        "CREATE TABLE posts (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            title TEXT NOT NULL, \
            deleted_at INTEGER\
        );",
    )
    .unwrap();
    larust_support::orm::migrate(migrations_dir.path())
        .await
        .unwrap();

    // `NewPost` has no `deleted_at` field at all - proven structurally,
    // just by this compiling.
    let a = Post::create(NewPost {
        title: "First".to_string(),
    })
    .await
    .unwrap();
    let b = Post::create(NewPost {
        title: "Second".to_string(),
    })
    .await
    .unwrap();
    assert!(!a.trashed());

    // A freshly created row is never trashed, so every normal read path
    // already sees it.
    assert_eq!(Post::all().await.unwrap().len(), 2);
    assert_eq!(Post::find(a.id).await.unwrap(), Some(a.clone()));

    // delete() soft-deletes: the row still physically exists, but every
    // normal read path (query()/find()/all()) now excludes it.
    Post::delete(a.id).await.unwrap();
    assert_eq!(Post::find(a.id).await.unwrap(), None);
    let remaining = Post::all().await.unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, b.id);

    // find_with_trashed/with_trashed see it anyway, and report it as
    // trashed.
    let trashed = Post::find_with_trashed(a.id).await.unwrap().unwrap();
    assert!(trashed.trashed());
    let all_with_trashed = Post::query().with_trashed().get().await.unwrap();
    assert_eq!(all_with_trashed.len(), 2);

    // only_trashed sees exactly the trashed row, nothing else.
    let only_trashed = Post::query().only_trashed().get().await.unwrap();
    assert_eq!(only_trashed.len(), 1);
    assert_eq!(only_trashed[0].id, a.id);

    // Deleting an already-trashed row again must not re-stamp
    // deleted_at with a later timestamp.
    let deleted_at_before = trashed.deleted_at;
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    Post::delete(a.id).await.unwrap();
    let trashed_again = Post::find_with_trashed(a.id).await.unwrap().unwrap();
    assert_eq!(trashed_again.deleted_at, deleted_at_before);

    // restore() un-deletes it - it's visible through normal reads again.
    Post::restore(a.id).await.unwrap();
    let restored = Post::find(a.id).await.unwrap().unwrap();
    assert!(!restored.trashed());
    assert_eq!(Post::all().await.unwrap().len(), 2);

    // force_delete() bypasses deleted_at entirely - a real, permanent
    // removal, invisible even to with_trashed().
    Post::force_delete(a.id).await.unwrap();
    assert_eq!(Post::find_with_trashed(a.id).await.unwrap(), None);
    assert_eq!(Post::query().with_trashed().get().await.unwrap().len(), 1);
}
