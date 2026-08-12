//! Verifies `test_transaction`'s core guarantee: every call gets a
//! brand-new, fully isolated database — proven the same way the
//! `RefreshDatabase`-vs-`DatabaseTransactions` doc comment in
//! `transaction.rs` explains: two independent calls, in sequence, each
//! inserting a row with the same UNIQUE value succeed both times, since
//! neither call's database ever sees the other's data. Unlike `test_db()`
//! (see `db_test.rs`'s own doc comment), none of this needs the "one test
//! per file" workaround, since each call gets its own dedicated database
//! rather than sharing one process-wide static — two independent
//! `#[tokio::test]` fns in this one file is itself part of that proof.

use std::io::Write;

fn migrations_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mut f = std::fs::File::create(dir.path().join("0001_create_widgets.sql")).unwrap();
    write!(
        f,
        "CREATE TABLE widgets (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE);"
    )
    .unwrap();
    dir
}

async fn widget_count(pool: &sqlx::SqlitePool) -> i64 {
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM widgets")
        .fetch_one(pool)
        .await
        .unwrap();
    count
}

#[tokio::test]
async fn each_call_gets_its_own_isolated_database_even_reusing_the_same_unique_value() {
    let dir = migrations_dir();

    // A row created inside `body` is visible to `body` while it runs.
    let seen_during = larust_testing::test_transaction(dir.path(), |pool| async move {
        sqlx::query("INSERT INTO widgets (name) VALUES (?)")
            .bind("gadget")
            .execute(&pool)
            .await
            .unwrap();
        widget_count(&pool).await
    })
    .await;
    assert_eq!(seen_during, 1);

    // A *second*, independent `test_transaction` call reusing the exact
    // same unique `name` succeeds cleanly — if it shared the first
    // call's database, this INSERT would hit a UNIQUE constraint
    // violation instead.
    let seen_in_second_call = larust_testing::test_transaction(dir.path(), |pool| async move {
        sqlx::query("INSERT INTO widgets (name) VALUES (?)")
            .bind("gadget")
            .execute(&pool)
            .await
            .unwrap();
        widget_count(&pool).await
    })
    .await;
    assert_eq!(
        seen_in_second_call, 1,
        "the second call's own insert should be the only row it ever sees — \
         a UNIQUE-constraint error here would mean it shared the first call's database"
    );
}

/// A second, independent `#[tokio::test]` fn in this same file, also
/// calling `test_transaction` — proving this genuinely doesn't need the
/// "one test per file" constraint. If `test_transaction` secretly shared
/// process-wide state the way `test_db()`/`larust_orm::connect()` do,
/// this and the test above running concurrently (`cargo test`'s default)
/// would interfere with each other.
#[tokio::test]
async fn a_second_independent_test_in_the_same_file_is_unaffected_by_the_other() {
    let dir = migrations_dir();

    let count = larust_testing::test_transaction(dir.path(), |pool| async move {
        sqlx::query("INSERT INTO widgets (name) VALUES (?)")
            .bind("widget-from-a-different-test-fn")
            .execute(&pool)
            .await
            .unwrap();
        widget_count(&pool).await
    })
    .await;
    assert_eq!(count, 1);
}
