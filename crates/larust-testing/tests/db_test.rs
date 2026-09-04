//! Proves `test_db()` runs migrations and is idempotent within one
//! process. Deliberately a single `#[tokio::test]` fn, not several -
//! `cargo test` doesn't guarantee execution order between separate test
//! functions in the same file (they may even run concurrently), so
//! "idempotent across repeated calls" has to be asserted within one
//! function's own sequential steps to be a real, order-independent proof.

use std::io::Write;

fn migrations_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mut f = std::fs::File::create(dir.path().join("0001_create_widgets.sql")).unwrap();
    write!(
        f,
        "CREATE TABLE widgets (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL);"
    )
    .unwrap();
    dir
}

#[tokio::test]
async fn test_db_runs_migrations_and_is_idempotent_within_one_process() {
    let dir = migrations_dir();
    let pool = larust_testing::test_db(dir.path()).await.unwrap();

    sqlx::query("INSERT INTO widgets (name) VALUES (?)")
        .bind("gadget")
        .execute(&pool)
        .await
        .unwrap();

    // A second call - with a *different*, unused migrations dir argument,
    // deliberately, to prove it's ignored - must return a pool still
    // backed by the exact same already-migrated database, not a fresh
    // one, and not an error from `connect()`'s own "second call errors"
    // behavior leaking through.
    let unused_dir = migrations_dir();
    let pool_again = larust_testing::test_db(unused_dir.path()).await.unwrap();

    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM widgets")
        .fetch_one(&pool_again)
        .await
        .unwrap();
    assert_eq!(
        count.0, 1,
        "the second test_db() call should see the row inserted through the first \
         call's pool, proving both calls share one process-wide connection"
    );
}
