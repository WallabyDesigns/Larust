use axum::http::StatusCode;
use larust_core::AppError;

/// Laravel's `abort(status)`: builds an [`AppError`] for a specific HTTP
/// status, to be returned (typically via `?`) from a controller.
///
/// # Panics
///
/// Panics if `status` is not a valid HTTP status code (100-599). This is a
/// caller contract violation (a hardcoded typo like `abort(9999)`), not a
/// condition that can arise from user input.
pub fn abort(status: u16) -> AppError {
    let status = StatusCode::from_u16(status)
        .unwrap_or_else(|_| panic!("abort() called with invalid HTTP status code: {status}"));
    let message = status.canonical_reason().unwrap_or("error").to_string();

    AppError::Http { status, message }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_error_for_known_status() {
        let err = abort(404);
        match err {
            AppError::Http { status, message } => {
                assert_eq!(status, StatusCode::NOT_FOUND);
                assert_eq!(message, "Not Found");
            }
            _ => panic!("expected AppError::Http"),
        }
    }

    #[test]
    #[should_panic(expected = "invalid HTTP status code")]
    fn panics_on_invalid_status() {
        abort(9999);
    }
}
