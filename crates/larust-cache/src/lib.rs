//! A single SQLite-backed cache, keyed by string, values serialized as JSON.
//!
//! Deliberately ships no in-memory driver and no driver toggle — mirroring
//! `larust_http::session`'s own "no in-memory option in the public API"
//! stance (an in-memory cache is the same trap: it "works" in every manual
//! test, then silently starts missing on every deploy/restart). Laravel's
//! own default (`CACHE_STORE=database` since Laravel 11) points the same
//! direction. See `store.rs` for the implementation.

mod store;

pub use store::{forget, get, put, remember};
