use axum::http::{HeaderMap, StatusCode};

/// A `TestClient` response with the body already buffered into an owned
/// `String` — every existing hand-rolled test in this codebase repeats
/// the same `axum::body::to_bytes(...).await.unwrap().to_vec()` +
/// `String::from_utf8(...).unwrap()` dance at nearly every assertion
/// site; this does it once, at construction, instead.
pub struct TestResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl TestResponse {
    pub(crate) fn new(status: StatusCode, headers: HeaderMap, body: String) -> Self {
        Self {
            status,
            headers,
            body,
        }
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }

    pub fn body(&self) -> &str {
        &self.body
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name)?.to_str().ok()
    }

    /// Scrapes the hidden `_csrf_token` field's value out of the response
    /// body (reuses [`larust_http::csrf::FIELD_NAME`] rather than a
    /// hardcoded string), for a test that fetched a form page and now
    /// needs the token to submit it via [`crate::TestClient::post_form`] —
    /// the exact manual step every CSRF-protected test in this codebase
    /// performed by hand before this crate existed.
    pub fn csrf_token(&self) -> Option<String> {
        extract_hidden_field_value(&self.body, larust_http::csrf::FIELD_NAME)
    }

    /// Scrapes the CSRF token out of the shared layout's
    /// `<meta name="csrf-token" content="...">` tag instead of a hidden
    /// form field — for a page with no `@csrf`-rendered `<form>` at all
    /// (e.g. one whose only interaction is a JS-driven `fetch()`, like a
    /// `@wire(...)`-mounted component's `wire:model`/`wire:submit` sync),
    /// the same way the real client runtime
    /// (`crates/larust-live/assets/wire-runtime.js`) itself reads it.
    pub fn meta_csrf_token(&self) -> Option<String> {
        extract_attr_value(&self.body, "name=\"csrf-token\" content=\"")
    }

    /// Panics (with the actual vs. expected status in the message) unless
    /// the response has `expected`'s status — matching this codebase's
    /// plain `assert!`/`assert_eq!` style, just packaged for chaining.
    pub fn assert_status(&self, expected: StatusCode) -> &Self {
        assert_eq!(
            self.status, expected,
            "expected status {expected}, got {} — body: {}",
            self.status, self.body
        );
        self
    }

    /// Asserts a 3xx status whose `Location` header equals `path` exactly
    /// (Laravel's `assertRedirect($path)`).
    pub fn assert_redirect_to(&self, path: &str) -> &Self {
        assert!(
            self.status.is_redirection(),
            "expected a redirect, got status {} — body: {}",
            self.status,
            self.body
        );
        assert_eq!(
            self.header("location"),
            Some(path),
            "expected a redirect to `{path}`"
        );
        self
    }

    /// Asserts the response body contains `needle` (Laravel's
    /// `assertSee($text)`).
    pub fn assert_body_contains(&self, needle: &str) -> &Self {
        assert!(
            self.body.contains(needle),
            "expected body to contain `{needle}` — body: {}",
            self.body
        );
        self
    }
}

/// The actual scraping logic, factored out from [`TestResponse::csrf_token`]
/// so it's testable against a plain string without a real response.
/// Looks for `name="{field}" value="..."` and returns the `value`.
fn extract_hidden_field_value(html: &str, field: &str) -> Option<String> {
    extract_attr_value(html, &format!("name=\"{field}\" value=\""))
}

/// Finds `needle` (an attribute-name-and-opening-quote prefix, e.g.
/// `name="csrf-token" content="`) and returns the quoted value that
/// follows it, up to the next `"`. Shared by [`extract_hidden_field_value`]
/// and [`TestResponse::meta_csrf_token`] — the only difference between
/// scraping a hidden input's `value=` and a meta tag's `content=` is which
/// attribute name comes right before the value.
fn extract_attr_value(html: &str, needle: &str) -> Option<String> {
    let start = html.find(needle)? + needle.len();
    let end = html[start..].find('"')? + start;
    Some(html[start..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_hidden_field_value_finds_the_value() {
        let html = r#"<input type="hidden" name="_csrf_token" value="abc123">"#;
        assert_eq!(
            extract_hidden_field_value(html, "_csrf_token"),
            Some("abc123".to_string())
        );
    }

    #[test]
    fn extract_hidden_field_value_returns_none_when_absent() {
        let html = "<p>no token here</p>";
        assert_eq!(extract_hidden_field_value(html, "_csrf_token"), None);
    }

    #[test]
    fn extract_hidden_field_value_only_matches_the_named_field() {
        let html = r#"<input name="other_field" value="not-this-one">"#;
        assert_eq!(extract_hidden_field_value(html, "_csrf_token"), None);
    }

    #[test]
    fn extract_attr_value_finds_a_meta_tags_content() {
        let html = r#"<meta name="csrf-token" content="xyz789">"#;
        assert_eq!(
            extract_attr_value(html, "name=\"csrf-token\" content=\""),
            Some("xyz789".to_string())
        );
    }
}
