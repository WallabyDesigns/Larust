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
    /// response (e.g. an email body) - deliberately bypasses
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

/// Checked once per process (not once per request) - `xr dev` sets
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
  // A named event, sent only for a static-asset-only change (see
  // `larust_core::dev_reload`) - no rebuild happened, no page reload
  // needed, just swap each stylesheet's own URL so the browser re-fetches
  // it instead of serving its cached copy.
  es.addEventListener('reload-assets', function () {
    document.querySelectorAll('link[rel="stylesheet"]').forEach(function (link) {
      var url = new URL(link.href);
      url.searchParams.set('_r', Date.now());
      link.href = url.href;
    });
  });
})();
</script>"#;

/// Injects the live-reload client just before `</body>` - falling back to
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
///
/// Every dynamic value in every rendered template passes through this -
/// a genuinely hot path, so it's worth not paying for what a naive
/// per-`char` loop actually costs: a `chars()` iterator decodes each
/// Unicode scalar and calls `String::push` (its own UTF-8 re-encode) even
/// for the overwhelming majority of input that's plain, nothing-to-escape
/// text. Scanning `s`'s raw bytes instead and bulk-`push_str`-ing each
/// unescaped run in one shot (rather than one `push` per character) means
/// an all-plain-text value - the common case - costs one `memcpy`-shaped
/// copy of the whole string, not N individual char pushes.
///
/// Byte-indexing here (not `char_indices()`) is safe *only* because every
/// character this function ever escapes (`&`, `<`, `>`, `"`, `'`) is
/// single-byte ASCII, and UTF-8 guarantees no byte belonging to a
/// multi-byte sequence can ever equal a single-byte ASCII code point -
/// so every index this loop slices `s` at is already guaranteed to sit on
/// a real `char` boundary, never mid-sequence. This is not true in
/// general for arbitrary byte values; if this function ever needs to
/// escape a non-ASCII character, this approach would need revisiting.
pub fn escape(s: &str) -> String {
    escape_ascii_bytes(s, |b| match b {
        b'&' => Some("&amp;"),
        b'<' => Some("&lt;"),
        b'>' => Some("&gt;"),
        b'"' => Some("&quot;"),
        b'\'' => Some("&#x27;"),
        _ => None,
    })
}

/// The byte-scan-and-bulk-copy loop `escape`/`hex_escape_for_html` both
/// use - `replacement` maps a byte needing escaping to its output text,
/// or `None` to leave it alone; see `escape`'s own doc comment for why
/// scanning bytes (not `char`s) is safe here specifically.
fn escape_ascii_bytes(s: &str, replacement: impl Fn(u8) -> Option<&'static str>) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut run_start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if let Some(escaped) = replacement(b) {
            out.push_str(&s[run_start..i]);
            out.push_str(escaped);
            run_start = i + 1;
        }
    }
    out.push_str(&s[run_start..]);
    out
}

/// `@js($expr)`'s JS-safe (not HTML-escaped) serialization - drops a
/// `Serialize` value into inline JavaScript as a `JSON.parse('...')` call,
/// mirroring Laravel's `Illuminate\Support\Js::from()` two-layer mechanism
/// faithfully:
///
/// 1. JSON-encode the value, then hex-escape `<`, `>`, `&`, `'` in the
///    result (`JSON_HEX_TAG`/`JSON_HEX_AMP`/`JSON_HEX_APOS` equivalents) -
///    this is what makes it safe to embed inside an HTML attribute or a
///    `<script>` block without e.g. a `"</script>"` string value breaking
///    out of context.
/// 2. Wrap that hex-escaped string as `JSON.parse('...')`, JSON-re-encoding
///    it (which escapes backslashes/newlines/quotes - including the `\u...`
///    sequences step 1 just introduced, and the delimiting `'` itself) and
///    stripping the outer `"..."` quotes that encoding pass adds. What's
///    left is exactly the text that belongs between the single quotes of
///    `JSON.parse('...')`, so the whole token can be spliced directly into
///    surrounding source with no caller-added quoting.
pub fn js<T: serde::Serialize>(value: &T) -> Result<String, serde_json::Error> {
    let json = serde_json::to_string(value)?;
    let hex_escaped = hex_escape_for_html(&json);
    let literal = serde_json::to_string(&hex_escaped)?;
    let inner = &literal[1..literal.len() - 1];
    Ok(format!("JSON.parse('{inner}')"))
}

