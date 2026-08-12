use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use std::collections::BTreeMap;

/// Laravel-shaped validation error bag: field name -> messages.
///
/// `IntoResponse` renders it as a 422 with the same JSON shape Laravel's
/// default validation exception produces:
/// `{"message": "...", "errors": {"field": ["msg", ...]}}`.
#[derive(Debug, Default)]
pub struct ValidationErrors {
    errors: BTreeMap<String, Vec<String>>,
}

impl ValidationErrors {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, field: &str, message: impl Into<String>) {
        self.errors
            .entry(field.to_string())
            .or_default()
            .push(message.into());
    }

    pub fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }
}

#[derive(serde::Serialize)]
struct ErrorBody<'a> {
    message: &'static str,
    errors: &'a BTreeMap<String, Vec<String>>,
}

impl IntoResponse for ValidationErrors {
    fn into_response(self) -> Response {
        let body = ErrorBody {
            message: "The given data was invalid.",
            errors: &self.errors,
        };
        (StatusCode::UNPROCESSABLE_ENTITY, Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_empty() {
        assert!(ValidationErrors::new().is_empty());
    }

    #[test]
    fn add_accumulates_multiple_messages_per_field() {
        let mut errors = ValidationErrors::new();
        errors.add("email", "is required");
        errors.add("email", "must be a valid email address");

        assert!(!errors.is_empty());
        assert_eq!(errors.errors.get("email").unwrap().len(), 2);
    }
}
