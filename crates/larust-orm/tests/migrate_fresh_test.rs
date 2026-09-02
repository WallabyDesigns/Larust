//! `larust_orm::migrate_fresh` — its own test binary since `larust_orm::
//! connect()` is a process-wide singleton (same "one test function per
//! binary" convention `tests/integration.rs` already follows).

#[tokio::test]
async fn fresh_drops_every_table_except_sessions_and_reapplies_migrations() {
    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("test.sqlite");
    let database_url = format!("sqlite://{}", db_path.display());
    larust_orm::connect(&database_url).await.unwrap();

    let migrations_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        migrations_dir.path().join("0001_create_posts.sql"),
        "CREATE TABLE posts (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL);",
    )
    .unwrap();
    larust_orm::migrate(migrations_dir.path()).await.unwrap();

    let pool = larust_orm::pool().unwrap();

    // A framework-managed `sessions` table — created the same way
    // `larust_http::session`'s store creates it at boot, outside the
    // migrations directory entirely. `fresh` must leave it alone.
    sqlx::query("CREATE TABLE sessions (id TEXT PRIMARY KEY, data BLOB)")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO sessions (id, data) VALUES ('abc', X'01')")
        .execute(pool)
        .await
        .unwrap();

    sqlx::query("INSERT INTO posts (title) VALUES ('will be wiped')")
        .execute(pool)
        .await
        .unwrap();
    let count_before: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM posts")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(count_before.0, 1);

    larust_orm::migrate_fresh(migrations_dir.path())
        .await
        .unwrap();

    // `posts` exists again (migrations replayed) but is empty (really
    // dropped and recreated, not just left alone).
    let count_after: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM posts")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(count_after.0, 0);

    // `sessions` survived untouched — same row still there.
    let session_row: (String,) = sqlx::query_as("SELECT id FROM sessions")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(session_row.0, "abc");

    // `_migrations` itself was dropped and recreated — re-running `fresh`
    // again must not error on "table already exists" or similar.
    larust_orm::migrate_fresh(migrations_dir.path())
        .await
        .unwrap();
}
