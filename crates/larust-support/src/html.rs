/// Sanitizes user-authored HTML (e.g. a rich-text editor's output) down to
/// a safe subset before it's persisted or rendered — strips
/// `<script>`/event-handler attributes/`javascript:` URLs, keeps ordinary
/// formatting markup (paragraphs, headings, lists, links, images, etc.).
/// Ammonia's default tag/attribute allowlist matches typical rich-text
/// editor output (Trix, and similar `contenteditable`-based editors)
/// closely enough to need no custom configuration for this.
///
/// This is not the same thing as `{{ }}`'s HTML-escaping: escaping turns
/// markup into inert, visible text (`<p>` becomes `&lt;p&gt;`); sanitizing
/// keeps real, renderable markup, just with anything dangerous removed.
/// Use this before storing (or rendering via `{!! !!}`) any HTML that
/// ultimately originated from a client request — nothing prevents a
/// request from POSTing arbitrary HTML directly to a field a browser-side
/// editor widget happens to populate, so the server-side sanitization step
/// is the actual security boundary, not the editor.
pub fn sanitize_rich_text(input: &str) -> String {
    ammonia::clean(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_script_tags() {
        let out = sanitize_rich_text("<p>hi</p><script>alert(1)</script>");
        assert!(!out.contains("<script"));
        assert!(out.contains("hi"));
    }

    #[test]
    fn strips_event_handler_attributes() {
        let out = sanitize_rich_text(r#"<img src="x.png" onerror="alert(1)">"#);
        assert!(!out.contains("onerror"));
    }

    #[test]
    fn strips_javascript_urls() {
        let out = sanitize_rich_text(r#"<a href="javascript:alert(1)">click</a>"#);
        assert!(!out.contains("javascript:"));
    }

    #[test]
    fn keeps_ordinary_formatting_markup() {
        let out = sanitize_rich_text("<p>Hello <strong>world</strong></p><ul><li>one</li></ul>");
        assert!(out.contains("<strong>world</strong>"));
        assert!(out.contains("<li>one</li>"));
    }

    #[test]
    fn keeps_safe_links() {
        let out = sanitize_rich_text(r#"<a href="https://example.com">link</a>"#);
        assert!(out.contains(r#"href="https://example.com""#));
    }
}
