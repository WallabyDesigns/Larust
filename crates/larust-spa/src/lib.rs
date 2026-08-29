//! `@spa`'s client runtime — hotloaded, SPA-style page navigation (History
//! API fetch-and-swap instead of a full reload). See `docs/MACROS.md`'s
//! `@spa` section for the full directive design; this crate is
//! deliberately tiny, and deliberately its own crate rather than folded
//! into `larust-live`.
//!
//! **Why its own crate, not `larust-live`**: `@spa` shares no machinery at
//! all with `larust-live`'s reactive-component (`@wire`) or server-push
//! (`@live`) features — no component registry, no session-backed mount, no
//! WebSocket. Every request `@spa` navigates to is rendered exactly the
//! same full HTML page `view!(...)` already produces for a hard reload;
//! the client extracts what changed via `DOMParser` and swaps it in. There
//! is no server-side rendering path for this feature at all — this crate
//! is, in full, one vendored static JS asset and the handler that serves
//! it, the same "thin sibling crate" shape `larust-reverb` already
//! establishes for a broadcast/socket variant of `larust-live::push` that
//! likewise didn't belong folded into that crate's own identity.
//!
//! **No feature flag** — re-exported unconditionally through
//! `larust_support::spa`, unlike the optional Tier-1 shim crates
//! (`permissions`/`reverb`/`sanctum`/`sitemap`/`socialite`, each a
//! Laravel-third-party-package equivalent an app opts into via a Cargo
//! feature). `@spa` is core template-directive surface, the same tier as
//! `wire`/`push`.
//!
//! **Route registration is the app's own job**, not automatic — same
//! explicit convention `/__larust_wire/*`/`/__larust_push/*` already use
//! (see `demo/routes/web.rs`). An app that uses `@spa ... @endspa`
//! anywhere registers exactly one route:
//!
//! ```ignore
//! .get("/__larust_spa/runtime.js", larust_support::spa::runtime_js)
//! ```
//!
//! Unlike `wire`/`push`, there is no second route — no `/__larust_spa/{id}`
//! endpoint of any kind — because there is no server-side counterpart to
//! this feature at all; ordinary page routes already serve everything
//! `@spa` needs.

use axum::http::header::CONTENT_TYPE;
use axum::response::IntoResponse;

const RUNTIME_JS: &str = include_str!("../assets/spa-runtime.js");

/// Serves the vendored `spa-runtime.js` client script — see this crate's
/// own doc comment for the route the app must register this under.
pub async fn runtime_js() -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "text/javascript; charset=utf-8")],
        RUNTIME_JS,
    )
}
