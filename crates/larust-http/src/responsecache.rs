//! Laravel's `spatie/laravel-responsecache` - narrowed to its core shape,
//! same "names/assignment split" reasoning `larust-permissions`'s own doc
//! comment used for its own narrowing (see `crates/larust-permissions/src/
//! lib.rs`): this caches `GET` responses with a `200` status, keyed by URL,
//! backed by `larust-cache` (which already owns its own table bootstrap and
//! expiry sweep - this module adds no new table, just a value shape to
//! store in the existing one).
//!
//! Opt-in per router/route via `.middleware(...)`, the same as
//! [`crate::throttle`] - never part of `larust_core::Application::serve`'s
//! own default middleware stack.
//!
//! ## Deliberately out of scope for this version
//!
//! - **No `Accept`/content-negotiation `Vary`.** This cache is purely
//!   server-side (backed by `larust_cache`, never a browser or proxy
//!   cache), so there's no real HTTP `Vary` semantics to implement - only
//!   the cache *key* needs to account for whatever the response actually
//!   depends on. Two requests to the same URL differing only in `Accept`
//!   or another header still collide (same cached entry either way); if a
//!   route genuinely content-negotiates, don't cache it, the same guidance
//!   as always. Per-session variance (the "auth state" half of the
//!   original limitation here) is now handled - see [`for_minutes_per_session`].
//! - **No auto-invalidation on writes.** Laravel's package can auto-clear
//!   on any non-`GET` request; this crate only offers [`forget`] (a single
//!   URL) and TTL expiry - an app that needs eager invalidation calls
//!   `forget` itself wherever it mutates the underlying data.
//! - **No bulk "clear everything."** `larust_cache` has no key-prefix scan
//!   API to build one on top of - a real follow-up if ever needed, not a
//!   gap worth solving speculatively here.
//! - **Only `Content-Type` survives a cache hit.** Every other response
//!   header (`Set-Cookie`, custom headers, ...) is dropped on a cached
//!   replay - for a `Set-Cookie` specifically, that's the *correct*
//!   behavior (never replay a stale session cookie from whoever's request
//!   happened to populate the cache), but a route relying on some other
//!   custom header surviving a cache hit needs to know this doesn't happen.

use crate::session::Session;
use axum::body::{to_bytes, Body};
use axum::extract::{Request, State};
use axum::http::{header, Method, StatusCode};
use axum::middleware::{from_fn_with_state, FromFnLayer, Next};
use axum::response::{IntoResponse, Response};
use larust_core::AppError;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

/// Bound on how large a response body actually gets *cached* - not a limit
/// on reading the body at all, since that has to happen regardless (see
/// [`middleware`]'s own doc comment). Mirrors axum's own `DefaultBodyLimit`
/// default. A response over this size is still served correctly; it's just
/// never written to the cache, so this middleware can't become a memory
/// blow-up vector if applied to a route that (against its own doc advice)
/// serves something large.
const MAX_CACHEABLE_BODY_BYTES: usize = 2 * 1024 * 1024;

/// `pub` only because it has to appear in [`for_minutes`]/[`for_duration`]'s
/// public return type - every field stays private, matching
/// `crate::throttle::ThrottleState`'s own reasoning.
pub struct ResponseCacheState {
    ttl: Duration,
}

#[derive(Serialize, Deserialize)]
struct CachedResponse {
    content_type: Option<String>,
    body: Vec<u8>,
}

impl CachedResponse {
    /// Infallible in practice - a `200` status plus a plain header/body
    /// pair can't fail to build - but `.expect()` rather than an unchecked
    /// unwrap-adjacent path, so a genuine bug here surfaces loudly instead
    /// of silently producing a broken response.
    fn into_response(self) -> Response {
        let mut builder = Response::builder().status(StatusCode::OK);
        if let Some(content_type) = &self.content_type {
            builder = builder.header(header::CONTENT_TYPE, content_type);
        }
        builder
            .body(Body::from(self.body))
            .expect("a cached response with a plain header/body pair always builds")
    }
}

fn cache_key_for_url(url: &str) -> String {
    format!("__larust_responsecache:{url}")
}

