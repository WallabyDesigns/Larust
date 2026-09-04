//! A single cache, keyed by string, values serialized as JSON - backed by
//! SQL-family storage (`Config::cache_driver == "database"`, the default)
//! or Redis (`"redis"`), chosen at each call by [`store`]'s own dispatch.
//! See [`sql_store`]/[`redis_store`] for the two implementations, and
//! `store.rs`'s own doc comment for the dispatch shape.
//!
//! Deliberately ships no in-memory driver - mirroring
//! `larust_http::session`'s own "no in-memory option in the public API"
//! stance (an in-memory cache is the same trap: it "works" in every manual
//! test, then silently starts missing on every deploy/restart). Laravel's
//! own default (`CACHE_STORE=database` since Laravel 11) points the same
//! direction - the `"redis"` option here mirrors Laravel's other common
//! production choice, not an in-memory one.

mod redis_store;
mod sql_store;
mod store;

pub use store::{forget, get, put, remember};
