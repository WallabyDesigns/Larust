//! Regression guard for the *other* half of `ErrorPages`: once an app has
//! called `Application::with_error_pages(...)`, `AppError::NotFound`/
//! `Internal`/`Config` render whatever was registered instead of Larust's
//! own built-in default (that default-only path is
//! `error_response_production_mode.rs`, a separate process deliberately -
//! see its own doc comment). `error_pages::set()`'s `OnceLock` can only be
//! set once per process, so this file is kept to exactly one test that
//! registers once and asserts everything it needs - a second test in this
//! same file racing to register different content would be order-dependent
//! under Rust's default parallel test threads.

use axum::http::StatusCode;
use axum::response::IntoResponse;
use larust_core::{AppError, Application, ErrorPages};

fn empty_config() -> serde_json::Value {
    serde_json::json!({})
}

#[tokio::test]
async fn a_registered_override_renders_instead_of_the_built_in_default() {
    let dir = tempfile::tempdir().unwrap();
    let _app = Application::at_root(dir.path(), empty_config)
        .unwrap()
        .with_error_pages(ErrorPages {
            not_found: "<h1>custom 404</h1>".to_string(),
            internal: "<h1>custom 500</h1>".to_string(),
        });

    let not_found_response = AppError::NotFound.into_response();
    assert_eq!(not_found_response.status(), StatusCode::NOT_FOUND);
    let not_found_bytes = axum::body::to_bytes(not_found_response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(not_found_bytes, "<h1>custom 404</h1>".as_bytes());

    let internal_response =
        AppError::Internal(Box::new(std::io::Error::other("db exploded"))).into_response();
    assert_eq!(
        internal_response.status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    let internal_bytes = axum::body::to_bytes(internal_response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(internal_bytes, "<h1>custom 500</h1>".as_bytes());
    assert!(!String::from_utf8(internal_bytes.to_vec())
        .unwrap()
        .contains("db exploded"));
}
