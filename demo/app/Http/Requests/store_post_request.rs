use larust_support::FormRequest;

#[derive(FormRequest)]
pub struct StorePostRequest {
    #[validate(required, length(max = 255))]
    pub title: String,
    /// Comma-separated tag names; not `required` since a post without
    /// tags is fine - an absent/empty field just becomes `""`.
    #[validate(length(max = 255))]
    pub tags: String,
    /// Trix's HTML output. `max = 50000` is generous for HTML-with-markup
    /// overhead (well beyond a genuinely long post in words) - just a sane
    /// upper bound against abuse. Sanitized (not just validated) before
    /// it's ever persisted - see `larust_support::html::sanitize_rich_text`,
    /// called from the controller after `validated()`.
    #[validate(required, length(max = 50000))]
    pub content: String,
}