/// Manually evicts one URL's cached entry - Laravel's own
/// `ResponseCache::forget($url)`, narrowed to a single URL (see this
/// module's own doc comment for why there's no bulk clear).
pub async fn forget(url: &str) -> Result<(), AppError> {
    larust_cache::forget(&cache_key_for_url(url)).await
}

/// `middleware`'s own extractor arguments (every one before `Next`), as a
/// tuple - see `crate::throttle::Extractors`'s own doc comment for why
/// `from_fn_with_state` needs this spelled out explicitly.
type Extractors = (State<Arc<ResponseCacheState>>, Request);
type PerSessionExtractors = (State<Arc<ResponseCacheState>>, Session, Request);

/// A plain `fn` pointer, not the anonymous type of an `async fn` item -
/// see `crate::throttle::MiddlewareFn`'s own doc comment for why.
type MiddlewareFn = fn(State<Arc<ResponseCacheState>>, Request, Next) -> MiddlewareFuture;
type PerSessionMiddlewareFn =
    fn(State<Arc<ResponseCacheState>>, Session, Request, Next) -> MiddlewareFuture;
type MiddlewareFuture = Pin<Box<dyn Future<Output = Response> + Send>>;

/// Caches for `minutes` minutes, keyed by URL alone - see this module's own
/// doc comment for why that means every viewer of a given URL shares one
/// cached response. Use [`for_minutes_per_session`] instead for a route
/// whose response depends on who's asking.
pub fn for_minutes(minutes: u64) -> FromFnLayer<MiddlewareFn, Arc<ResponseCacheState>, Extractors> {
    for_duration(Duration::from_secs(minutes * 60))
}

/// The general form behind [`for_minutes`]. Returns the fully concrete
/// `FromFnLayer<...>` type rather than an opaque `impl Trait` - same
/// reasoning as `crate::throttle::per`'s own doc comment: an app in a
/// different crate calling `.middleware(responsecache::for_minutes(5))`
/// needs every real trait bound on the returned value visible, which an
/// opaque return type would erase.
pub fn for_duration(
    ttl: Duration,
) -> FromFnLayer<MiddlewareFn, Arc<ResponseCacheState>, Extractors> {
    from_fn_with_state(
        Arc::new(ResponseCacheState { ttl }),
        middleware as MiddlewareFn,
    )
}

/// Caches for `minutes` minutes, keyed by URL **and session** - a cache hit
/// for one visitor is never served to another. This is the fix for the
/// "no per-user caching" half of this module's original limitation: the
/// cache key incorporates the session id (from `Session::id()`, the same
/// cookie-backed identity `require_auth`/CSRF already key off), not a raw
/// `Cookie` header or full HTTP `Vary` semantics - this cache has no
/// browser/proxy audience to announce a `Vary` header to, so only the key
/// itself needs to account for the viewer. Requires `.with_sessions(...)`
/// to already be enabled on this router (same requirement CSRF/`Session`
/// extraction anywhere else already has) - `Session` is one of this
/// method's own extractors, so applying it to a router with no session
/// layer fails to compile the same way any other `Session`-extracting
/// handler would.
///
/// Trades a lower hit rate for correctness: every distinct session gets its
/// own cache entry, so this is worth it for a page that's expensive to
/// render but genuinely per-viewer (a dashboard), not for content that's
/// identical for everyone (use [`for_minutes`] for that - a shared cache
/// entry serving every visitor is the whole point there).
///
/// **A session's very first-ever request is never cached** - see
/// `middleware_per_session`'s own doc comment for why this is a hard
/// `tower_sessions` architectural constraint (session ids are assigned
/// lazily, only by the outer session layer's own post-processing, never
/// visible to this middleware in time) rather than something this crate
/// could reasonably work around. From that session's second request
/// onward (once its cookie is being sent back), caching works normally.
pub fn for_minutes_per_session(
    minutes: u64,
) -> FromFnLayer<PerSessionMiddlewareFn, Arc<ResponseCacheState>, PerSessionExtractors> {
    for_duration_per_session(Duration::from_secs(minutes * 60))
}

/// The general form behind [`for_minutes_per_session`].
pub fn for_duration_per_session(
    ttl: Duration,
) -> FromFnLayer<PerSessionMiddlewareFn, Arc<ResponseCacheState>, PerSessionExtractors> {
    from_fn_with_state(
        Arc::new(ResponseCacheState { ttl }),
        middleware_per_session as PerSessionMiddlewareFn,
    )
}

