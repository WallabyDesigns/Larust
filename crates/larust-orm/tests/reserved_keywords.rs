// A separate test binary (not `integration.rs`) so its own `connect()`
// call doesn't collide with `integration.rs`'s process-wide `OnceLock` pool
// - each file under `tests/` is its own process.

#[derive(sqlx::FromRow, Debug, PartialEq)]
struct Order {
    id: i64,
    group: String,
}

#[tokio::test]
async fn table_and_column_names_that_are_sql_reserved_keywords_work() {
    let db_dir = tempfile::tempdir().unwrap();
    let database_url = format!("sqlite://{}/test.sqlite", db_dir.path().display());
    larust_orm::connect(&database_url).await.unwrap();

    let pool = larust_orm::pool().unwrap();
    // "order" (table) and "group" (column) are both SQL reserved keywords.
    sqlx::query(
        "CREATE TABLE \"order\" (id INTEGER PRIMARY KEY AUTOINCREMENT, \"group\" TEXT NOT NULL)",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO \"order\" (\"group\") VALUES (?)")
        .bind("vip")
        .execute(pool)
        .await
        .unwrap();

    let found: Option<Order> = larust_orm::QueryBuilder::new("order")
        .where_eq("group", "vip")
        .latest("id")
        .first()
        .await
        .unwrap();

    assert_eq!(
        found,
        Some(Order {
            id: 1,
            group: "vip".to_string()
        })
    );
}
