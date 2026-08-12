use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use std::sync::OnceLock;

/// A rendered template. What `view!(...)` (in `larust-macros`) produces.
#[must_use]
pub struct View(String);

impl View {
    pub fn new(html: String) -> Self {
        Self(html)
    }

    /// The raw rendered HTML, for a caller that isn't building an HTTP
    /// response (e.g. an email body) — deliberately bypasses
    /// `into_response()`'s dev-reload script injection, which only makes
    /// sense for a page a browser tab is polling for a live-reload signal.
    pub fn into_html(self) -> String {
        self.0
    }
}

impl IntoResponse for View {
    fn into_response(self) -> Response {
        let html = if dev_reload_enabled() {
            inject_dev_reload_script(self.0)
        } else {
            self.0
        };
        ([(CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
    }
}

/// Checked once per process (not once per request) — `xr dev` sets
/// `LARUST_DEV_RELOAD` only on the child process it spawns itself, so this
/// is `false` for the lifetime of any normal `cargo run`.
fn dev_reload_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("LARUST_DEV_RELOAD").is_some())
}

const DEV_RELOAD_SCRIPT: &str = r#"<script>
(function () {
  var opened = false;
  var es = new EventSource('/__larust_dev');
  es.onopen = function () {
    if (opened) location.reload();
    opened = true;
  };
})();
</script>"#;

/// Injects the live-reload client just before `</body>` — falling back to
/// appending it if a page doesn't have one (a fragment response, say).
fn inject_dev_reload_script(html: String) -> String {
    match html.find("</body>") {
        Some(index) => {
            let mut out = String::with_capacity(html.len() + DEV_RELOAD_SCRIPT.len());
            out.push_str(&html[..index]);
            out.push_str(DEV_RELOAD_SCRIPT);
            out.push_str(&html[index..]);
            out
        }
        None => html + DEV_RELOAD_SCRIPT,
    }
}

/// HTML-escapes a string for safe interpolation (`{{ }}`, not `{!! !!}`).
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn into_html_returns_the_raw_rendered_string() {
        let view = View::new("<p>hello</p>".to_string());
        assert_eq!(view.into_html(), "<p>hello</p>");
    }

    #[test]
    fn escapes_all_special_characters() {
        assert_eq!(
            escape(r#"<script>alert('&"x"')</script>"#),
            "&lt;script&gt;alert(&#x27;&amp;&quot;x&quot;&#x27;)&lt;/script&gt;"
        );
    }

    #[test]
    fn leaves_plain_text_unchanged() {
        assert_eq!(escape("hello world"), "hello world");
    }

    #[test]
    fn dev_reload_script_is_injected_before_closing_body_tag() {
        let html = "<html><body><h1>hi</h1></body></html>".to_string();
        let out = inject_dev_reload_script(html);
        assert!(out.contains("EventSource('/__larust_dev')"));
        // The script comes before </body>, not after it — the rest of the
        // document (the closing tags) must still follow the script.
        let script_pos = out.find("<script>").unwrap();
        let body_close_pos = out.find("</body>").unwrap();
        assert!(script_pos < body_close_pos);
        assert!(out.ends_with("</html>"));
    }

    #[test]
    fn dev_reload_script_is_appended_when_there_is_no_body_tag() {
        let html = "<p>just a fragment</p>".to_string();
        let out = inject_dev_reload_script(html);
        assert!(out.starts_with("<p>just a fragment</p>"));
        assert!(out.ends_with("</script>"));
    }
}
