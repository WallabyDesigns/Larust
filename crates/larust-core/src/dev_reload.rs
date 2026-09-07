//! Live-reload signal for `xr dev` - the endpoint a page's injected script
//! (`larust_view::runtime::View`, and the dev-mode-only script
//! `error_pages` injects into the framework's own 404/500 fallback pages)
//! connects to. Three independent signals share this one endpoint:
//!
//! - A **full rebuild handoff** (a new generation actually took over) never
//!   sends a real event here at all - the running process itself gets
//!   replaced (see `lifecycle::handoff`), so the client's own
//!   `EventSource` connection drops and reconnects against the new
//!   process. The injected script watches *that* (lost, then successfully
//!   reconnected) as its "a new build is up" signal - nothing needs to
//!   originate from the server side for this case.
//! - A **static-asset-only change** (`public/`, e.g. a stylesheet) never
//!   needs a rebuild at all - `xr dev` skips straight to sending the admin
//!   channel's `RELOAD_ASSETS` command (see `lifecycle::admin`) to whatever
//!   is currently running, which calls [`broadcast_asset_reload`] here. That
//!   pushes a real, named `reload-assets` SSE event to every connected tab
//!   so it can refresh just its stylesheets in place, with no full page
//!   navigation and no server process restart at all.
//! - A **rebuild that doesn't (yet) replace the running process** - every
//!   rebuild after the very first one, for the whole time it's in flight,
//!   plus a rebuild that fails outright. The process this endpoint is
//!   already connected to is, by design, the *last known-good* one and
//!   keeps right on serving throughout (that's the zero-downtime guarantee
//!   `dev.rs`'s own doc comment describes) - so its SSE connection never
//!   drops, and a request that 404s because the route only exists in the
//!   code currently being compiled looks identical to a genuine mistake
//!   with no signal at all otherwise. `xr dev` sends the admin channel's
//!   `BUILD_STATUS` command (`building`/`failed`) at the start/end of every
//!   such rebuild, which calls [`broadcast_build_status`] here, pushing a
//!   named `build-status` SSE event a connected tab turns into a small,
//!   non-disruptive banner.

use axum::response::sse::{Event, KeepAlive, Sse};
use futures_core::Stream;
use std::convert::Infallible;
use std::sync::OnceLock;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

/// What a connected dev-reload client is told about, beyond the implicit
/// "connection dropped and reconnected" handoff signal (see this module's
/// own doc comment).
#[derive(Debug, Clone, PartialEq, Eq)]
enum DevReloadEvent {
    ReloadAssets,
    /// `status` is exactly `xr dev`'s own `BUILD_STATUS` payload word
    /// (`"building"`/`"failed"`) - passed through verbatim as the SSE
    /// event's `data`, not re-interpreted here; the client script is what
    /// gives each value meaning.
    BuildStatus(String),
}

impl DevReloadEvent {
    fn into_sse_event(self) -> Event {
        match self {
            DevReloadEvent::ReloadAssets => Event::default().event("reload-assets"),
            DevReloadEvent::BuildStatus(status) => {
                Event::default().event("build-status").data(status)
            }
        }
    }
}

/// Every connected dev-reload client subscribes to this. Sized generously
/// relative to how rarely a save actually happens - a lagged receiver just
/// drops the stale signal and waits for the next one (see `handler`'s own
/// `filter_map`), which is harmless: every event here is a status update
/// superseded by whatever comes next, never something that needs to be
/// replayed in full.
fn reload_channel() -> &'static broadcast::Sender<DevReloadEvent> {
    static CHANNEL: OnceLock<broadcast::Sender<DevReloadEvent>> = OnceLock::new();
    CHANNEL.get_or_init(|| broadcast::channel(16).0)
}

/// Called from the admin channel (`lifecycle::admin`) when it receives
/// `RELOAD_ASSETS` - i.e. `xr dev` decided a change didn't need a rebuild.
/// Best-effort and silent when nobody's listening: no browser tab currently
/// connected simply means there's nothing to refresh right now.
pub fn broadcast_asset_reload() {
    let _ = reload_channel().send(DevReloadEvent::ReloadAssets);
}

/// Called from the admin channel when it receives `BUILD_STATUS <status>` -
/// i.e. `xr dev` starting or finishing a rebuild that (so far) hasn't
/// replaced this process. `status` is forwarded to connected clients
/// verbatim as the `build-status` SSE event's data - see this module's own
/// doc comment for the two values `xr dev` actually sends
/// (`"building"`/`"failed"`). Best-effort and silent when nobody's
/// listening, same as [`broadcast_asset_reload`].
pub fn broadcast_build_status(status: &str) {
    let _ = reload_channel().send(DevReloadEvent::BuildStatus(status.to_string()));
}

/// A stream that never resolves on its own - `Sse`'s `KeepAlive` layer
/// keeps the connection alive on its own timer regardless of what the
/// underlying stream produces. Real items only ever arrive via
/// [`broadcast_asset_reload`]/[`broadcast_build_status`]; a lagged receiver
/// (fell behind by more than the channel's capacity) is treated the same
/// as "no event yet" rather than propagated as a stream error, since a
/// missed intermediate status update is never worth tearing down the
/// connection over.
pub async fn handler() -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = BroadcastStream::new(reload_channel().subscribe())
        .filter_map(|result| result.ok().map(|event| Ok(event.into_sse_event())));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn broadcast_asset_reload_delivers_to_an_already_subscribed_receiver() {
        let mut receiver = reload_channel().subscribe();
        broadcast_asset_reload();
        assert_eq!(receiver.recv().await, Ok(DevReloadEvent::ReloadAssets));
    }

    #[tokio::test]
    async fn broadcast_build_status_delivers_the_status_verbatim() {
        let mut receiver = reload_channel().subscribe();
        broadcast_build_status("building");
        assert_eq!(
            receiver.recv().await,
            Ok(DevReloadEvent::BuildStatus("building".to_string()))
        );
    }
}
