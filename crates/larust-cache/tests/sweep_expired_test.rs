// Separate test binary - see the comment at the top of `cache_test.rs` for
// why each scenario needing its own `larust_orm::connect()` gets its own
// file.

async fn connect_test_db() {
    let dir = tempfile::tempdir().unwrap().keep();
    let database_url = format!("sqlite://{}/test.sqlite", dir.display());
    larust_orm::connect(&database_url).await.unwrap();
}

#[tokio::test]
async fn sweep_expired_deletes_an_expired_row_without_waiting_on_the_five_minute_gate() {
    connect_test_db().await;

    larust_cache::put("short-lived", &"value", std::time::Duration::from_secs(0))
        .await
        .unwrap();

    // Calling `sweep_expired()` directly (not `get`/`put`, which only sweep
    // opportunistically at most once every five minutes) must delete the
    // already-expired row immediately - this is the whole point of the
    // unconditional variant `larust_support::maintenance::clean` calls.
    larust_cache::sweep_expired().await.unwrap();

    let pool = larust_orm::pool().unwrap();
    let remaining: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM cache_items")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(remaining.0, 0, "the expired row should have been swept");
}
