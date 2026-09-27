//! End-to-end proof of `SESSION_DRIVER=file` - a real `Application::
//! at_root(...)` (so `Config`/`AppPaths` are genuinely published the same
//! way any real app's `main.rs` does, unlike this crate's other session
//! tests, which never construct an `Application` at all and so always
//! fall back to the database backend - see `session_layer`'s own doc
//! comment), proving `resolve_backend` actually picks `FileSessionStore`
//! and that sessions round-trip as real files under `storage/sessions/`,
//! not just through the in-process `FileSessionStore` unit tests in
//! `larust-http/src/session.rs` itself.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use larust_core::Application;
use larust_http::session::Session;
use tower::ServiceExt;

fn file_driver_config() -> serde_json::Value {
    // The exact shape a real generated `config/app.rs` produces once
    // `SESSION_DRIVER=file` is set in `.env` - `Application::new`'s own
    // `config: fn() -> serde_json::Value` parameter can't be a capturing
    // closure, so a fixed literal here (rather than reading a real `.env`)
    // is the direct, honest equivalent.
    serde_json::json!({ "session_driver": "file" })
}

/// `Application::new`/`at_root` publish `Config`/`AppPaths` as real,
/// process-wide `OnceLock`s (see `larust_core::config`/`paths`'s own doc
/// comments) - settable exactly once per process, the identical
/// constraint `larust_orm::connect()` already has, and for the same
/// reason every other test suite in this codebase works around it: call
/// this once, share the resulting root across every test in this file.
fn shared_app_root() -> std::path::PathBuf {
    static ROOT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap().keep();
        Application::at_root(&dir, file_driver_config).unwrap();
        dir
    })
    .clone()
}

async fn shared_pool() -> sqlx::AnyPool {
    let dir = tempfile::tempdir().unwrap().keep();
    let database_url = format!("sqlite://{}/test.sqlite", dir.display());
    // The file driver never touches this pool at all - `with_sessions`'s
    // own signature still requires one regardless of which backend ends
    // up active, since which one that is isn't known until `session_layer`
    // itself resolves `Config::session_driver`.
    let _ = larust_orm::connect(&database_url).await;
    larust_orm::pool().unwrap().clone()
}

async fn greet(session: Session) -> String {
    session
        .insert("greeting", "hi")
        .await
        .expect("session insert must succeed");
    "greeted".to_string()
}

async fn recall(session: Session) -> String {
    session
        .get::<String>("greeting")
        .await
        .expect("session get must succeed")
        .unwrap_or_default()
}

async fn app() -> Router {
    let _ = shared_app_root();
    let pool = shared_pool().await;
    larust_http::Route::get("/greet", greet)
        .get("/recall", recall)
        .with_sessions(&pool, true)
        .await
        .unwrap()
        .into_axum_router()
}

#[tokio::test]
async fn session_driver_file_actually_uses_the_file_backend_not_the_database() {
    let root = shared_app_root();
    let router = app().await;

    let response = router
        .oneshot(Request::get("/greet").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .expect("session cookie should be set")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    // e.g. "larust_app_session=<the actual session id>"
    let session_id = cookie.split('=').nth(1).expect("cookie must have a value");

    // The real regression guard: a session file, named after this exact
    // id, must exist on disk at `storage/sessions/<id>` - proving
    // `resolve_backend` picked `FileSessionStore`, not `AnySessionStore`
    // (which would have written to a `sessions` SQL table in the throwaway
    // pool instead, leaving this directory untouched).
    let session_file = root.join("storage/sessions").join(session_id);
    assert!(
        session_file.exists(),
        "expected a session file at {}, but it doesn't exist - SESSION_DRIVER=file did not \
         actually route through FileSessionStore",
        session_file.display()
    );
    let contents = std::fs::read_to_string(&session_file).unwrap();
    assert!(
        contents.contains("greeting") && contents.contains("hi"),
        "session file content should contain the saved key/value, got: {contents}"
    );
}

#[tokio::test]
async fn session_driver_file_round_trips_data_across_two_separate_requests() {
    let router = app().await;

    let greet_response = router
        .clone()
        .oneshot(Request::get("/greet").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let cookie = greet_response
        .headers()
        .get(header::SET_COOKIE)
        .expect("session cookie should be set")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();

    let recall_response = router
        .oneshot(
            Request::get("/recall")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(recall_response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(recall_response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(String::from_utf8(body.to_vec()).unwrap(), "hi");
}

/// Proves the actual point of offering `"file"` at all over an in-memory
/// store (see `larust_http::session`'s own module doc comment): a session
/// saved by one `FileSessionStore` handle is readable by a genuinely
/// separate, freshly-constructed one pointed at the same directory - the
/// same shape a real process restart, or a zero-downtime handoff's old and
/// new generation briefly coexisting, actually has.
#[tokio::test]
async fn a_session_saved_by_one_store_handle_is_readable_by_a_fresh_one_over_the_same_directory() {
    use larust_http::session::FileSessionStore;
    use tower_sessions::session::Record;
    use tower_sessions::SessionStore;

    let root = shared_app_root();
    let dir = root.join("storage/sessions");
    tokio::fs::create_dir_all(&dir).await.unwrap();

    let first_handle = FileSessionStore::new(dir.clone());
    let mut record = Record {
        id: Default::default(),
        data: Default::default(),
        expiry_date: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
    };
    record
        .data
        .insert("survives".to_string(), serde_json::json!("a restart"));
    first_handle.create(&mut record).await.unwrap();

    // A brand-new handle, sharing nothing with `first_handle` but the
    // directory path - the same relationship two separate OS processes
    // (an old generation and its zero-downtime replacement) would have.
    let second_handle = FileSessionStore::new(dir);
    let loaded = second_handle
        .load(&record.id)
        .await
        .unwrap()
        .expect("a fresh store handle must still see the other handle's saved session");
    assert_eq!(loaded.data["survives"], serde_json::json!("a restart"));
}
