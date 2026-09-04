/// Builds an absolute URL from `path`, using the app's configured
/// `APP_URL` (Laravel's `url($path)`).
pub fn url(path: &str) -> String {
    join_url(&larust_core::config().app_url, path)
}

/// Laravel's `asset($path)` only differs from `url($path)` once a separate
/// CDN/`ASSET_URL` is configured - no such concept exists in this
/// framework yet, so this is a direct delegation, matching Laravel's own
/// default behavior when no separate asset URL is set. A real, distinct
/// implementation (and its own config field) is a natural addition if a
/// CDN-hosted-assets need ever shows up.
pub fn asset(path: &str) -> String {
    url(path)
}

/// The actual joining logic, factored out from `url()` so it's testable
/// without touching `larust_core::config()`'s process-wide `OnceLock`
/// (which only `Application::new()` can populate, and only once per
/// process - not practical to exercise per-test-case here). Normalizes
/// exactly one `/` between `base` and `path` regardless of whether either
/// side already has one, so `url("/posts")` and `url("posts")` produce the
/// same result - callers don't need to remember which style this
/// particular helper expects.
fn join_url(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_a_leading_slash_path_with_exactly_one_slash() {
        assert_eq!(
            join_url("http://example.test", "/posts"),
            "http://example.test/posts"
        );
    }

    #[test]
    fn joins_a_path_with_no_leading_slash_the_same_way() {
        assert_eq!(
            join_url("http://example.test", "posts"),
            "http://example.test/posts"
        );
    }

    #[test]
    fn joins_a_base_with_a_trailing_slash_without_doubling_it() {
        assert_eq!(
            join_url("http://example.test/", "/posts"),
            "http://example.test/posts"
        );
    }

    #[test]
    fn empty_path_still_produces_a_trailing_slash_on_the_base() {
        assert_eq!(join_url("http://example.test", ""), "http://example.test/");
    }
}
