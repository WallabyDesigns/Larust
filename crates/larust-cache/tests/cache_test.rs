// A separate test binary per file (same reasoning as
// `larust-orm/tests/reserved_keywords.rs`): `larust_orm::connect()` can
// only succeed once per process, and each `tests/*.rs` file compiles to
// its own binary - but every `#[tokio::test]` fn *within* one file shares
// that one process, so a file gets exactly one `#[tokio::test]` fn,
// exercising its whole scenario end to end rather than being split into
// several fns that would collide on a second `connect()` call.

async fn connect_test_db() {
    // `.keep()` (not a dropped `TempDir` guard), same as
    // `larust-testing/src/db.rs`'s `TEST_DB` - the pool outlives this
    // function, so nothing should delete the directory out from under it.
    let dir = tempfile::tempdir().unwrap().keep();
    let database_url = format!("sqlite://{}/test.sqlite", dir.display());
    larust_orm::connect(&database_url).await.unwrap();
}

#[tokio::test]
async fn put_get_forget_overwrite_and_type_mismatch() {
    connect_test_db().await;

    // Miss on a key that was never set.
    assert_eq!(larust_cache::get::<i64>("missing").await.unwrap(), None);

    // Basic put/get round trip.
    larust_cache::put("posts.count", &7i64, std::time::Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(
        larust_cache::get::<i64>("posts.count").await.unwrap(),
        Some(7)
    );

    // Putting the same key again overwrites rather than erroring or
    // duplicating (the `ON CONFLICT ... DO UPDATE` upsert).
    larust_cache::put("posts.count", &9i64, std::time::Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(
        larust_cache::get::<i64>("posts.count").await.unwrap(),
        Some(9)
    );

    // The value was stored as an `i64`; reading it as a `String` fails to
    // deserialize. This must surface as an `Err`, not silently degrade to
    // `Ok(None)` the way an ordinary miss would - a type mismatch is a
    // caller bug, not a cache-freshness question.
    assert!(larust_cache::get::<String>("posts.count").await.is_err());

    // forget removes it; a second forget on an already-missing key is not
    // an error.
    larust_cache::forget("posts.count").await.unwrap();
    assert_eq!(larust_cache::get::<i64>("posts.count").await.unwrap(), None);
    larust_cache::forget("posts.count").await.unwrap();
}
