//! Genuine server-*pushed* real-time updates - `@live('channel') ...
//! @endlive` in a template, paired with a [`broadcast`] call wherever
//! server state actually changes. Deliberately a *different* mechanism
//! from `@wire`'s reactive components, not a bigger version of the same
//! one: `@wire` is client-*initiated* (a user's own `wire:model`/
//! `wire:click`/`wire:submit` triggers an AJAX round-trip that only ever
//! updates *that visitor's own* view); `@live` is server-*initiated* (any
//! server-side event pushes an update to *every currently connected
//! viewer* of that channel, with zero interaction required in any of
//! their tabs - the live-chat/live-notification case neither `@wire` nor
//! real Livewire itself can do, since both are built around a
//! request/response cycle, not a held-open connection).
//!
//! Real-time push is the one thing PHP/Laravel genuinely can't do
//! natively (no long-running process to hold a WebSocket open across
//! requests - Laravel's own answer is an external system, Echo/Reverb).
//! Larust has no such constraint: it's already one long-running process,
//! same as everything else this framework relies on (`larust_orm::pool()`,
//! `larust-live`'s own component registry), so this is a native fit, not
//! a bolt-on.
//!
//! No component trait, no session state, no server-side struct at all -
//! deliberately simpler than `@wire`'s `WireComponent` machinery.
//! `@live(channel) ... @endlive` just renders its body once, normally, at
//! page-load time (in the *caller's* own scope, same as `@loadonce`'s
//! body), wrapped in a `<div data-live-channel="...">`. From then on,
//! whoever calls [`broadcast`] is responsible for constructing new HTML
//! shaped the same way ([`wrap`] helps) and pushing it to every open
//! socket on that channel - there's no "component" the framework
//! re-renders on the app's behalf, since there's nothing for it to
//! re-render *from* (no stored state to re-render against). This is a
//! deliberate simplicity trade: the app owns keeping the initial render
//! and each broadcast payload in sync, in exchange for not needing to
//! reinvent `@wire`'s stateful-component complexity for what's meant to
//! be a thin, real-time notification layer.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::Path;
use axum::http::header::CONTENT_TYPE;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::{broadcast, OwnedSemaphorePermit, Semaphore};

/// How many not-yet-delivered broadcasts a channel buffers before a slow
/// subscriber starts missing the oldest ones (`RecvError::Lagged`, handled
/// by just catching up to the newest available message - a dropped
/// intermediate update is an accepted v1 tradeoff for "the page eventually
/// reflects the latest state," not a "never miss an update" delivery
/// guarantee this isn't trying to make).
const CHANNEL_CAPACITY: usize = 32;

/// Bounds process memory consumed by attacker-controlled channel names.
const MAX_CHANNELS: usize = 1_024;
/// Bounds file descriptors/tasks consumed by long-lived WebSocket clients.
const MAX_PUSH_CONNECTIONS: usize = 1_024;
const MAX_CHANNEL_NAME_BYTES: usize = 128;

type ChannelMap = HashMap<String, broadcast::Sender<String>>;

static CHANNELS: OnceLock<Mutex<ChannelMap>> = OnceLock::new();
static CONNECTIONS: OnceLock<Arc<Semaphore>> = OnceLock::new();

/// Process-wide, per-channel-name broadcast sender - same single-process
/// `OnceLock` shape `larust_orm::pool()`/`larust-live`'s own component
/// registry already use (this framework has no multi-worker story
/// anywhere yet). Channels are created lazily on first use (by either a
/// `broadcast()` call or an incoming WebSocket subscription, whichever
/// happens first). Their count is bounded by [`MAX_CHANNELS`] so untrusted
/// subscription paths cannot grow process memory without limit.
fn sender_for(channel: &str) -> Option<broadcast::Sender<String>> {
    let mut channels = CHANNELS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(sender) = channels.get(channel) {
        return Some(sender.clone());
    }
    if channels.len() >= MAX_CHANNELS {
        tracing::warn!(
            channel,
            max_channels = MAX_CHANNELS,
            "live channel limit reached"
        );
        return None;
    }
    let sender = broadcast::channel(CHANNEL_CAPACITY).0;
    channels.insert(channel.to_string(), sender.clone());
    Some(sender)
}

/// Pushes `html` to every browser tab currently subscribed to `channel`
/// via an open `@live(...)` WebSocket - call this from wherever server
/// state actually changes (an event listener, a controller action), not
/// from inside the request that's rendering the page itself. A no-op, not
/// an error, when nobody's listening (`Sender::send` returning `Err` only
/// ever means "zero receivers exist right now"): ordinary fire-and-forget
/// pub/sub semantics, since there's no reason a broadcast should fail just
/// because no browser tab happens to have this channel open at the
/// moment.
///
/// `html` should be shaped like the `<div data-live-channel="...">...
/// </div>` wrapper `@live(...)`'s own initial render produces - see
/// [`wrap`] for building that shape by hand from a plain HTML fragment.
pub fn broadcast(channel: &str, html: impl Into<String>) {
    if valid_channel_name(channel) {
        if let Some(sender) = sender_for(channel) {
            let _ = sender.send(html.into());
        }
    } else {
        tracing::warn!(channel, "refusing broadcast to invalid live channel name");
    }
}

/// Wraps `inner_html` in the same `data-live-channel`-carrying `<div>`
/// `@live(...)`'s own codegen produces for its initial render - the
/// client's DOM patcher matches an incoming broadcast against the
/// existing root element by *replacing* it wholesale if position/tag
/// don't line up, so a broadcast payload needs this exact wrapper shape,
/// not just the inner content, to patch cleanly in place instead of being
/// discarded or mismatched.
pub fn wrap(channel: &str, inner_html: &str) -> String {
    format!(
        r#"<div data-live-channel="{}">{inner_html}</div>"#,
        larust_view::escape(channel)
    )
}

