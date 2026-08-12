use larust_support::axum::body::Body;
use larust_support::axum::extract::{FromRequest, Request};
use larust_support::axum::http::{header, StatusCode};
use larust_support::axum::response::IntoResponse;
use larust_support::FormRequest;

#[derive(FormRequest, Debug)]
pub struct RegisterRequest {
    #[validate(required, email)]
    pub email: String,
    #[validate(required, length(min = 8), confirmed)]
    pub password: String,
}

fn form_request(body: &str) -> Request {
    Request::builder()
        .method("POST")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn confirmed_rule_accepts_a_matching_confirmation_field() {
    let request =
        form_request("email=a@b.com&password=longenough&password_confirmation=longenough");
    let result = RegisterRequest::from_request(request, &()).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn confirmed_rule_rejects_a_mismatched_confirmation_field() {
    let request = form_request("email=a@b.com&password=longenough&password_confirmation=different");
    let errors = RegisterRequest::from_request(request, &())
        .await
        .unwrap_err();

    let response = errors.into_response();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = larust_support::axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        body.contains("password"),
        "the password field should be flagged in the error body: {body}"
    );
}

#[tokio::test]
async fn confirmed_rule_rejects_a_missing_confirmation_field() {
    let request = form_request("email=a@b.com&password=longenough");
    let errors = RegisterRequest::from_request(request, &())
        .await
        .unwrap_err();

    assert_eq!(
        errors.into_response().status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
}
