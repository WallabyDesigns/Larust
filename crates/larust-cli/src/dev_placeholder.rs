//! A minimal, hand-rolled HTTP responder `xr dev` binds and serves on the
//! app's own port from the moment it starts — before the very first build
//! has ever run. Closes a real gap: every rebuild *after* a server has
//! come up already leaves the last good build serving on a failure (see
//! `dev.rs`'s own doc comment), but before that first success there was
//! nothing listening at all, so a request got a bare connection-refused
//! rather than something a developer could act on. This placeholder shows
//! "building…"/"build failed: ..." instead, and is retired the instant
//! the first successful build takes over the same socket via a real
//! handoff (`larust_core::__internal::handoff`), not before.
//!
//! No `axum`/`larust-http` dependency for this — `larust-cli` has never
//! needed one, and a single fixed response doesn't justify adding one; a
//! hand-rolled HTTP/1.1 response over a raw `TcpStream` is the same
//! "minimal mechanism over a crate" choice this codebase already makes
//! for the fd-passing code itself (`larust_core::lifecycle::listener`).

use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;

/// What the placeholder currently shows a request — read fresh on every
/// connection, never cached at spawn time, so a build failure's error
/// text shows up on the very next request without restarting anything.
pub type SharedMessage = Arc<Mutex<String>>;

pub fn initial_message() -> SharedMessage {
    Arc::new(Mutex::new(
        "Building your app for the first time…".to_string(),
    ))
}

pub fn set_message(message: &SharedMessage, text: impl Into<String>) {
    let mut guard = message
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = text.into();
}

/// Accepts connections on `listener` until `stop` is notified, answering
/// every one with the current contents of `message`. Returns immediately
/// once stopped — there's nothing meaningful to drain (each connection is
/// answered in one shot and closed), unlike the real app's own graceful
/// shutdown. `app_name` is the real app's own `Config::app_name` (or its
/// `"Larust"` default) — shown in the page title/heading in place of the
/// generic "xr dev", so the placeholder for e.g. a `demo` app reads
/// "demo", matching what the real app's own pages will say once it's up.
pub async fn serve(
    listener: TcpListener,
    app_name: String,
    message: SharedMessage,
    stop: Arc<Notify>,
) {
    let app_name = Arc::new(app_name);
    loop {
        tokio::select! {
            () = stop.notified() => return,
            accepted = listener.accept() => {
                let Ok((stream, _addr)) = accepted else { continue };
                let message = Arc::clone(&message);
                let app_name = Arc::clone(&app_name);
                tokio::spawn(async move {
                    let _ = respond(stream, &app_name, &message).await;
                });
            }
        }
    }
}

