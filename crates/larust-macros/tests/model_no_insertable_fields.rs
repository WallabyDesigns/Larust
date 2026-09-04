// A separate test binary from `model.rs`/other `tests/*.rs` files so its
// own `connect()` call doesn't collide with their process-wide `OnceLock`
// pool - each file under `tests/` is its own process.

use larust_support::Model;

#[derive(Model, sqlx::FromRow, Debug, PartialEq)]
#[table("counters")]
pub struct Counter {
    #[primary_key]
    pub id: i64,
}

#[tokio::test]
async fn model_with_no_insertable_fields_uses_default_values() {
    let db_dir = tempfile::tempdir().unwrap();
    let database_url = format!("sqlite://{}/test.sqlite", db_dir.path().display());
    larust_support::orm::connect(&database_url).await.unwrap();
    sqlx::query("CREATE TABLE counters (id INTEGER PRIMARY KEY AUTOINCREMENT)")
        .execute(larust_support::orm::pool().unwrap())
        .await
        .unwrap();

    let created = Counter::create(NewCounter {}).await.unwrap();
    assert_eq!(created.id, 1);

    let all = Counter::all().await.unwrap();
    assert_eq!(all, vec![Counter { id: 1 }]);
}
