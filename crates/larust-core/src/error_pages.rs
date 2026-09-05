//! Process-wide error-page registry. Same `OnceLock`-backed process-wide-
//! state idiom `crate::debug` (and `crates/larust-orm/src/pool.rs`) uses -
//! needed for the same reason `debug::is_enabled()` is: `AppError::
//! into_response()` is a stateless `IntoResponse` impl with no access to
//! `AppState`, so this is the only way for it to reach app-registered
//! content.
//!
//! `default_not_found_html`/`default_internal_html` are `pub`, not
//! `pub(crate)`, because `larust-macros`' `error_view!` macro (expanded
//! inside an *app's* crate, not this one) calls them directly as the
//! "no `resources/views/errors/{code}.blade.xr` file exists" fallback for
//! 404/500 - one canonical implementation of what the default page looks
//! like, shared by both the always-on registry fallback below and the
//! opt-in override macro, rather than two copies that could drift apart.

use std::sync::OnceLock;

/// Rendered once (at `Application::with_error_pages` call time, not per
/// request) and cached as plain `String`s rather than a render closure or
/// a `larust_view::View` - these pages have no per-request dynamic content
/// (no session, no user), and this crate has no dependency on the view
/// engine at all (see `Cargo.toml`), so there's nothing a closure would buy
/// over pre-rendering once.
pub struct ErrorPages {
    pub not_found: String,
    pub internal: String,
}

static PAGES: OnceLock<ErrorPages> = OnceLock::new();

/// Idempotent - a second call is a silent no-op, same reasoning as
/// `debug::set`: there's no meaningful conflict to report for state this
/// coarse.
pub(crate) fn set(pages: ErrorPages) {
    let _ = PAGES.set(pages);
}

pub(crate) fn not_found_html() -> String {
    PAGES
        .get()
        .map(|p| p.not_found.clone())
        .unwrap_or_else(default_not_found_html)
}

pub(crate) fn internal_html() -> String {
    PAGES
        .get()
        .map(|p| p.internal.clone())
        .unwrap_or_else(default_internal_html)
}

/// The framework's own built-in 404 page - styled to match the demo app's
/// branding (see `page_shell`'s own doc comment for the exact source of
/// the color tokens/copy this mirrors).
pub fn default_not_found_html() -> String {
    page_shell(
        "404",
        "Page not found",
        "The page you're looking for doesn't exist, or it's moved somewhere else.",
    )
}

/// The framework's own built-in 500 page. Production-mode only - debug
/// mode (`APP_DEBUG=true`) never reaches this; see `error::debug_page`.
pub fn default_internal_html() -> String {
    page_shell(
        "500",
        "Something went wrong",
        "An unexpected error occurred on our end. We're looking into it.",
    )
}

/// Self-contained (no external CSS/JS, no build step) - same reasoning as
/// `error::debug_page`: this has to render standalone even as the very
/// first response a broken app ever produces. Colors, font stack, the
/// ">_" brand mark, and the footer credit line are copied from `demo/
/// public/styles/style.css`'s `:root`/`[data-theme="dark"]` tokens and
/// `demo/resources/views/layouts/app.blade.xr`'s header/footer markup -
/// the actual reference the "look like the demo app" ask points at. Unlike
/// the demo layout's own cookie-persisted `data-theme` (which needs a
/// `CookieJar` this call site doesn't have - see the module doc comment),
/// light/dark here is `prefers-color-scheme`, decided by the browser with
/// no server-side state at all.
fn page_shell(status: &str, title: &str, message: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{status} {title}</title>
<style>
  :root {{
    --ink: #202124; --muted: #6b6d73; --paper: #fffdf9; --canvas: #f4f0e8;
    --line: #e4ddd2; --brand: #f4513d; --brand-dark: #cf3628;
  }}
  @media (prefers-color-scheme: dark) {{
    :root {{
      --ink: #f6f1e8; --muted: #b8b1a8; --paper: #272522; --canvas: #181716;
      --line: #45413b; --brand: #ff735f; --brand-dark: #ff8a79;
    }}
  }}
  * {{ box-sizing: border-box; }}
  body {{
    margin: 0; min-height: 100vh; display: flex; flex-direction: column;
    color: var(--ink); background: var(--canvas);
    font-family: Inter, ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif;
    line-height: 1.5;
  }}
  a {{ color: inherit; }}
  main {{
    flex: 1; display: flex; flex-direction: column; align-items: center;
    justify-content: center; text-align: center; padding: 40px 20px;
  }}
  .brand {{
    display: inline-flex; gap: 10px; align-items: center; margin-bottom: 32px;
    font-size: 1.15rem; font-weight: 800; letter-spacing: -.04em; text-decoration: none;
  }}
  .brand-mark {{
    display: grid; place-items: center; width: 30px; height: 30px; color: #fff;
    background: var(--brand); border-radius: 9px 9px 9px 2px;
    font-family: ui-monospace, monospace; font-size: .9rem;
  }}
  .status {{
    font-size: clamp(3rem, 10vw, 5rem); font-weight: 800; letter-spacing: -.03em;
    color: var(--brand); margin: 0;
  }}
  h1 {{ font-size: 1.5rem; margin: 8px 0 12px; }}
  p.message {{ color: var(--muted); max-width: 40ch; margin: 0 0 28px; }}
  .home-link {{
    display: inline-block; padding: 12px 22px; border-radius: 10px; font-weight: 700;
    color: #fff; background: var(--brand); text-decoration: none;
  }}
  .home-link:hover {{ background: var(--brand-dark); }}
  footer {{
    padding: 24px 0 34px; text-align: center; color: var(--muted);
    font-size: .82rem; border-top: 1px solid var(--line);
  }}
  .wallaby {{ color: var(--brand); font-weight: 700; }}
</style>
</head>
<body>
<main>
  <a class="brand" href="/"><span class="brand-mark">&gt;_</span><span>larust</span></a>
  <p class="status">{status}</p>
  <h1>{title}</h1>
  <p class="message">{message}</p>
  <a class="home-link" href="/">Go back home</a>
</main>
<footer>Larust by <span class="wallaby">Wallaby Designs</span></footer>
</body>
</html>"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_not_found_html_carries_the_404_status_and_brand_mark() {
        let html = default_not_found_html();
        assert!(html.contains("404"));
        assert!(html.contains("Page not found"));
        assert!(html.contains("larust"));
    }

    #[test]
    fn default_internal_html_carries_the_500_status_and_no_leaked_detail() {
        let html = default_internal_html();
        assert!(html.contains("500"));
        assert!(html.contains("Something went wrong"));
        // The whole point of this page: it's a static, canned message -
        // never anything sourced from a real error's own detail.
        assert!(!html.contains("Caused by"));
    }
}
