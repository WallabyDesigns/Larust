//! `Route`/`Router` DSL over axum, plus middleware, sessions, and CSRF.
//! Route model binding lands in a later milestone.

pub mod csrf;
mod path;
pub mod preferences;
mod random;
mod request;
pub mod responsecache;
mod route;
pub mod session;
pub mod throttle;

pub use random::random_hex;
pub use request::Request;
pub use route::{resolve_route_name, Route, RouteInfo, Router};

pub use axum;