/// Only `GET` requests are ever looked up or stored - a `POST`/`PUT`/etc.
/// bypasses this middleware entirely, running the handler and returning its
/// response completely untouched (no buffering, no caching).
///
/// A `GET` response has to be fully read into memory either way to relay it
/// to the client - that's not a cost this middleware introduces, it's the
/// same cost already paid constructing any in-memory `View`/`Json`
/// response. So the body is always buffered for a `200 GET` response;
/// [`MAX_CACHEABLE_BODY_BYTES`] only bounds whether that buffered body then
/// gets *written to the cache*, not whether it gets read at all.
fn middleware(
    State(state): State<Arc<ResponseCacheState>>,
    request: Request,
    next: Next,
) -> MiddlewareFuture {
    Box::pin(async move {
        if request.method() != Method::GET {
            return next.run(request).await;
        }
        let key = cache_key_for_url(&request.uri().to_string());
        if let Some(hit) = lookup(&key).await {
            return hit;
        }
        let response = next.run(request).await;
        store_and_respond(&state, &key, response).await
    })
}

/// Same shape as [`middleware`], but the cache key also incorporates the
/// current session's id - see [`for_minutes_per_session`]'s own doc
/// comment for the full rationale.
///
/// **A session's very first request (before it has a session cookie at
/// all) is never cached, even for its own later requests.** This isn't a
/// missed optimization - it's a hard constraint of `tower_sessions`'
/// own architecture, confirmed by reading `Session::save`'s source
/// directly: a brand-new session's id is only ever assigned inside
/// `save()`, which the *outer* `SessionManagerLayer` calls in its own
/// post-processing, strictly after the entire inner service chain
/// (including this middleware and the handler it wraps) has already
/// returned its response. There is no point during this function's own
/// execution - not before `next.run()`, not after - where a genuinely
/// new session's id exists yet to key a cache entry on. Once a session
/// has made one real round trip (so the client is sending its cookie
/// back), `session.id()` is populated from the start of every later
/// request, and caching works normally from then on.
fn middleware_per_session(
    State(state): State<Arc<ResponseCacheState>>,
    session: Session,
    request: Request,
    next: Next,
) -> MiddlewareFuture {
    Box::pin(async move {
        if request.method() != Method::GET {
            return next.run(request).await;
        }
        let Some(session_id) = session.id() else {
            return next.run(request).await;
        };
        let key = format!(
            "{}:{session_id}",
            cache_key_for_url(&request.uri().to_string())
        );
        if let Some(hit) = lookup(&key).await {
            return hit;
        }
        let response = next.run(request).await;
        store_and_respond(&state, &key, response).await
    })
}

/// Cache lookup shared by [`middleware`]/[`middleware_per_session`] - a
/// store failure or miss both mean "nothing usable," collapsed into
/// `None` either way; only a real hit short-circuits the caller.
async fn lookup(key: &str) -> Option<Response> {
    match larust_cache::get::<CachedResponse>(key).await {
        Ok(Some(cached)) => Some(cached.into_response()),
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(%error, "responsecache lookup failed, serving live");
            None
        }
    }
}

/// Buffers `response`'s body (required either way, to relay it to the
/// client - see [`middleware`]'s own doc comment), writes it to the cache
/// under `key` if it qualifies (`200`, under [`MAX_CACHEABLE_BODY_BYTES`]),
/// and returns the rebuilt response.
async fn store_and_respond(state: &ResponseCacheState, key: &str, response: Response) -> Response {
    if response.status() != StatusCode::OK {
        return response;
    }

    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let (parts, body) = response.into_parts();
    let bytes = match to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::error!(%error, "failed to read response body for caching");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "response body read failed",
            )
                .into_response();
        }
    };

    if bytes.len() <= MAX_CACHEABLE_BODY_BYTES {
        let cached = CachedResponse {
            content_type,
            body: bytes.to_vec(),
        };
        if let Err(error) = larust_cache::put(key, &cached, state.ttl).await {
            tracing::warn!(%error, "responsecache store failed");
        }
    }

    Response::from_parts(parts, Body::from(bytes))
}
