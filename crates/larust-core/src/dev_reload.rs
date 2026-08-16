//! Live-reload signal for `xr dev` — the endpoint a page's injected script
//! (`larust_view::runtime::View`) connects to. Only ever wired into the
//! router when the `LARUST_DEV_RELOAD` env var is set, which `xr dev` sets
//! only on the child process it spawns itself — a plain `cargo run` never
//! sees this route at all, so it costs nothing outside the dev loop.
//!
//! Two independent reload paths share this one endpoint:
//!
//! - A **full rebuild** (source changed) never sends a real event here at
//!   all — the running process itself gets replaced by a real build/handoff
//!   (see `lifecycle::handoff`), so the client's own `EventSource`
//!   connection drops and reconnects against the new process. The injected
//!   script watches *that* (lost, then successfully reconnected) as its "a
//!   new build is up" signal — nothing needs to originate from the server
//!   side for this case.
//! - A **static-asset-only change** (`public/`, e.g. a stylesheet) never
//!   needs a rebuild at all — `xr dev` skips straight to sending the admin
//!   channel's `RELOAD_ASSETS` command (see `lifecycle::admin`) to whatever
//!   is currently running, which calls [`broadcast_asset_reload`] here. That
//!   pushes a real, named `reload-assets` SSE event to every connected tab
//!   so it can refresh just its stylesheets in place, with no full page
//!   navigation and no server process restart at all.

use axum::response::sse::{Event, KeepAlive, Sse};
use futures_core::Stream;
use std::convert::Infallible;
use std::sync::OnceLock;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

/// Every connected dev-reload client subscribes to this. Sized generously
/// relative to how rarely a save actually happens — a lagged receiver just
/// drops the stale signal and waits for the next one (see `handler`'s own
/// `filter_map`), which is harmless: the whole point of the message is
/// "refresh", and a later refresh already implies the earlier one.
fn reload_channel() -> &'static broadcast::Sender<()> {
    static CHANNEL: OnceLock<broadcast::Sender<()>> = OnceLock::new();
    CHANNEL.get_or_init(|| broadcast::channel(16).0)
}

/// Called from the admin channel (`lifecycle::admin`) when it receives
/// `RELOAD_ASSETS` — i.e. `xr dev` decided a change didn't need a rebuild.
/// Best-effort and silent when nobody's listening: no browser tab currently
/// connected simply means there's nothing to refresh right now.
pub fn broadcast_asset_reload() {
    let _ = reload_channel().send(());
}

/// A stream that never resolves on its own — `Sse`'s `KeepAlive` layer
/// keeps the connection alive on its own timer regardless of what the
/// underlying stream produces. Real items only ever arrive via
/// [`broadcast_asset_reload`]; a lagged receiver (fell behind by more than
/// the channel's capacity) is treated the same as "no event yet" rather
/// than propagated as a stream error, since a missed intermediate refresh
/// is never worth tearing down the connection over.
pub async fn handler() -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = BroadcastStream::new(reload_channel().subscribe()).filter_map(|result| {
        result
            .ok()
            .map(|()| Ok(Event::default().event("reload-assets")))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn broadcast_asset_reload_delivers_to_an_already_subscribed_receiver() {
        let mut receiver = reload_channel().subscribe();
        broadcast_asset_reload();
        assert_eq!(receiver.recv().await, Ok(()));
    }
}
