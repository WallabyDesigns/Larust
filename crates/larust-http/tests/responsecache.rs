// A single test binary, one `#[tokio::test]` fn covering the whole
// scenario - same reasoning as `larust-cache/tests/cache_test.rs`:
// `larust_orm::connect()` can only succeed once per process, and every
// `#[tokio::test]` fn within one file shares that one process.

use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, StatusCode};
use axum::response::Response;
use larust_http::{responsecache, Route};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tower::ServiceExt;

async fn connect_test_db() {
    let dir = tempfile::tempdir().unwrap().keep();
    let database_url = format!("sqlite://{}/test.sqlite", dir.display());
    larust_orm::connect(&database_url).await.unwrap();
}

/// Extracts just the cookie's own name=value pair from a `Set-Cookie`
/// response header - same trick `middleware_dsl.rs`'s own CSRF tests use
/// to replay a session cookie on a later request.
fn cookie_from(response: &Response) -> String {
    response
        .headers()
        .get(header::SET_COOKIE)
        .expect("a session-backed router should set a session cookie")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
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

    // A non-GET to the same path always bypasses the cache - its own
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

    // `for_minutes_per_session`: two distinct sessions hitting the same
    // URL must never see each other's cached response - the literal fix
    // for this module's own former "no per-user caching" limitation.
    let session_hits = Arc::new(AtomicUsize::new(0));
    let counted_hits = session_hits.clone();
    let pool = larust_orm::pool().unwrap();
    let session_router = Route::get(
        "/dashboard",
        move |session: larust_http::session::Session| {
            let hits = counted_hits.clone();
            async move {
                // A real app's session is already populated (login, CSRF,
                // flash data, ...) by the time a per-session-cached route
                // runs - an unmodified session issues no cookie at all
                // (tower_sessions' own lazy-persistence behavior), so this
                // write just stands in for that, forcing a real session id
                // to exist for this test to key against.
                session.insert("touched", true).await.unwrap();
                let count = hits.fetch_add(1, Ordering::SeqCst) + 1;
                format!("render {count}")
            }
        },
    )
    .middleware(responsecache::for_minutes_per_session(5))
    .with_sessions(pool, true)
    .await
    .unwrap()
    .into_axum_router();

    // Session A's first-ever request (no cookie yet) is never cacheable -
    // see `middleware_per_session`'s own doc comment: `tower_sessions`
    // only assigns a brand-new session's id in the *outer* session
    // layer's post-processing, strictly after this request has already
    // finished, so there's no id yet for this one request to key a cache
    // entry on. It still renders correctly; it just can't be stored.
    let a_first = session_router
        .clone()
        .oneshot(Request::get("/dashboard").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let a_cookie = cookie_from(&a_first);
    let a_first_body = axum::body::to_bytes(a_first.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&a_first_body[..], b"render 1");

    // Session B (no cookie yet - a distinct, fresh session) is in the
    // same "first-ever request" position as A was - also uncacheable yet,
    // and critically must never see A's session id or any cached entry
    // of A's.
    let b_first = session_router
        .clone()
        .oneshot(Request::get("/dashboard").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let b_cookie = cookie_from(&b_first);
    assert_ne!(
        a_cookie, b_cookie,
        "two fresh requests must get distinct sessions"
    );
    let b_first_body = axum::body::to_bytes(b_first.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        &b_first_body[..],
        b"render 2",
        "a different session must not reuse session A's response"
    );
    assert_eq!(session_hits.load(Ordering::SeqCst), 2);

    // Session A's SECOND request - now replaying its real cookie, so
    // `session.id()` is populated from the start. Nothing has been
    // stored for A yet (its first request couldn't be), so this is a
    // genuine cache MISS: the handler runs again, and this response is
    // what actually gets cached.
    let a_second = session_router
        .clone()
        .oneshot(
            Request::get("/dashboard")
                .header(header::COOKIE, &a_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let a_second_body = axum::body::to_bytes(a_second.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        &a_second_body[..],
        b"render 3",
        "A's second request is a cache miss (nothing was stored on its first) and runs for real"
    );
    assert_eq!(session_hits.load(Ordering::SeqCst), 3);

    // Session A's THIRD request - the real cache hit: still "render 3",
    // handler does not run again.
    let a_third = session_router
        .oneshot(
            Request::get("/dashboard")
                .header(header::COOKIE, &a_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let a_third_body = axum::body::to_bytes(a_third.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        &a_third_body[..],
        b"render 3",
        "A's third request must hit the entry stored on its second"
    );
    assert_eq!(
        session_hits.load(Ordering::SeqCst),
        3,
        "the handler must not run again for a real per-session cache hit"
    );
}
