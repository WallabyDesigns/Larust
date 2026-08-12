// Separate test binary — see the comment at the top of `cache_test.rs` for
// why each scenario needing its own `larust_orm::connect()` gets its own
// file.

use std::sync::atomic::{AtomicUsize, Ordering};

async fn connect_test_db() {
    let dir = tempfile::tempdir().unwrap().keep();
    let database_url = format!("sqlite://{}/test.sqlite", dir.display());
    larust_orm::connect(&database_url).await.unwrap();
}

#[tokio::test]
async fn remember_only_calls_the_closure_on_a_miss() {
    connect_test_db().await;

    let calls = AtomicUsize::new(0);

    let first = larust_cache::remember("expensive", std::time::Duration::from_secs(60), || async {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok::<i64, larust_core::AppError>(42)
    })
    .await
    .unwrap();
    assert_eq!(first, 42);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Second call hits the cache — the closure must not run again.
    let second =
        larust_cache::remember("expensive", std::time::Duration::from_secs(60), || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok::<i64, larust_core::AppError>(999)
        })
        .await
        .unwrap();
    assert_eq!(second, 42);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
