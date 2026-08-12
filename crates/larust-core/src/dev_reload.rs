//! Live-reload signal for `xr dev` — the endpoint a page's injected script
//! (`larust_view::runtime::View`) connects to. Only ever wired into the
//! router when the `LARUST_DEV_RELOAD` env var is set, which `xr dev` sets
//! only on the child process it spawns itself — a plain `cargo run` never
//! sees this route at all, so it costs nothing outside the dev loop.
//!
//! The endpoint never sends a real event. Its only job is to exist so a
//! client connecting to it *succeeds* — the injected script watches its own
//! connection state (lost, then successfully reconnected) as the "a new
//! build is up" signal, so no message needs to originate from here at all.

use axum::response::sse::{Event, KeepAlive, Sse};
use futures_core::Stream;
use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};

/// A stream that never resolves. `Sse`'s `KeepAlive` layer keeps the
/// connection alive on its own timer regardless of what the underlying
/// stream ever produces, so this doesn't need to yield anything.
struct Never;

impl Stream for Never {
    type Item = Result<Event, Infallible>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}

pub async fn handler() -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(Never).keep_alive(KeepAlive::default())
}
