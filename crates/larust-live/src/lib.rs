//! Server-state-backed reactive components — Larust's Livewire equivalent.
//! Unlike Livewire's client-held, HMAC-signed state snapshot (a workaround
//! for PHP/Laravel's stateless-between-requests model), a component's state
//! lives entirely server-side, keyed by the user's session: only an opaque
//! component id ever crosses the wire. See `docs/ARCHITECTURE.md`'s
//! "Reactive components" section for the full design rationale.
//!
//! App authors implement [`LiveComponent`], register instances via
//! [`components`]/[`LiveRegistry`], mount them from a template with
//! `@live('name', { prop: expr, ... })`, and wire the two routes this crate
//! provides ([`update`], [`runtime_js`]) into their app's router — nothing
//! here is auto-mounted, matching this framework's "explicit over magic"
//! convention for routes/middleware/listeners elsewhere.

mod component;
mod lock;
mod mount;
mod registry;
mod routes;
mod state;

pub use component::LiveComponent;
pub use mount::mount;
pub use registry::{components, LiveRegistry};
pub use routes::{runtime_js, update, UpdatePayload};
