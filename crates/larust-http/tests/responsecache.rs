// A single test binary, one `#[tokio::test]` fn covering the whole
// scenario — same reasoning as `larust-cache/tests/cache_test.rs`:
// `larust_orm::connect()` can only succeed once per process, and every
// `#[tokio::test]` fn within one file shares that one process.

use axum::body::Body;
use axum::extract::Request;
use axum::http::StatusCode;
use larust_http::{responsecache, Route};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tower::ServiceExt;

async fn connect_test_db() {
    let dir = tempfile::tempdir().unwrap().keep();
    let database_url = format!("sqlite://{}/test.sqlite", dir.display());
    larust_orm::connect(&database_url).await.unwrap();
}

#[tokio::test]
async fn caches_a_200_get_response_bypasses_writes_and_forgets_on_demand() {
    connect_test_db().await;

    let hits = Arc::new(AtomicUsize::new(0));
    let ok_hits = hits.clone();
    let not_found_hits = Arc::new(AtomicUsize::new(0));
    let nf_hits = not_found_hits.clone();

    let router = Route::get("/ok", move || {
        let hits = ok_hits.clone();
        async move {
            let count = hits.fetch_add(1, Ordering::SeqCst) + 1;
            format!("hit {count}")
        }
    })
    .post("/ok", || async move { "posted" })
    .get("/missing", move || {
        let hits = nf_hits.clone();
        async move {
            hits.fetch_add(1, Ordering::SeqCst);
            StatusCode::NOT_FOUND
        }
    })
    .middleware(responsecache::for_minutes(5))
    .into_axum_router();

    // First GET runs the handler.
    let response = router
        .clone()
        .oneshot(Request::get("/ok").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&body[..], b"hit 1");
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    // Second GET is a real cache hit: same body, handler never re-runs.
    let response = router
        .clone()
        .oneshot(Request::get("/ok").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&body[..], b"hit 1");
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    // A non-GET to the same path always bypasses the cache — its own
    // handler runs directly.
    let response = router
        .clone()
        .oneshot(Request::post("/ok").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&body[..], b"posted");

    // ...and doesn't disturb the GET cache entry: still the first hit.
    let response = router
        .clone()
        .oneshot(Request::get("/ok").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&body[..], b"hit 1");
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    // A non-200 response is never cached: the handler re-runs every time.
    router
        .clone()
        .oneshot(Request::get("/missing").body(Body::empty()).unwrap())
        .await
        .unwrap();
    router
        .clone()
        .oneshot(Request::get("/missing").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(not_found_hits.load(Ordering::SeqCst), 2);

    // `forget` evicts the cached entry, so the next GET runs the handler
    // again.
    responsecache::forget("/ok").await.unwrap();
    let response = router
        .clone()
        .oneshot(Request::get("/ok").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&body[..], b"hit 2");
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}
