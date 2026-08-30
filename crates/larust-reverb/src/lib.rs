//! A generic WebSocket pub/sub broadcasting server — Larust's port of
//! `laravel/reverb`. Sibling to, not a replacement for, `larust_live::push`
//! (`@live(channel) ... @endlive`): that mechanism pushes pre-rendered HTML
//! fragments to a fixed `[data-live-channel]` DOM element and has no
//! notion of a named event; this one pushes arbitrary JSON payloads,
//! tagged with an event name, to whatever JS code on the page chooses to
//! subscribe (`LarustReverb.channel(name).listen(eventName, callback)`,
//! not a DOM patch). Deliberately a separate route namespace
//! (`/__larust_reverb/*`, not `/__larust_push/*`) and a separate channel
//! registry — an app that pointed both mechanisms at the same channel
//! name would have `@live`'s client try to DOM-patch a JSON payload, or a
//! Reverb listener receive raw HTML instead of `{event, data}`, so the two
//! contracts stay physically separate rather than trying to share one
//! wire format.
//!
//! The channel/broadcast/WebSocket-upgrade plumbing below intentionally
//! mirrors `larust_live::push`'s own — the same process-wide, lazily
//! created channel-registry shape, the same connection-limiting
//! semaphore, the same bounded/validated channel names — duplicated
//! rather than shared, since it's a small, well-understood mechanism and
//! the two crates' payload contracts (opaque HTML vs. a JSON envelope)
//! are different enough that forcing a shared abstraction across them
//! would cost more clarity than it'd save.
//!
//! # Public vs. private channels
//!
//! A channel name starting with `private-` requires authorization (see
//! [`authorize`]); any other name is public — anyone who can reach the
//! route can subscribe, no different from `@live`'s own channels today.
//! This is Pusher/Laravel's own real naming convention, not invented here.
//!
//! # Deliberately out of scope
//!
//! - **No presence channels** — membership tracking, join/leave events,
//!   a `here()`/`joining()`/`leaving()` client API. A real, separate
//!   feature; not attempted in this version.
//! - **No Pusher-wire-protocol compatibility.** This isn't a drop-in
//!   replacement for `pusher-js` or Laravel Echo — [`authorize`]'s
//!   callback runs directly at WebSocket-upgrade time (the browser
//!   already carries the session cookie to this same-origin request),
//!   not via a separate `POST .../auth` round trip the way real
//!   Pusher/Reverb need to (their split exists because Pusher itself is
//!   a third-party service with no access to the app's own session —
//!   this server *is* the app, so that round trip has no purpose here).
//! - **No per-channel-pattern authorization registry.** Laravel lets an
//!   app register a separate callback per channel-name pattern
//!   (`Broadcast::channel('orders.{id}', fn ($user, $id) => ...)`). This
//!   crate has one global callback instead — it receives the full
//!   channel name and does its own matching inside — avoiding a
//!   route-pattern-matching mini-DSL for what's usually a handful of
//!   `if`/`match` arms in practice.
//!
//! # Example
//!
//! ```ignore
//! // At app boot, next to `larust_support::wire::components()...publish()`:
//! larust_support::reverb::authorize(|session, channel| async move {
//!     let Ok(Some(user)) = larust_support::auth::user::<User>(&session).await else {
//!         return false;
//!     };
//!     // e.g. "private-orders.42" -> only order 42's own owner may listen.
//!     channel == format!("private-orders.{}", user.id)
//! });
//!
//! // Route registration — shorthand for the pair of `.get(...)` calls this
//! // crate's own `ReverbPlugin` bundles:
//! // .plugin(larust_support::reverb::ReverbPlugin)
//!
//! // Wherever an order actually ships:
//! larust_support::reverb::broadcast_event(
//!     &format!("private-orders.{}", order.user_id),
//!     "OrderShipped",
//!     &order,
//! )?;
//! ```

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::Path;
use axum::http::header::CONTENT_TYPE;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use larust_core::AppError;
use larust_http::session::Session;
use serde::Serialize;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::{broadcast, OwnedSemaphorePermit, Semaphore};

