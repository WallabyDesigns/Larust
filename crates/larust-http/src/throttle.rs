//! Laravel's `throttle` middleware — a fixed-window request-rate limit,
//! keyed by the caller's real TCP peer address (`ConnectInfo<SocketAddr>`,
//! see `larust_core::Application::serve()`'s `into_make_service_with_connect_info`
//! wiring), never a client-supplied header like `X-Forwarded-For` — this
//! framework has no "trusted proxy" concept to validate such a header
//! against, so trusting it would make the limiter trivially bypassable
//! (a different value per request) while looking like it works.
//!
//! Rejects with `429 Too Many Requests` plus a `Retry-After` header once a
//! key's bucket is exhausted, matching Laravel's own `ThrottleRequests`
//! response shape. Hand-rolled rather than built on `tower::limit`
//! (present in this workspace's dependency tree only via `tower`'s default
//! features; its `RateLimit` service isn't `Clone`, failing `Router::
//! middleware()`'s bound without extra wrapping, and it has no per-key
//! bucketing at all) or a new dependency like `governor` — this mirrors
//! `csrf.rs`'s own hand-written `axum::middleware::from_fn`-style shape
//! instead, the established house style for this kind of thing.

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{from_fn_with_state, FromFnLayer, Next};
use axum::response::{IntoResponse, Response};
use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Caps how many distinct keys (source IPs, plus the one shared fallback
/// below) this process tracks at once, mirroring `larust_live::lock`'s
/// identical `MAX_SESSION_LOCKS` cap and reasoning: without a bound, a
/// flood of requests from many distinct source addresses could grow this
/// table without limit. Unlike that module, a new key arriving at the cap
/// **fails open** here (allowed, just not tracked) rather than evicting
/// the oldest entry — this cap protects this process's own memory, it
/// isn't meant to be a rejection mechanism in its own right, and a
/// genuinely abusive client keeps reusing the same key regardless, so its
/// own bucket still catches it.
const MAX_TRACKED_KEYS: usize = 10_000;

/// The shared bucket key for any request with no `ConnectInfo` at all.
/// Every request driven through `larust_testing::TestClient` falls into
/// this case — it dispatches via `tower::ServiceExt::oneshot`, never a
/// real accepted TCP connection, so `ConnectInfo` is never populated.
/// Sharing one bucket keeps existing and future tests from needing to
/// know anything about rate limiting to pass, at the cost of tests
/// sharing a limit with each other if a single test fires more requests
/// than the configured limit within one window — not the case for any
/// test in this codebase today.
const NO_CONNECT_INFO_KEY: &str = "__no_connect_info__";

struct Bucket {
    count: u32,
    window_start: Instant,
}

/// `pub` only because it has to appear in [`per`]/[`per_minute`]'s public
/// return type (see that doc comment) — every field stays private, so
/// nothing outside this module can construct or inspect one.
pub struct ThrottleState {
    max_requests: u32,
    window: Duration,
    buckets: Mutex<HashMap<String, Bucket>>,
}

impl ThrottleState {
    /// `true` if a request keyed `key` may proceed, recording it against
    /// that key's bucket either way. A fixed-window counter (matches
    /// Laravel's own `RateLimiter` algorithm, not token-bucket/sliding-
    /// window) — a key's window simply resets, rather than aligning to a
    /// global clock, once `window` has elapsed since it was first seen.
    fn allow(&self, key: &str) -> bool {
        let mut buckets = self
            .buckets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Instant::now();
        // Opportunistic sweep, same as `larust_live::lock` — a bucket
        // whose window has fully elapsed is equivalent to a reset count
        // of zero, so removing it here and re-inserting fresh below (if
        // this request needs to) is exactly a window reset.
        buckets.retain(|_, bucket| now.duration_since(bucket.window_start) < self.window);

        if let Some(bucket) = buckets.get_mut(key) {
            if bucket.count >= self.max_requests {
                return false;
            }
            bucket.count += 1;
            return true;
        }

        if buckets.len() < MAX_TRACKED_KEYS {
            buckets.insert(
                key.to_string(),
                Bucket {
                    count: 1,
                    window_start: now,
                },
            );
        }
        true
    }
}

/// `middleware`'s own extractor arguments (every one before `Next`), as a
/// tuple — `axum::middleware::from_fn_with_state`'s hidden extractor-arity
/// marker parameter, spelled out explicitly (see [`per`]'s own doc comment
/// for why it has to be).
type Extractors = (
    State<Arc<ThrottleState>>,
    Option<ConnectInfo<SocketAddr>>,
    Request,
);

/// A plain `fn` pointer, not the anonymous type of an `async fn` item —
/// see [`per`]'s own doc comment for why `middleware` has to be written to
/// coerce to this rather than left as an `async fn`.
type MiddlewareFn = fn(
    State<Arc<ThrottleState>>,
    Option<ConnectInfo<SocketAddr>>,
    Request,
    Next,
) -> MiddlewareFuture;
type MiddlewareFuture = Pin<Box<dyn Future<Output = Response> + Send>>;

/// `max_requests` per rolling minute — Laravel's own common default
/// (`throttle:60,1`) when a route doesn't specify its own limit.
pub fn per_minute(max_requests: u32) -> FromFnLayer<MiddlewareFn, Arc<ThrottleState>, Extractors> {
    per(max_requests, Duration::from_secs(60))
}

/// `max_requests` per `window` — the general form behind [`per_minute`].
/// Usable via `Router::middleware(...)` the same way `csrf::verify`/
/// `DefaultBodyLimit::max(...)` already are.
///
/// Returns the fully concrete `FromFnLayer<...>` type rather than `impl
/// tower::Layer<axum::routing::Route> + Clone + Send + 'static` — tempting
/// since that's exactly what `Router::middleware()` requires, but an
/// opaque `impl Trait` return only carries the bounds spelled out on it,
/// not the *other* facts a caller in a different crate needs (that
/// `L::Service` is itself `Clone + Send + tower::Service<Request>`,
/// `Router::middleware()`'s real bound on `L::Service`) — those get
/// erased at the opaque-type boundary, so `demo`/`examples/blog`'s own
/// `.middleware(throttle::per_minute(60))` call failed to type-check
/// against `impl Trait` even though this exact value works fine passed
/// directly. A concrete, nameable return type has no such boundary; every
/// real trait impl on it stays visible everywhere. Same reason `Router::
/// middleware()`'s own doc comment says to call `axum::middleware::
/// from_fn(handler)` directly at the `.middleware(...)` call site instead
/// of through a wrapping function — this file does the equivalent by
/// keeping the concrete type nameable instead.
pub fn per(
    max_requests: u32,
    window: Duration,
) -> FromFnLayer<MiddlewareFn, Arc<ThrottleState>, Extractors> {
    let state = Arc::new(ThrottleState {
        max_requests,
        window,
        buckets: Mutex::new(HashMap::new()),
    });
    from_fn_with_state(state, middleware as MiddlewareFn)
}

/// A plain `fn`, not `async fn` — `async fn`'s return type is anonymous
/// and can't be named, which [`MiddlewareFn`] (and so [`per`]'s own
/// concrete return type) needs it to be; boxing the future by hand is what
/// makes that possible.
fn middleware(
    State(state): State<Arc<ThrottleState>>,
    connect_info: Option<ConnectInfo<SocketAddr>>,
    request: Request,
    next: Next,
) -> MiddlewareFuture {
    Box::pin(async move {
        let key = connect_info
            .map(|ConnectInfo(addr)| addr.ip().to_string())
            .unwrap_or_else(|| NO_CONNECT_INFO_KEY.to_string());

        if !state.allow(&key) {
            return reject(state.window);
        }

        next.run(request).await
    })
}

fn reject(window: Duration) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, window.as_secs().to_string())],
        "Too Many Requests",
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(max_requests: u32, window: Duration) -> ThrottleState {
        ThrottleState {
            max_requests,
            window,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    #[test]
    fn allows_up_to_the_limit_then_rejects_the_next_request_in_the_same_window() {
        let state = state(3, Duration::from_secs(60));
        assert!(state.allow("a"));
        assert!(state.allow("a"));
        assert!(state.allow("a"));
        assert!(!state.allow("a"));
    }

    #[test]
    fn a_fresh_window_resets_the_count() {
        let state = state(1, Duration::from_millis(50));
        assert!(state.allow("a"));
        assert!(!state.allow("a"));
        std::thread::sleep(Duration::from_millis(80));
        assert!(state.allow("a"));
    }

    #[test]
    fn different_keys_have_independent_buckets() {
        let state = state(1, Duration::from_secs(60));
        assert!(state.allow("a"));
        assert!(!state.allow("a"));
        assert!(state.allow("b"));
    }

    #[test]
    fn a_new_key_at_capacity_fails_open_rather_than_panicking_or_rejecting() {
        let state = state(1, Duration::from_secs(60));
        for i in 0..MAX_TRACKED_KEYS {
            assert!(state.allow(&i.to_string()));
        }
        assert!(state.allow("one-more-past-capacity"));
    }
}
