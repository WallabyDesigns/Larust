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
/// shutdown.
pub async fn serve(listener: TcpListener, message: SharedMessage, stop: Arc<Notify>) {
    loop {
        tokio::select! {
            () = stop.notified() => return,
            accepted = listener.accept() => {
                let Ok((stream, _addr)) = accepted else { continue };
                let message = Arc::clone(&message);
                tokio::spawn(async move {
                    let _ = respond(stream, &message).await;
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
async fn respond(mut stream: TcpStream, message: &SharedMessage) -> std::io::Result<()> {
    let mut discard = [0u8; 1024];
    let _ = tokio::time::timeout(Duration::from_millis(200), stream.read(&mut discard)).await;

    let text = message
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let body = render_page(&text);
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

fn render_page(message: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta http-equiv="refresh" content="2">
  <title>xr dev</title>
  <style>
    body {{ font-family: ui-sans-serif, system-ui, sans-serif; background: #111827; color: #f9fafb; display: grid; place-items: center; min-height: 100vh; margin: 0; }}
    main {{ max-width: 40rem; padding: 2rem; }}
    pre {{ white-space: pre-wrap; word-break: break-word; background: #1f2937; padding: 1rem; border-radius: .5rem; }}
  </style>
</head>
<body>
  <main>
    <h1>xr dev</h1>
    <pre>{escaped}</pre>
    <p>This page refreshes automatically once your app builds.</p>
  </main>
</body>
</html>"#,
        escaped = html_escape(message)
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
        let page = render_page("build failed: missing semicolon");
        assert!(page.contains("build failed: missing semicolon"));
        assert!(page.contains(r#"<meta http-equiv="refresh" content="2">"#));
    }

    #[test]
    fn render_page_escapes_html_in_the_message() {
        let page = render_page("<script>alert(1)</script>");
        assert!(!page.contains("<script>"));
        assert!(page.contains("&lt;script&gt;"));
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
