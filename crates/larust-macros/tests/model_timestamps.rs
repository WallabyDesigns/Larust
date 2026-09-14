//! End-to-end proof that `#[timestamps]` (Laravel's `$table->timestamps()`
//! plus Eloquent's default auto-touch behavior) actually populates
//! `created_at`/`updated_at` through the real `create()`/`update()`
//! codegen, against a real sqlite database - mirrors `model.rs`'s own
//! `model_crud_round_trip_against_real_sqlite` in shape.

use larust_support::Model;

#[derive(Model, sqlx::FromRow, Debug, Clone)]
#[table("posts")]
#[timestamps]
pub struct Post {
    #[primary_key]
    pub id: i64,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[tokio::test]
async fn timestamps_are_stamped_on_create_and_retouched_on_update() {
    let db_dir = tempfile::tempdir().unwrap();
    let database_url = format!("sqlite://{}/test.sqlite", db_dir.path().display());
    larust_support::orm::connect(&database_url).await.unwrap();

    let migrations_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        migrations_dir.path().join("0001_create_posts.sql"),
        "CREATE TABLE posts (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            title TEXT NOT NULL, \
            created_at INTEGER NOT NULL, \
            updated_at INTEGER NOT NULL\
        );",
    )
    .unwrap();
    larust_support::orm::migrate(migrations_dir.path())
        .await
        .unwrap();

    // `NewPost` (the generated insertable struct) has no `created_at`/
    // `updated_at` fields at all - proven structurally, just by this
    // compiling: if either were still caller-supplied, this literal would
    // be missing fields and fail to build.
    let before = now_unix_secs();
    let created = Post::create(NewPost {
        title: "Hello, Larust".to_string(),
    })
    .await
    .unwrap();
    let after = now_unix_secs();

    assert!(
        created.created_at >= before && created.created_at <= after,
        "created_at should be stamped to roughly now, got {}",
        created.created_at
    );
    assert_eq!(
        created.created_at, created.updated_at,
        "a freshly created row's created_at and updated_at should match"
    );

    // Cross a real second boundary so a re-stamped `updated_at` is
    // provably different, not just coincidentally unchanged because both
    // calls landed in the same second.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

    let updated = Post::update(
        created.id,
        NewPost {
            title: "Hello, Larust (edited)".to_string(),
        },
    )
    .await
    .unwrap();

    assert_eq!(
        updated.created_at, created.created_at,
        "update() must never touch created_at"
    );
    assert!(
        updated.updated_at > created.updated_at,
        "update() must re-stamp updated_at to a later value"
    );
}

fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}
