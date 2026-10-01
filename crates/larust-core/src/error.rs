use crate::{debug, error_pages};
use axum::http::header::CONTENT_TYPE;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use thiserror::Error;

/// The framework's primary error type.
///
/// HTTP responses for `Config`/`Internal` only ever expose a generic
/// message - *unless* `APP_DEBUG=true` (see the `debug` module), in which
/// case the full message and source chain are rendered as an HTML page
/// instead. The wrapped source error (with full detail) is always logged
/// via `tracing` regardless of debug mode. Never enable `APP_DEBUG` outside
/// local development - see `docs/GOTCHAS.md`.
#[derive(Debug, Error)]
pub enum AppError {
    #[error("configuration error: {0}")]
    Config(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("not found")]
    NotFound,

    #[error("internal server error: {0}")]
    Internal(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// A specific HTTP status with a message safe to show clients (Laravel's
    /// `abort()`). Unlike `Config`/`Internal`, this message is sent as-is -
    /// callers are responsible for not putting sensitive detail in it.
    #[error("{message}")]
    Http { status: StatusCode, message: String },
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        // Computed once, up front, from thiserror's own derived `Display`
        // (`#[error("configuration error: {0}")]` / `#[error("internal
        // server error: {0}")]`) - the single source of truth for the
        // top-level message, used both for the log line below and (for
        // `Config`/`Internal`) as the debug-page's own first "cause" card,
        // so the two can never silently drift apart the way two separately
        // hand-written literals could.
        let message = self.to_string();
        if matches!(self, AppError::Config(_) | AppError::Internal(_)) {
            tracing::error!(error = %message, "unhandled application error");
        }

        match self {
            AppError::NotFound => {
                if debug::is_enabled() {
                    debug_page(
                        StatusCode::NOT_FOUND,
                        "Not Found",
                        &["No route matched this request.".to_string()],
                        None,
                    )
                } else {
                    html_response(StatusCode::NOT_FOUND, error_pages::not_found_html())
                }
            }
            AppError::Http { status, message } => (status, message).into_response(),
            AppError::Config(source) | AppError::Internal(source) => {
                internal_response(&message, source.as_ref())
            }
        }
    }
}

/// Shared by both `AppError` variants that carry a boxed source error -
/// walks the full `source()` chain (each wrapped error, one level at a
/// time) so a debug-mode page shows e.g. the actual SQL driver error, not
/// just "internal server error". Capped so a pathological (e.g. cyclic)
/// third-party `source()` implementation can't hang the request or grow
/// the page unbounded - every error source in this codebase today
/// terminates in a handful of levels, so the cap is generous, not tight.
const MAX_SOURCE_CHAIN_DEPTH: u8 = 20;

fn internal_response(top_message: &str, source: &(dyn std::error::Error + 'static)) -> Response {
    if debug::is_enabled() {
        // One message per card (see `debug_page`'s own doc comment for why
        // this changed from one big concatenated string) rather than
        // building a single blob - each `source()` level renders as its
        // own distinct, readable card instead of a wall of text with
        // hand-inserted "Caused by:" separators.
        let mut messages = vec![top_message.to_string()];
        let mut cause = source.source();
        let mut depth = 0;
        while let Some(err) = cause {
            depth += 1;
            if depth > MAX_SOURCE_CHAIN_DEPTH {
                messages.push("… source chain truncated …".to_string());
                break;
            }
            messages.push(err.to_string());
            cause = err.source();
        }
        debug_page(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error",
            &messages,
            None,
        )
    } else {
        html_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            error_pages::internal_html(),
        )
    }
}

