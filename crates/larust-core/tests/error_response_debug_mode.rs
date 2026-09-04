//! With `APP_DEBUG=true`, `AppError` should render the real detail instead
//! of a generic message. Own process from `error_response_production_mode.rs`
//! (see that file's header comment) - the debug flag is a process-wide
//! `OnceLock`, set for real here via `Application::new()` reading
//! `APP_DEBUG` from the environment, the same path a real app takes.

use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use larust_core::{AppError, Application};

/// `std::env::set_var` is only sound to call when no other thread might
/// read or write the environment concurrently - `cargo test` runs this
/// file's three `#[tokio::test]`s on separate threads by default, so
/// calling it once per test (even setting the same value each time) is a
/// real data race, not just a style nit. `Once` guarantees the mutation
/// (and the `Application::new()` call that reads it back) happens exactly
/// once, with every other caller blocking until it's done rather than
/// racing it.
fn enable_debug_mode() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        std::env::set_var("APP_DEBUG", "true");
        // `Application::new()` is what actually reads `APP_DEBUG` and flips
        // the process-wide flag `AppError::into_response` checks - going
        // through the real startup path rather than reaching into a
        // private setter. No app-specific `config/app.rs` exists for this
        // test (it lives inside `larust-core` itself, which can't depend
        // on `larust-support`'s `config_env` helpers without a circular
        // dependency), so this reads `APP_DEBUG` directly, the one field
        // this test actually needs read from the environment.
        fn config() -> serde_json::Value {
            let mut value = serde_json::json!({});
            if let Ok(debug) = std::env::var("APP_DEBUG")
                .unwrap_or_default()
                .parse::<bool>()
            {
                value["app_debug"] = serde_json::json!(debug);
            }
            value
        }
        Application::new(config).unwrap();
    });
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[derive(Debug)]
struct Root;
impl std::fmt::Display for Root {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "root cause: connection refused")
    }
}
impl std::error::Error for Root {}

#[derive(Debug)]
struct Wrapper(Root);
impl std::fmt::Display for Wrapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "query failed")
    }
}
impl std::error::Error for Wrapper {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

#[tokio::test]
async fn internal_error_renders_the_full_source_chain_as_html() {
    enable_debug_mode();

    let error = AppError::Internal(Box::new(Wrapper(Root)));
    let response = error.into_response();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/html; charset=utf-8")
    );

    let body = body_text(response).await;
    assert!(body.contains("internal server error: query failed"));
    assert!(body.contains("root cause: connection refused"));
    assert!(body.contains("Caused by"));
}

#[tokio::test]
async fn not_found_renders_a_branded_html_page_in_debug_mode() {
    enable_debug_mode();

    let response = AppError::NotFound.into_response();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = body_text(response).await;
    assert!(body.contains("Not Found"));
    assert!(body.contains("<html"));
}

#[tokio::test]
async fn http_variant_still_returns_the_caller_supplied_message_verbatim() {
    enable_debug_mode();

    let response = AppError::Http {
        status: StatusCode::IM_A_TEAPOT,
        message: "short and steamy".to_string(),
    }
    .into_response();

    assert_eq!(response.status(), StatusCode::IM_A_TEAPOT);
    assert_eq!(body_text(response).await, "short and steamy");
}