/// See [`larust_live::push`]'s own `CHANNEL_CAPACITY` doc comment — same
/// "an occasional dropped update under a slow subscriber is an accepted
/// trade-off" reasoning applies here.
const CHANNEL_CAPACITY: usize = 32;
const MAX_CHANNELS: usize = 1_024;
const MAX_CONNECTIONS: usize = 1_024;
const MAX_CHANNEL_NAME_BYTES: usize = 128;

/// Channel names starting with this prefix require [`authorize`]'s
/// callback to allow the subscription; every other name is public.
/// Pusher/Laravel's own real convention, not invented here.
const PRIVATE_CHANNEL_PREFIX: &str = "private-";

type ChannelMap = HashMap<String, broadcast::Sender<String>>;

static CHANNELS: OnceLock<Mutex<ChannelMap>> = OnceLock::new();
static CONNECTIONS: OnceLock<Arc<Semaphore>> = OnceLock::new();

type AuthorizeFn =
    dyn Fn(Session, String) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync;

static AUTHORIZER: OnceLock<Box<AuthorizeFn>> = OnceLock::new();

/// Registers the callback deciding whether a `private-`-prefixed channel
/// may be subscribed to. Called with the current request's `Session` and
/// the full channel name (prefix included) at WebSocket-upgrade time,
/// before the connection is accepted — never called at all for a channel
/// that isn't `private-`-prefixed (public, unauthenticated by design).
///
/// No authorizer registered and a private channel requested is denied
/// (fail closed, not fail open). Call this once, at app boot — same
/// registration convention as `larust_support::wire::components()`; a
/// second call is ignored (logged as a warning) rather than silently
/// replacing the first, so a duplicate registration is visible instead of
/// quietly changing which callback governs authorization.
pub fn authorize<F, Fut>(callback: F)
where
    F: Fn(Session, String) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = bool> + Send + 'static,
{
    let boxed: Box<AuthorizeFn> =
        Box::new(move |session, channel| Box::pin(callback(session, channel)));
    if AUTHORIZER.set(boxed).is_err() {
        tracing::warn!(
            "larust_reverb::authorize() called more than once; ignoring the later registration"
        );
    }
}

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
            "reverb channel limit reached"
        );
        return None;
    }
    let sender = broadcast::channel(CHANNEL_CAPACITY).0;
    channels.insert(channel.to_string(), sender.clone());
    Some(sender)
}

/// Broadcasts `event_name`/`payload` — JSON-serialized as `{"event":
/// event_name, "data": payload}` — to every client currently subscribed
/// to `channel`. Call this from wherever server state actually changes
/// (an event listener, a controller action), not from inside the request
/// rendering the page itself.
///
/// A no-op, not an error, when nobody's listening or the channel name is
/// invalid — the same fire-and-forget tolerance `larust_live::push::
/// broadcast` already uses: ordinary pub/sub semantics, since nothing
/// about "zero receivers right now" is actually exceptional. The only
/// real error case is `payload` failing to serialize.
///
/// Not auto-wired to `larust-events::dispatch` — call it explicitly
/// wherever it belongs, the same way `push::broadcast` and `larust-events`
/// are bridged today: by hand, in app code, not a framework-level
/// "ShouldBroadcast" marker trait.
pub fn broadcast_event<E: Serialize>(
    channel: &str,
    event_name: &str,
    payload: &E,
) -> Result<(), AppError> {
    if !valid_channel_name(channel) {
        tracing::warn!(channel, "refusing broadcast to invalid reverb channel name");
        return Ok(());
    }
    let envelope = serde_json::json!({ "event": event_name, "data": payload });
    let serialized =
        serde_json::to_string(&envelope).map_err(|source| AppError::Internal(Box::new(source)))?;
    if let Some(sender) = sender_for(channel) {
        let _ = sender.send(serialized);
    }
    Ok(())
}