/// `GET /__larust_push/{channel}` - upgrades to a WebSocket and streams
/// every [`broadcast`] call's HTML straight through, verbatim, for the
/// lifetime of the connection. Pure server → client push in v1: no
/// client → server message is ever acted on (an inbound message is only
/// read at all to detect the browser tab closing the connection).
///
/// Deliberately outside any CSRF check (unlike `@wire(...)`'s `update`
/// route) - CSRF protects against an *attacker-initiated state change*
/// riding the victim's cookies, and this endpoint never processes a
/// client-originated state change at all, only pushes data out. If a
/// channel carries anything sensitive, gate it the same way any other
/// route would be gated - this handler is registered explicitly by the
/// app (matching `@wire(...)`'s own routes), so ordinary middleware
/// composition already covers it; no per-channel authorization exists at
/// the framework level in v1.
pub async fn socket(Path(channel): Path<String>, ws: WebSocketUpgrade) -> Response {
    if !valid_channel_name(&channel) {
        return (StatusCode::BAD_REQUEST, "invalid live channel").into_response();
    }

    let permits = CONNECTIONS
        .get_or_init(|| Arc::new(Semaphore::new(MAX_PUSH_CONNECTIONS)))
        .clone();
    let Ok(permit) = permits.try_acquire_owned() else {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "live connection limit reached",
        )
            .into_response();
    };
    let Some(sender) = sender_for(&channel) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "live channel limit reached",
        )
            .into_response();
    };

    ws.on_upgrade(move |socket| handle_socket(socket, sender, permit))
        .into_response()
}

async fn handle_socket(
    mut socket: WebSocket,
    sender: broadcast::Sender<String>,
    _permit: OwnedSemaphorePermit,
) {
    let mut updates = sender.subscribe();
    loop {
        tokio::select! {
            update = updates.recv() => {
                match update {
                    Ok(html) => {
                        if socket.send(Message::Text(html)).await.is_err() {
                            // The browser tab navigated away or closed -
                            // nothing left to push to.
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            incoming = socket.recv() => {
                // Only read to detect the client closing the connection
                // (`None`) or the transport erroring - the *content* of
                // any client -> server message is ignored; this is a
                // push-only channel in v1.
                if incoming.is_none() {
                    break;
                }
            }
        }
    }
}

/// Keep channel identifiers predictable and safely bounded. Dynamic channel
/// names remain supported (for example `orders.42`), but clients cannot use
/// arbitrary long or control-character-bearing path values to consume memory.
fn valid_channel_name(channel: &str) -> bool {
    !channel.is_empty()
        && channel.len() <= MAX_CHANNEL_NAME_BYTES
        && channel
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

const RUNTIME_JS: &str = include_str!("../assets/push-runtime.js");

/// `GET /__larust_push/runtime.js` - the vendored client runtime, same
/// "served from the installed crate itself, not copied into
/// `public/js/`" reasoning as `crate::routes::runtime_js`.
pub async fn runtime_js() -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "text/javascript; charset=utf-8")],
        RUNTIME_JS,
    )
}

/// The two routes `@live(...)`/[`broadcast`] need, bundled for
/// [`larust_http::Router::plugin`] - sugar for the two `.get` calls this
/// module's own doc comment shows an app writing by hand today.
pub struct PushPlugin;

impl larust_http::Plugin for PushPlugin {
    fn routes(&self) -> larust_http::Router {
        larust_http::Router::new()
            .get("/__larust_push/runtime.js", runtime_js)
            .get("/__larust_push/{channel}", socket)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_plugin_contributes_exactly_the_two_routes_an_app_used_to_hand_write() {
        let routes = larust_http::Router::new().plugin(PushPlugin).routes();

        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0].method, "GET");
        assert_eq!(routes[0].path, "/__larust_push/runtime.js");
        assert_eq!(routes[1].method, "GET");
        assert_eq!(routes[1].path, "/__larust_push/{channel}");
    }

    #[test]
    fn wrap_produces_the_same_shape_the_live_directive_renders() {
        let html = wrap("posts.count", "<span>5 posts</span>");
        assert_eq!(
            html,
            r#"<div data-live-channel="posts.count"><span>5 posts</span></div>"#
        );
    }

    #[test]
    fn wrap_escapes_the_channel_name() {
        let html = wrap("a\"b", "x");
        assert!(!html.contains("a\"b\""), "channel name should be escaped");
    }

    #[tokio::test]
    async fn broadcast_with_no_subscribers_is_a_harmless_no_op() {
        broadcast("nobody-is-listening-to-this-channel", "<p>hi</p>");
    }

    #[tokio::test]
    async fn a_subscriber_receives_a_broadcast_sent_after_it_subscribed() {
        let mut rx = sender_for("subscriber-test-channel").unwrap().subscribe();
        broadcast("subscriber-test-channel", "<p>update</p>");
        let received = rx.recv().await.unwrap();
        assert_eq!(received, "<p>update</p>");
    }

    #[test]
    fn channel_names_are_bounded_and_reject_control_characters() {
        assert!(valid_channel_name("orders.42_status"));
        assert!(!valid_channel_name(""));
        assert!(!valid_channel_name("orders/42"));
        assert!(!valid_channel_name("orders\n42"));
        assert!(!valid_channel_name(&"a".repeat(MAX_CHANNEL_NAME_BYTES + 1)));
    }
}