/// What capturing a backtrace for a given panic turned up.
/// `Application::serve()` installs a custom panic hook (alongside its
/// `CatchPanicLayer`) that captures this at the exact moment a panic
/// happens, since capturing *after* `catch_unwind` returns would be
/// useless - the stack has already unwound back up to the catching layer
/// by then, so a fresh capture there would just show tower-http's own
/// frames, not the panic site's.
pub(crate) enum PanicBacktrace {
    /// `RUST_BACKTRACE`/`RUST_LIB_BACKTRACE` wasn't set at the moment this
    /// exact panic happened - `std::backtrace::Backtrace::capture()`'s own
    /// documented behavior, the same env var Rust's own default panic hook
    /// already reads. `xr dev --debug` sets `RUST_BACKTRACE=full`
    /// automatically for exactly this reason - see `larust_cli::dev::
    /// set_debug_env_vars`.
    Disabled,
    /// This platform/build doesn't support capturing one at all -
    /// `std::backtrace::BacktraceStatus` can report this; rare, but real.
    Unsupported,
    /// The real, captured backtrace - `Backtrace`'s own `Display` output,
    /// parsed into individual frames purely for rendering (grouping
    /// consecutive Rust-runtime-internal frames into a collapsible region
    /// instead of showing every single one at full prominence).
    Captured(String),
}

/// Same debug/production branching `AppError::Internal` uses, for a panic
/// caught by `Application::serve()`'s `CatchPanicLayer`. There's no
/// `AppError`/`std::error::Error` value for a panic - just its payload's
/// message, plus whatever `backtrace` the panic hook managed to capture -
/// so this is a distinct entry point rather than routed through
/// `internal_response`, with no synthetic `source()` chain to walk.
pub(crate) fn render_panic(message: &str, backtrace: PanicBacktrace) -> Response {
    tracing::error!(error = %message, "panic in request handler");
    if debug::is_enabled() {
        debug_page(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error (panic)",
            &[message.to_string()],
            Some(backtrace),
        )
    } else {
        html_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            error_pages::internal_html(),
        )
    }
}