/// Best-effort drain of whatever the client already sent (a real request
/// never needs more than this to have arrived) before writing the fixed
/// response — writing back to a socket the OS still has unread inbound
/// bytes buffered on can otherwise show up as a reset to some clients.
/// Bounded so a client that never sends anything (unlikely for a browser
/// or curl, but not impossible) can't hang this connection's task.
async fn respond(
    mut stream: TcpStream,
    app_name: &str,
    message: &SharedMessage,
) -> std::io::Result<()> {
    let mut discard = [0u8; 1024];
    let _ = tokio::time::timeout(Duration::from_millis(200), stream.read(&mut discard)).await;

    let text = message
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let body = render_page(app_name, &text);
    let response = format!(
        "HTTP/1.1 503 Service Unavailable\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).await
}

/// Reloads the instant a *real* app takes over this same socket — no blind
/// polling. `/__larust_dev` only exists on the real app's own router, and
/// only under `LARUST_DEV_RELOAD` (`larust_core::Application::serve`,
/// `dev_reload.rs`), which `xr dev` sets on exactly the child it spawns as
/// this placeholder's eventual replacement (see `handoff`). Until that
/// handoff happens, *every* request to this placeholder — including one to
/// this exact path — gets the same fixed 503 `text/html` response from
/// `respond()` above, regardless of path.
///
/// That response arriving at all is exactly why this can't lean on
/// `EventSource`'s own built-in reconnect: per the spec, a *network*-level
/// failure (connection refused, reset before headers) is what triggers
/// automatic retry — but a response that actually arrives with the wrong
/// status/`Content-Type` (503 `text/html`, precisely this placeholder's
/// response) makes the user agent "fail the connection" instead, which sets
/// `readyState` to `CLOSED` *permanently and does not reconnect on its
/// own*. Relying on the default here would mean the very first attempt
/// (near-certainly against the still-building placeholder) kills the
/// `EventSource` for good, long before the real app ever comes up — so
/// `onerror` below recreates a fresh one after a short delay by hand,
/// standing in for the retry the spec won't provide in this case. The
/// moment the real app answers instead, `/__larust_dev` is a genuine SSE
/// endpoint, `onopen` fires, and that's the reload signal — no second-open
/// bookkeeping needed the way `larust_view::runtime`'s own copy of this
/// script needs for a *running* app's restart-detection, since here
/// literally any successful open only ever means "a real app is now live."
const LIVE_RELOAD_SCRIPT: &str = r#"<script>
(function () {
  function connect() {
    var es = new EventSource('/__larust_dev');
    es.onopen = function () {
      location.reload();
    };
    es.onerror = function () {
      es.close();
      setTimeout(connect, 1000);
    };
  }
  connect();
})();
</script>"#;

fn render_page(app_name: &str, message: &str) -> String {
    let app_name = html_escape(app_name);
    format!(
        r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="theme-color" content="#f4513d">
  {live_reload_script}
  <title>{app_name} · Larust development</title>
  <style>
    :root {{ color-scheme: dark; font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }}
    * {{ box-sizing: border-box; }}
    body {{
      min-height: 100vh; margin: 0; display: grid; place-items: center; overflow: hidden;
      color: #f8f3eb; background: #171513;
    }}
    main {{ width: min(100% - 32px, 42rem); position: relative; }}
    .card {{ padding: clamp(1.5rem, 6vw, 3.5rem); }}
    .brand {{ display: inline-flex; align-items: center; gap: .75rem; color: #fff; font-size: 1.15rem; font-weight: 800; letter-spacing: -.04em; }}
    .brand-mark {{ width: 2.5rem; height: 2.5rem; flex: none; }}
    .eyebrow {{ display: flex; align-items: center; gap: .55rem; margin: 3rem 0 .85rem; color: #ff9a89; font-size: .74rem; font-weight: 800; letter-spacing: .11em; text-transform: uppercase; }}
    h1 {{ margin: 0; max-width: 13ch; font-size: clamp(2.15rem, 7vw, 3.6rem); line-height: .98; letter-spacing: -.07em; }}
    p {{ margin: 1rem 0 0; color: #c7bdb1; font-size: 1rem; line-height: 1.6; }}
    .app-name {{ color: #fff; font-weight: 700; }}
    .status {{ margin-top: 0.2rem; padding: 1rem 1.1rem; color: #f3ece2; background: #272522; border: 1px solid #4c453d; border-radius: .8rem; }}
    .status-label {{ display: block; margin-top: 2rem; margin-bottom: .15rem; color: #a99d90; font-size: .7rem; font-weight: 800; letter-spacing: .1em; text-transform: uppercase; }}
    pre {{ margin: 0; overflow-wrap: anywhere; white-space: pre-wrap; font: .82rem/1.55 ui-monospace, SFMono-Regular, Consolas, monospace; }}
    .refresh {{ display: flex; align-items: center; gap: .55rem; margin-top: 1.3rem; color: #a99d90; font-size: .83rem; }}
    .refresh svg {{ width: 1rem; height: 1rem; color: #ff735f; }}
    @keyframes pulse {{ 70% {{ box-shadow: 0 0 0 .55rem transparent; }} 100% {{ box-shadow: 0 0 0 0 transparent; }} }}
    @media (prefers-reduced-motion: reduce) {{ .pulse {{ animation: none; }} }}
  </style>
</head>
<body>
  <main>
    <section class="card" aria-labelledby="status-heading">
      <div class="brand" aria-label="Larust">
        <svg class="brand-mark" viewBox="0 0 48 48" role="img" aria-hidden="true"><path fill="#ff735f" d="M12 0h24c6.63 0 12 5.37 12 12v24c0 6.63-5.37 12-12 12H0V12C0 5.37 5.37 0 12 0Z"/><path fill="#fff" d="M13.25 30.59a1 1 0 0 1-.76-1.64l4.7-5.59-4.69-5.42a1 1 0 1 1 1.51-1.31l5.25 6.07a1 1 0 0 1 0 1.3l-5.25 6.24a1 1 0 0 1-.76.35Z"/><path fill="#fff" d="M32.75 34.73h-12a1 1 0 1 1 0-2h12a1 1 0 1 1 0 2Z"/></svg>
        <span>larust</span>
      </div>
      <h1 id="status-heading">Your app is on its way.</h1>
      <p>We’re waiting for <span class="app-name">{app_name}</span> to finish building. This page will disappear as soon as your app is ready.</p>
      <span class="status-label">Build status</span>
      <div class="status"><pre>{escaped}</pre></div>
      <div class="refresh"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="M20 11a8.1 8.1 0 0 0-15.5-2M4 5v4h4M4 13a8.1 8.1 0 0 0 15.5 2M20 19v-4h-4"/></svg>This page will reload itself the instant your app is ready</div>
    </section>
  </main>
</body>
</html>"##,
        escaped = html_escape(message),
        live_reload_script = LIVE_RELOAD_SCRIPT,
    )
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_page_includes_the_given_message() {
        let page = render_page("xr dev", "build failed: missing semicolon");
        assert!(page.contains("build failed: missing semicolon"));
    }

    #[test]
    fn render_page_includes_the_live_reload_script_instead_of_a_polling_meta_refresh() {
        let page = render_page("xr dev", "building");
        assert!(page.contains("EventSource('/__larust_dev')"));
        assert!(page.contains("location.reload()"));
        assert!(!page.contains("http-equiv=\"refresh\""));
    }

    #[test]
    fn render_page_manually_retries_the_event_source_instead_of_relying_on_its_built_in_reconnect() {
        // EventSource's own auto-reconnect only fires on a network-level
        // failure — a response that actually arrives with the wrong status/
        // content-type (exactly what this placeholder always sends) makes it
        // give up permanently instead, so the script must drive its own
        // retry via `onerror` rather than trusting the default behavior.
        let page = render_page("xr dev", "building");
        assert!(page.contains("es.onerror"));
        assert!(page.contains("setTimeout(connect"));
    }

    #[test]
    fn render_page_includes_the_given_app_name_in_the_title_and_heading() {
        let page = render_page("demo", "building");
        assert!(page.contains("<title>demo · Larust development</title>"));
        assert!(page.contains("waiting for <span class=\"app-name\">demo</span>"));
        assert!(page.contains("<span>larust</span>"));
    }

    #[test]
    fn render_page_escapes_html_in_the_message() {
        // The page legitimately contains its own `<script>` (the live-reload
        // client) — what must stay escaped is the *message's* payload, so
        // this checks for the raw injection string, not `<script>` at all.
        let page = render_page("xr dev", "<script>alert(1)</script>");
        assert!(!page.contains("<script>alert(1)</script>"));
        assert!(page.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    }

    #[test]
    fn set_message_replaces_the_shared_text() {
        let message = initial_message();
        set_message(&message, "build failed: syntax error");
        assert_eq!(
            *message.lock().unwrap(),
            "build failed: syntax error".to_string()
        );
    }
}