/// `GET /__larust_reverb/{channel}` — upgrades to a WebSocket and streams
/// every [`broadcast_event`] call's JSON envelope through, verbatim, for
/// the lifetime of the connection. Pure server → client push: no client →
/// server message is ever acted on (an inbound message is only read to
/// detect the browser tab closing the connection).
///
/// A `private-`-prefixed `channel` is checked against [`authorize`]'s
/// registered callback before the upgrade completes; anything else
/// subscribes unconditionally, same as `larust_live::push::socket` today.
pub async fn socket(
    Path(channel): Path<String>,
    session: Session,
    ws: WebSocketUpgrade,
) -> Response {
    if !valid_channel_name(&channel) {
        return (StatusCode::BAD_REQUEST, "invalid reverb channel").into_response();
    }

    if channel.starts_with(PRIVATE_CHANNEL_PREFIX) {
        let authorized = match AUTHORIZER.get() {
            Some(authorize) => authorize(session, channel.clone()).await,
            None => false,
        };
        if !authorized {
            return StatusCode::FORBIDDEN.into_response();
        }
    }

    let permits = CONNECTIONS
        .get_or_init(|| Arc::new(Semaphore::new(MAX_CONNECTIONS)))
        .clone();
    let Ok(permit) = permits.try_acquire_owned() else {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "reverb connection limit reached",
        )
            .into_response();
    };
    let Some(sender) = sender_for(&channel) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "reverb channel limit reached",
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
                    Ok(json) => {
                        if socket.send(Message::Text(json)).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            incoming = socket.recv() => {
                if incoming.is_none() {
                    break;
                }
            }
        }
    }
}

/// Same bounds/character-set rules as `larust_live::push`'s own
/// `valid_channel_name` — see its doc comment for the reasoning.
fn valid_channel_name(channel: &str) -> bool {
    !channel.is_empty()
        && channel.len() <= MAX_CHANNEL_NAME_BYTES
        && channel
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

const RUNTIME_JS: &str = include_str!("../assets/reverb-runtime.js");

/// `GET /__larust_reverb/runtime.js` — the vendored client runtime, served
/// from the installed crate itself rather than copied into `public/js/`,
/// same reasoning as `larust_live::push::runtime_js`.
pub async fn runtime_js() -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "text/javascript; charset=utf-8")],
        RUNTIME_JS,
    )
}

/// The two routes a Reverb-broadcasting app needs, bundled for
/// [`larust_http::Router::plugin`] — sugar for the `.get`/`.get` pair this
/// crate's own doc comment example shows an app writing by hand today.
/// Gated behind the `reverb` Cargo feature one layer up, in
/// `larust_support::reverb` — this crate itself has no feature flags of
/// its own, since it only ever compiles when that optional dependency is
/// pulled in.
pub struct ReverbPlugin;

impl larust_http::Plugin for ReverbPlugin {
    fn routes(&self) -> larust_http::Router {
        larust_http::Router::new()
            .get("/__larust_reverb/runtime.js", runtime_js)
            .get("/__larust_reverb/{channel}", socket)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reverb_plugin_contributes_exactly_the_two_routes_an_app_used_to_hand_write() {
        let routes = larust_http::Router::new().plugin(ReverbPlugin).routes();

        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0].method, "GET");
        assert_eq!(routes[0].path, "/__larust_reverb/runtime.js");
        assert_eq!(routes[1].method, "GET");
        assert_eq!(routes[1].path, "/__larust_reverb/{channel}");
    }

    #[test]
    fn channel_names_are_bounded_and_reject_control_characters() {
        assert!(valid_channel_name("orders.42_status"));
        assert!(valid_channel_name("private-orders.42"));
        assert!(!valid_channel_name(""));
        assert!(!valid_channel_name("orders/42"));
        assert!(!valid_channel_name("orders\n42"));
        assert!(!valid_channel_name(&"a".repeat(MAX_CHANNEL_NAME_BYTES + 1)));
    }

    #[tokio::test]
    async fn broadcast_with_no_subscribers_is_a_harmless_no_op() {
        broadcast_event("nobody-is-listening", "SomeEvent", &serde_json::json!({})).unwrap();
    }

    #[tokio::test]
    async fn a_subscriber_receives_the_event_envelope() {
        let mut rx = sender_for("reverb-lib-test-channel").unwrap().subscribe();
        broadcast_event(
            "reverb-lib-test-channel",
            "OrderShipped",
            &serde_json::json!({ "id": 42 }),
        )
        .unwrap();
        let received = rx.recv().await.unwrap();
        assert_eq!(received, r#"{"data":{"id":42},"event":"OrderShipped"}"#);
    }
}