/// Shared by every production-mode branch above - builds the same
/// `text/html; charset=utf-8` response shape `debug_page` uses, just for
/// an already-fully-rendered page instead of one this module formats
/// itself.
fn html_response(status: StatusCode, html: String) -> Response {
    (status, [(CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
}

/// One frame of a parsed backtrace - `text` is the frame's own full,
/// possibly-multi-line block (the `N: symbol` header line, plus any
/// `at FILE:LINE` location line(s) `Backtrace`'s own `Display` impl prints
/// under it), kept together as one unit so collapsing/rendering never
/// splits a frame's header from its own location.
struct Frame {
    text: String,
    is_runtime_noise: bool,
}

/// A line starting a new frame in `Backtrace`'s own `Display` output looks
/// like (leading whitespace varies, but always) `<digits>: <symbol>` -
/// e.g. `  12: core::ops::function::FnOnce::call_once`. Any other line
/// (typically an indented `at /path/to/file.rs:10:5` location line, or a
/// continuation for an inlined frame) belongs to whichever frame most
/// recently started.
fn is_frame_header_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    let digit_count = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
    digit_count > 0 && trimmed[digit_count..].starts_with(':')
}

/// Rust-standard-library/runtime frames that appear, unchanged, in the
/// backtrace of *every* panic, regardless of what the application actually
/// did - panic machinery unwinding itself, allocator internals, the
/// backtrace-capturing code's own frames, and (since every Larust app runs
/// on it) tokio's own task-polling machinery. Collapsing these by default
/// is the same spirit as Laravel/Ignition's own "N vendor frames
/// collapsed", applied to what a Rust backtrace's own equivalent of
/// "vendor" actually is - there's no portable way to also detect "this
/// frame belongs to some *other* third-party crate the app depends on" at
/// this generic a layer, so this intentionally stays conservative: it only
/// ever hides frames that are *never* the app's own code, rather than
/// guessing more broadly and risking hiding something relevant.
const RUNTIME_NOISE_PREFIXES: &[&str] = &[
    "std::",
    "core::",
    "alloc::",
    "__rust_",
    "rust_begin_unwind",
    "backtrace::",
    "backtrace_rs::",
    "tokio::",
    "<unknown>",
];

fn classify_as_runtime_noise(frame_text: &str) -> bool {
    let first_line = frame_text.lines().next().unwrap_or("");
    let symbol = first_line
        .split_once(':')
        .map(|(_, rest)| rest.trim())
        .unwrap_or(first_line);
    RUNTIME_NOISE_PREFIXES
        .iter()
        .any(|prefix| symbol.starts_with(prefix))
}

/// Parses `Backtrace`'s own `Display` output into individual frames - there
/// is no stable, structured per-frame API on `std::backtrace::Backtrace`
/// itself (only `Display`/`Debug`, producing pre-formatted text), so this
/// has to work from that text directly rather than real frame objects.
fn parse_backtrace_frames(text: &str) -> Vec<Frame> {
    let mut frames: Vec<Frame> = Vec::new();
    for line in text.lines() {
        if is_frame_header_line(line) {
            frames.push(Frame {
                text: line.to_string(),
                is_runtime_noise: false,
            });
        } else if let Some(last) = frames.last_mut() {
            last.text.push('\n');
            last.text.push_str(line);
        }
        // A line before any frame header at all (shouldn't happen given
        // `Backtrace`'s own fixed format, but not worth panicking the
        // error page itself over) is simply dropped.
    }
    for frame in &mut frames {
        frame.is_runtime_noise = classify_as_runtime_noise(&frame.text);
    }
    frames
}

/// Renders `frames` as HTML, grouping any *run* of consecutive
/// runtime-noise frames into one collapsed `<details>` region (native,
/// keyboard-accessible, needs no JavaScript) rather than showing every
/// single one at full prominence - noise can cluster at both the start of
/// a backtrace (panic/unwind machinery) and the end (tokio's own task-
/// polling loop, `main`), so this groups each *contiguous* run
/// independently instead of assuming there's only ever one.
fn render_backtrace_frames_html(frames: &[Frame]) -> String {
    let mut html = String::new();
    let mut i = 0;
    while i < frames.len() {
        if frames[i].is_runtime_noise {
            let start = i;
            while i < frames.len() && frames[i].is_runtime_noise {
                i += 1;
            }
            let hidden = &frames[start..i];
            let joined = hidden
                .iter()
                .map(|f| f.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            html.push_str(&format!(
                "<details class=\"bt-noise\"><summary>{} runtime frame{} hidden</summary><pre>{}</pre></details>",
                hidden.len(),
                if hidden.len() == 1 { "" } else { "s" },
                escape_html(&joined),
            ));
        } else {
            html.push_str(&format!(
                "<pre class=\"bt-frame\">{}</pre>",
                escape_html(&frames[i].text)
            ));
            i += 1;
        }
    }
    html
}

/// Renders the "Stack trace" section for a panic - absent entirely for
/// every other `AppError` variant, which has a `source()` chain instead
/// (already rendered as its own cause cards by the caller).
fn render_backtrace_section(backtrace: &PanicBacktrace) -> String {
    match backtrace {
        PanicBacktrace::Captured(text) => {
            let frames = parse_backtrace_frames(text);
            format!(
                r#"<div class="section">
  <h2>Stack trace</h2>
  <div class="backtrace">{}</div>
</div>"#,
                render_backtrace_frames_html(&frames)
            )
        }
        PanicBacktrace::Disabled => r#"<div class="section">
  <h2>Stack trace</h2>
  <p class="hint">No backtrace was captured. Set <code>RUST_BACKTRACE=1</code> (or run <code>xr dev --debug</code>, which sets it automatically) to see one here next time.</p>
</div>"#
            .to_string(),
        PanicBacktrace::Unsupported => r#"<div class="section">
  <h2>Stack trace</h2>
  <p class="hint">Backtraces aren't supported on this platform/build.</p>
</div>"#
            .to_string(),
    }
}

/// Self-contained (no external CSS/JS, no build step) - this has to render
/// standalone even as the very first response a broken app ever produces.
///
/// `messages` is the ordered list of "what went wrong" entries: the
/// top-level error first, then each `source()` cause in order (or just one
/// entry, for `NotFound`/a panic, which have no chain at all) - rendered as
/// its own distinct card rather than one big concatenated string with
/// hand-inserted "Caused by:" text, both for readability on a long chain
/// and so a single very long message doesn't force the *entire* page to
/// scroll instead of just its own card (reported directly, from a real
/// overflow bug in `xr dev`'s own build-failure placeholder page - see
/// `docs/GOTCHAS.md` - that this page's design deliberately avoids
/// repeating).
///
/// `backtrace` is only ever `Some` for a panic (see `render_panic`) - every
/// other `AppError` variant has a real `source()` chain instead, which
/// `messages` already captures.
fn debug_page(
    status: StatusCode,
    title: &str,
    messages: &[String],
    backtrace: Option<PanicBacktrace>,
) -> Response {
    let (first, rest) = messages.split_first().expect("messages is never empty");
    let causes_html: String = rest
        .iter()
        .map(|message| {
            format!(
                "<div class=\"cause\"><span class=\"cause-label\">Caused by</span><pre>{}</pre></div>",
                escape_html(message)
            )
        })
        .collect();
    let backtrace_html = backtrace
        .as_ref()
        .map(render_backtrace_section)
        .unwrap_or_default();

    let html = format!(
        r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{status} {title}</title>
<style>
{style}
</style>
</head>
<body>
<div class="page">
  <header>
    <div class="brand">
      <svg viewBox="0 0 48 48" role="img" aria-hidden="true"><path fill="#ff735f" d="M12 0h24c6.63 0 12 5.37 12 12v24c0 6.63-5.37 12-12 12H0V12C0 5.37 5.37 0 12 0Z"/><path fill="#fff" d="M13.25 30.59a1 1 0 0 1-.76-1.64l4.7-5.59-4.69-5.42a1 1 0 1 1 1.51-1.31l5.25 6.07a1 1 0 0 1 0 1.3l-5.25 6.24a1 1 0 0 1-.76.35Z"/><path fill="#fff" d="M32.75 34.73h-12a1 1 0 1 1 0-2h12a1 1 0 1 1 0 2Z"/></svg>
      <span>larust</span>
    </div>
    {theme_switch}
  </header>
  <main>
    <div class="badges">
      <span class="badge badge-status">{status}</span>
      <span class="badge badge-title">{title}</span>
    </div>
    <div class="cause cause-primary"><pre>{first}</pre></div>
    {causes_html}
    {backtrace_html}
  </main>
  <footer>Shown because <code>APP_DEBUG=true</code>. Never enable this outside local development.</footer>
</div>
{theme_script}
</body>
</html>"##,
        status = status.as_u16(),
        title = escape_html(title),
        first = escape_html(first),
        style = DEBUG_PAGE_STYLE,
        theme_switch = THEME_SWITCH_HTML,
        theme_script = THEME_SWITCH_SCRIPT,
    );

    (status, [(CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
}

/// Color tokens reuse this framework's own established brand palette
/// (`#f4513d`/`#ff735f`, already used by `larust_cli::dev_placeholder` and
/// every generated app's own scaffolded CSS) and the exact light-mode
/// values `demo`'s own stylesheet already defines (`--ink`/`--muted`/
/// `--line`/`--paper`) - this page should look like a deliberate part of
/// Larust, not a generic reskin of Laravel's own debug page, even though
/// the *idea* (a real full-page error view instead of a bare `<pre>` dump)
/// is the same one Laravel/Ignition already proved out well.
const DEBUG_PAGE_STYLE: &str = r#"
:root {
  color-scheme: dark;
  --bg: #171513; --panel: #211e1b; --panel-2: #272522; --border: #3a352e;
  --ink: #f8f3eb; --muted: #a99d90; --accent: #ff735f; --accent-soft: #3a2420;
  font-family: Inter, ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif;
}
@media (prefers-color-scheme: light) {
  :root:not([data-theme="dark"]) {
    --bg: #f4f0e8; --panel: #fffdf9; --panel-2: #f4f0e8; --border: #e4ddd2;
    --ink: #202124; --muted: #6b6d73; --accent: #cf3628; --accent-soft: #fbe6e3;
  }
}
:root[data-theme="light"] {
  --bg: #f4f0e8; --panel: #fffdf9; --panel-2: #f4f0e8; --border: #e4ddd2;
  --ink: #202124; --muted: #6b6d73; --accent: #cf3628; --accent-soft: #fbe6e3;
}
* { box-sizing: border-box; }
body { margin: 0; background: var(--bg); color: var(--ink); }
.page { max-width: 64rem; margin: 0 auto; padding: 1.5rem clamp(1rem, 4vw, 2.5rem) 3rem; }
header { display: flex; align-items: center; justify-content: space-between; padding: .5rem 0 1.5rem; }
.brand { display: inline-flex; align-items: center; gap: .6rem; font-size: 1.05rem; font-weight: 800; letter-spacing: -.03em; }
.brand svg { width: 1.75rem; height: 1.75rem; flex: none; }
.badges { display: flex; gap: .5rem; flex-wrap: wrap; margin-bottom: 1rem; }
.badge { display: inline-flex; align-items: center; padding: .3rem .7rem; border-radius: 999px; font-size: .78rem; font-weight: 700; }
.badge-status { background: var(--accent-soft); color: var(--accent); }
.badge-title { background: var(--panel-2); color: var(--muted); border: 1px solid var(--border); }
.cause { margin-bottom: .9rem; background: var(--panel); border: 1px solid var(--border); border-radius: .8rem; padding: 1rem 1.1rem; }
.cause-primary { border-color: var(--accent); }
.cause-label { display: block; margin-bottom: .4rem; color: var(--muted); font-size: .7rem; font-weight: 800; letter-spacing: .09em; text-transform: uppercase; }
.cause pre, .section pre { margin: 0; overflow-wrap: anywhere; white-space: pre-wrap; font: .85rem/1.6 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }
.section { margin-top: 1.5rem; }
.section h2 { font-size: .85rem; font-weight: 800; letter-spacing: .06em; text-transform: uppercase; color: var(--muted); margin: 0 0 .7rem; }
.hint { color: var(--muted); font-size: .9rem; }
.backtrace { display: flex; flex-direction: column; gap: .4rem; }
.bt-frame { background: var(--panel); border: 1px solid var(--border); border-radius: .6rem; padding: .6rem .8rem; }
.bt-noise { background: var(--panel-2); border: 1px dashed var(--border); border-radius: .6rem; }
.bt-noise summary { cursor: pointer; padding: .5rem .8rem; color: var(--muted); font-size: .82rem; font-weight: 650; }
.bt-noise[open] summary { border-bottom: 1px dashed var(--border); }
.bt-noise pre { padding: .6rem .8rem; }
footer { margin-top: 2rem; color: var(--muted); font-size: .82rem; }
footer code { background: var(--panel-2); border-radius: .3rem; padding: .1rem .35rem; }
.theme-switch { position: relative; }
.theme-switch button.toggle { display: inline-flex; align-items: center; justify-content: center; width: 2.2rem; height: 2.2rem; border-radius: .6rem; border: 1px solid var(--border); background: var(--panel); color: var(--muted); cursor: pointer; }
.theme-switch button.toggle svg { width: 1.1rem; height: 1.1rem; }
.theme-menu { position: absolute; right: 0; top: calc(100% + .4rem); min-width: 8rem; background: var(--panel); border: 1px solid var(--border); border-radius: .6rem; padding: .3rem; box-shadow: 0 10px 30px rgba(0,0,0,.25); z-index: 10; }
.theme-menu button { display: block; width: 100%; text-align: left; padding: .45rem .6rem; border: 0; background: none; color: var(--ink); font: inherit; font-size: .85rem; border-radius: .4rem; cursor: pointer; }
.theme-menu button:hover { background: var(--panel-2); }
"#;

const THEME_SWITCH_HTML: &str = r#"<div class="theme-switch">
      <button type="button" class="toggle" id="larust-theme-toggle" aria-haspopup="true" aria-expanded="false" aria-label="Change color theme">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M6.34 17.66l-1.41 1.41M19.07 4.93l-1.41 1.41"/></svg>
      </button>
      <div class="theme-menu" id="larust-theme-menu" hidden>
        <button type="button" data-theme-choice="light">Light</button>
        <button type="button" data-theme-choice="dark">Dark</button>
        <button type="button" data-theme-choice="system">System</button>
      </div>
    </div>"#;

/// Persists to `localStorage`, wrapped in `try`/`catch` throughout -
/// private browsing or a blocked-storage policy can make any call throw,
/// and this page has to keep working (just without persistence) even then,
/// not break the one interactive thing on an already-broken-app's error
/// page.
const THEME_SWITCH_SCRIPT: &str = r#"<script>
(function () {
  var root = document.documentElement;
  var STORAGE_KEY = 'larust-debug-theme';
  function apply(theme) {
    if (theme === 'dark') { root.setAttribute('data-theme', 'dark'); }
    else if (theme === 'light') { root.setAttribute('data-theme', 'light'); }
    else { root.removeAttribute('data-theme'); }
  }
  var saved = null;
  try { saved = localStorage.getItem(STORAGE_KEY); } catch (e) {}
  apply(saved || 'system');

  var button = document.getElementById('larust-theme-toggle');
  var menu = document.getElementById('larust-theme-menu');
  button.addEventListener('click', function () {
    var opening = menu.hidden;
    menu.hidden = !opening;
    button.setAttribute('aria-expanded', String(opening));
  });
  Array.prototype.forEach.call(menu.querySelectorAll('[data-theme-choice]'), function (option) {
    option.addEventListener('click', function () {
      var theme = option.getAttribute('data-theme-choice');
      apply(theme);
      try { localStorage.setItem(STORAGE_KEY, theme); } catch (e) {}
      menu.hidden = true;
      button.setAttribute('aria-expanded', 'false');
    });
  });
  document.addEventListener('click', function (event) {
    if (!menu.hidden && !event.target.closest('.theme-switch')) {
      menu.hidden = true;
      button.setAttribute('aria-expanded', 'false');
    }
  });
})();
</script>"#;

fn escape_html(s: &str) -> String {
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
    fn is_frame_header_line_matches_a_real_backtrace_frame_header() {
        assert!(is_frame_header_line("   0: rust_begin_unwind"));
        assert!(is_frame_header_line("  12: core::panicking::panic_fmt"));
        assert!(!is_frame_header_line(
            "             at /rustc/abc/library/std/src/panicking.rs:665:5"
        ));
        assert!(!is_frame_header_line(""));
    }

    #[test]
    fn classify_as_runtime_noise_flags_known_stdlib_and_runtime_prefixes() {
        assert!(classify_as_runtime_noise(
            "   0: rust_begin_unwind\n             at /rustc/abc/library/std/src/panicking.rs:665:5"
        ));
        assert!(classify_as_runtime_noise(
            "   1: core::panicking::panic_fmt"
        ));
        assert!(classify_as_runtime_noise(
            "   2: tokio::runtime::task::core::Core<T,S>::poll"
        ));
        assert!(!classify_as_runtime_noise("   3: demo::handlers::broken"));
    }

    #[test]
    fn parse_backtrace_frames_groups_a_multi_line_frame_together() {
        let text = "   0: rust_begin_unwind\n             at /rustc/abc/library/std/src/panicking.rs:665:5\n   1: demo::handlers::broken\n             at ./src/handlers.rs:10:5";
        let frames = parse_backtrace_frames(text);
        assert_eq!(frames.len(), 2);
        assert!(frames[0].text.contains("rust_begin_unwind"));
        assert!(frames[0].text.contains("panicking.rs:665:5"));
        assert!(frames[1].text.contains("demo::handlers::broken"));
    }

    /// Uses a *real* captured backtrace (`force_capture`, which ignores
    /// `RUST_BACKTRACE` entirely - unlike `capture()`, whose env-var check
    /// is cached process-wide after the first call, making it unreliable
    /// to toggle from within a shared test binary; see `std::backtrace`'s
    /// own doc comment) so this proves the parser against Rust's own real
    /// output format, not a hand-constructed approximation of it that could
    /// drift from what `Backtrace::capture()` actually produces.
    #[test]
    fn parse_backtrace_frames_handles_a_real_captured_backtrace() {
        let backtrace = std::backtrace::Backtrace::force_capture();
        let frames = parse_backtrace_frames(&backtrace.to_string());
        assert!(
            !frames.is_empty(),
            "a force-captured backtrace must parse into at least one frame"
        );
        // Every real backtrace from inside this test binary's own process
        // includes genuine Rust-runtime frames (test harness setup, if
        // nothing else) - this doesn't assert on any *specific* frame
        // (fragile against toolchain/std version drift), just that the
        // classifier isn't a no-op that flags nothing at all.
        assert!(
            frames.iter().any(|f| f.is_runtime_noise),
            "a real backtrace must contain at least one recognizable runtime frame"
        );
    }

    #[test]
    fn render_backtrace_frames_html_collapses_consecutive_noisy_frames_into_one_region() {
        let frames = vec![
            Frame {
                text: "   0: rust_begin_unwind".to_string(),
                is_runtime_noise: true,
            },
            Frame {
                text: "   1: core::panicking::panic_fmt".to_string(),
                is_runtime_noise: true,
            },
            Frame {
                text: "   2: demo::handlers::broken".to_string(),
                is_runtime_noise: false,
            },
            Frame {
                text: "   3: tokio::runtime::task::core::Core<T,S>::poll".to_string(),
                is_runtime_noise: true,
            },
        ];
        let html = render_backtrace_frames_html(&frames);
        assert_eq!(
            html.matches("<details class=\"bt-noise\">").count(),
            2,
            "two separate noisy runs (leading and trailing) must become two separate collapsed regions, not one"
        );
        assert!(html.contains("2 runtime frames hidden"));
        assert!(html.contains("1 runtime frame hidden"));
        assert!(html.contains("demo::handlers::broken"));
    }

    /// `debug_page` is called directly here, not through `render_panic`/
    /// `AppError::into_response` - both of those gate on the process-wide
    /// `debug::is_enabled()` `OnceLock`, which stays `false` for this
    /// entire shared unit-test binary (nothing in *this* module ever calls
    /// `Application::new()` to flip it, and flipping it directly would
    /// permanently commit that choice for every other unit test sharing
    /// this same process - the exact conflict `application.rs`'s own test
    /// module doc comment already documents avoiding). `debug_page` itself
    /// has no such gate - only its callers do - so calling it directly
    /// exercises the real rendering logic in full isolation from that
    /// global flag. The gate itself (does `APP_DEBUG=true` actually route
    /// here at all) is covered separately, in a real separate process, by
    /// `tests/error_response_debug_mode.rs`.
    async fn body_text(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn debug_page_renders_each_cause_as_its_own_card() {
        let response = debug_page(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error",
            &["top-level failure".to_string(), "root cause".to_string()],
            None,
        );
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = body_text(response).await;
        assert!(body.contains("top-level failure"));
        assert!(body.contains("root cause"));
        assert!(body.contains("Caused by"));
        // The theme switcher is always present, regardless of which
        // `AppError` variant produced the page.
        assert!(body.contains("larust-theme-toggle"));
    }

    #[tokio::test]
    async fn debug_page_shows_a_hint_when_backtraces_are_disabled() {
        let response = debug_page(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error (panic)",
            &["boom".to_string()],
            Some(PanicBacktrace::Disabled),
        );
        let body = body_text(response).await;
        assert!(body.contains("RUST_BACKTRACE=1"));
        assert!(body.contains("xr dev --debug"));
        // No `source()` chain for a panic - must never show a stray
        // "Caused by" card with nothing behind it.
        assert!(!body.contains("Caused by"));
    }

    #[tokio::test]
    async fn debug_page_shows_a_hint_when_backtraces_are_unsupported() {
        let response = debug_page(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error (panic)",
            &["boom".to_string()],
            Some(PanicBacktrace::Unsupported),
        );
        let body = body_text(response).await;
        assert!(body.contains("aren't supported on this platform"));
    }

    #[tokio::test]
    async fn debug_page_renders_a_captured_backtrace_with_noisy_frames_collapsed() {
        let backtrace = concat!(
            "   0: rust_begin_unwind\n",
            "             at /rustc/abc/library/std/src/panicking.rs:665:5\n",
            "   1: demo::handlers::broken\n",
            "             at ./src/handlers.rs:10:5\n",
            "   2: tokio::runtime::task::core::Core<T,S>::poll\n",
        );
        let response = debug_page(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error (panic)",
            &["boom".to_string()],
            Some(PanicBacktrace::Captured(backtrace.to_string())),
        );
        let body = body_text(response).await;
        assert!(body.contains("Stack trace"));
        assert!(body.contains("demo::handlers::broken"));
        assert!(body.contains("bt-noise"));
        assert!(!body.contains("RUST_BACKTRACE=1"));
    }
}
