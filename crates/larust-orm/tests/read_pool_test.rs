use std::time::{Duration, Instant};

#[derive(sqlx::FromRow, Debug)]
struct Post {
    #[allow(dead_code)]
    id: i64,
    #[allow(dead_code)]
    title: String,
}

#[tokio::test]
async fn read_pool_is_a_distinct_sqlite_pool_that_never_serializes_behind_a_slow_write() {
    let db_dir = tempfile::tempdir().unwrap();
    let database_url = format!("sqlite://{}/test.sqlite", db_dir.path().display());
    larust_orm::connect(&database_url).await.unwrap();

    let migrations_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        migrations_dir.path().join("0001_create_posts.sql"),
        "CREATE TABLE posts (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL);",
    )
    .unwrap();
    larust_orm::migrate(migrations_dir.path()).await.unwrap();

    // 1. `read_pool()` is a genuinely distinct pool from `pool()` for SQLite.
    let write_pool = larust_orm::pool().unwrap();
    let reader = larust_orm::read_pool().unwrap();
    assert!(
        !std::ptr::eq(write_pool, reader),
        "read_pool() should be a separate pool from pool() for SQLite"
    );

    sqlx::query("INSERT INTO posts (title) VALUES ('seed')")
        .execute(write_pool)
        .await
        .unwrap();

    // 2. Concurrent reads complete quickly even while a write transaction
    // is held open and slow to commit - they must not serialize behind it.
    let write_pool_owned = write_pool.clone();
    let writer = tokio::spawn(async move {
        let mut tx = write_pool_owned.begin().await.unwrap();
        sqlx::query("INSERT INTO posts (title) VALUES ('slow')")
            .execute(&mut *tx)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        tx.commit().await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await; // let the write start first
    let start = Instant::now();
    let readers: Vec<_> = (0..10)
        .map(|_| {
            tokio::spawn(async {
                larust_orm::QueryBuilder::<Post>::new("posts")
                    .count()
                    .await
                    .unwrap()
            })
        })
        .collect();
    for r in readers {
        r.await.unwrap();
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(250),
        "reads took {elapsed:?}, appear to have serialized behind the write"
    );
    writer.await.unwrap();

    // 3. Engine-level enforcement: a write attempted against `read_pool()`
    // fails with SQLite's real read-only error, not an app-level check.
    let err = sqlx::query("INSERT INTO posts (title) VALUES ('rejected')")
        .execute(reader)
        .await
        .unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("readonly")
            || err.to_string().to_lowercase().contains("read-only"),
        "expected a read-only error, got: {err}"
    );
}