/// `JSON_HEX_TAG | JSON_HEX_AMP | JSON_HEX_APOS` equivalent - deliberately
/// narrower than [`escape`] (no `"` - that's handled by `js`'s own second
/// JSON-encoding pass, not here). Same byte-scan approach as `escape`, for
/// the same reason (see its doc comment) - `<`, `>`, `&`, `'` are all
/// single-byte ASCII, so byte-index slicing is safe here too.
fn hex_escape_for_html(s: &str) -> String {
    escape_ascii_bytes(s, |b| match b {
        b'<' => Some("\\u003C"),
        b'>' => Some("\\u003E"),
        b'&' => Some("\\u0026"),
        b'\'' => Some("\\u0027"),
        _ => None,
    })
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
        // The script comes before </body>, not after it - the rest of the
        // document (the closing tags) must still follow the script.
        let script_pos = out.find("<script>").unwrap();
        let body_close_pos = out.find("</body>").unwrap();
        assert!(script_pos < body_close_pos);
        assert!(out.ends_with("</html>"));
    }

    #[test]
    fn dev_reload_script_listens_for_a_named_reload_assets_event() {
        let html = "<html><body></body></html>".to_string();
        let out = inject_dev_reload_script(html);
        assert!(out.contains("addEventListener('reload-assets'"));
        assert!(out.contains(r#"link[rel="stylesheet"]"#));
    }

    #[test]
    fn dev_reload_script_is_appended_when_there_is_no_body_tag() {
        let html = "<p>just a fragment</p>".to_string();
        let out = inject_dev_reload_script(html);
        assert!(out.starts_with("<p>just a fragment</p>"));
        assert!(out.ends_with("</script>"));
    }

    /// Reverses `js`'s two encoding passes so a test can assert on the
    /// *value* a browser would actually see, not the escaped token text.
    fn decode_js_token(token: &str) -> serde_json::Value {
        let inner = token
            .strip_prefix("JSON.parse('")
            .and_then(|s| s.strip_suffix("')"))
            .expect("token must be JSON.parse('...')");
        // `inner` is valid JSON-string-literal content (produced by `js`'s
        // own second `serde_json::to_string` pass) - wrapping it back in
        // `"..."` lets `serde_json` reverse that escaping for us instead of
        // hand-rolling a JS-string unescaper.
        let hex_escaped: String =
            serde_json::from_str(&format!("\"{inner}\"")).expect("valid JSON string literal");
        let unescaped = hex_escaped
            .replace("\\u003C", "<")
            .replace("\\u003E", ">")
            .replace("\\u0026", "&")
            .replace("\\u0027", "'");
        serde_json::from_str(&unescaped).expect("valid JSON")
    }

    #[test]
    fn js_wraps_output_as_a_json_parse_call() {
        let token = js(&42).unwrap();
        assert!(token.starts_with("JSON.parse('"));
        assert!(token.ends_with("')"));
    }

    #[test]
    fn js_round_trips_a_plain_value() {
        let token = js(&serde_json::json!({"id": 7, "name": "Alice"})).unwrap();
        assert_eq!(
            decode_js_token(&token),
            serde_json::json!({"id": 7, "name": "Alice"})
        );
    }

    #[test]
    fn js_hex_escapes_angle_brackets_ampersand_and_apostrophe() {
        let value = "</script>&'<b>'";
        let token = js(&value).unwrap();
        // The raw characters must never appear literally in the token -
        // that's the whole point of hex-escaping before either JSON pass.
        assert!(!token.contains('<'));
        assert!(!token.contains('>'));
        assert!(!token.contains('&'));
        // `'` is allowed to appear only as the two literal quotes framing
        // `JSON.parse(' ... ')` - never inside the payload, or it would
        // terminate the JS string literal early.
        assert_eq!(token.matches('\'').count(), 2);
        assert_eq!(decode_js_token(&token), serde_json::json!(value));
    }

    #[test]
    fn js_output_never_contains_a_literal_close_script_tag() {
        let token = js(&"</script><script>alert(1)</script>").unwrap();
        assert!(!token.contains("</script>"));
        assert_eq!(
            decode_js_token(&token),
            serde_json::json!("</script><script>alert(1)</script>")
        );
    }
}
