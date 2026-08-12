// Separate test binary — see the comment at the top of `cache_test.rs` for
// why each scenario needing its own `larust_orm::connect()` gets its own
// file.

async fn connect_test_db() {
    let dir = tempfile::tempdir().unwrap().keep();
    let database_url = format!("sqlite://{}/test.sqlite", dir.display());
    larust_orm::connect(&database_url).await.unwrap();
}

#[tokio::test]
async fn expired_entries_read_back_as_a_miss_and_are_evicted() {
    connect_test_db().await;

    larust_cache::put("short-lived", &"value", std::time::Duration::from_secs(0))
        .await
        .unwrap();

    // `ttl` of 0 means "expires_at == now", and `get` treats `expires_at <=
    // now` as already expired — no sleep needed to observe the miss.
    assert_eq!(
        larust_cache::get::<String>("short-lived").await.unwrap(),
        None
    );

    // The expired row was evicted as a side effect of that `get`, not just
    // masked — re-putting the same key under a long TTL and reading it
    // back proves the table isn't left holding a stale row under the same
    // key that a naive upsert could have collided with.
    larust_cache::put(
        "short-lived",
        &"fresh".to_string(),
        std::time::Duration::from_secs(60),
    )
    .await
    .unwrap();
    assert_eq!(
        larust_cache::get::<String>("short-lived").await.unwrap(),
        Some("fresh".to_string())
    );
}
