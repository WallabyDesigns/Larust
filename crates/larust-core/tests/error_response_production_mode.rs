//! Regression guard: with `APP_DEBUG` never set and no `ErrorPages` ever
//! registered (see `error_response_custom_pages.rs` for that case), 404/500
//! responses must render Larust's own built-in default page - and, above
//! all, never leak internal detail. A separate process (this file is its
//! own integration test binary) from `error_response_debug_mode.rs`,
//! deliberately: the debug flag is a process-wide `OnceLock` that can only
//! be set once, so "debug on" and "debug off" behavior can't share a
//! process.

use axum::http::StatusCode;
use axum::response::IntoResponse;
use larust_core::{default_internal_html, default_not_found_html, AppError};

async fn body_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn internal_error_renders_the_default_branded_page_not_the_real_detail() {
    let error = AppError::Internal(Box::new(std::io::Error::other("sensitive db detail")));
    let response = error.into_response();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = body_text(response).await;
    assert_eq!(body, default_internal_html());
    assert!(!body.contains("sensitive db detail"));
}

#[tokio::test]
async fn config_error_renders_the_default_branded_page_not_the_real_detail() {
    let error = AppError::Config(Box::new(std::io::Error::other("secret path detail")));
    let response = error.into_response();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = body_text(response).await;
    assert_eq!(body, default_internal_html());
    assert!(!body.contains("secret path detail"));
}

#[tokio::test]
async fn not_found_renders_the_default_branded_page() {
    let response = AppError::NotFound.into_response();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_text(response).await, default_not_found_html());
}

#[tokio::test]
async fn http_variant_is_unaffected_by_debug_mode_either_way() {
    let response = AppError::Http {
        status: StatusCode::IM_A_TEAPOT,
        message: "short and steamy".to_string(),
    }
    .into_response();

    assert_eq!(response.status(), StatusCode::IM_A_TEAPOT);
    assert_eq!(body_text(response).await, "short and steamy");
}
