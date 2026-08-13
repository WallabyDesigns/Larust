//! Server-state-backed reactive components — Larust's Livewire equivalent.
//! Unlike Livewire's client-held, HMAC-signed state snapshot (a workaround
//! for PHP/Laravel's stateless-between-requests model), a component's state
//! lives entirely server-side, keyed by the user's session: only an opaque
//! component id ever crosses the wire. See `docs/ARCHITECTURE.md`'s
//! "Reactive components" section for the full design rationale.
//!
//! The crate itself keeps its original name (`larust-live`) — only the
//! user-facing directive/trait/route surface is `@wire`/`WireComponent`/
//! `/__larust_wire/...`.
//!
//! App authors implement [`WireComponent`], register instances via
//! [`components`]/[`LiveRegistry`], mount them from a template with
//! `@wire('name', { prop: expr, ... })`, and wire the two routes this crate
//! provides ([`update`], [`runtime_js`]) into their app's router — nothing
//! here is auto-mounted, matching this framework's "explicit over magic"
//! convention for routes/middleware/listeners elsewhere.
//!
//! [`push`] is a separate, unrelated feature living in the same crate:
//! genuine server-*pushed* updates (`@live('channel') ... @endlive`,
//! WebSocket-based) rather than `@wire`'s client-*initiated* AJAX sync —
//! see that module's own doc comment for why these two are deliberately
//! not the same mechanism.

mod component;
mod lock;
mod mount;
pub mod push;
mod registry;
mod routes;
mod state;

pub use component::WireComponent;
pub use mount::mount;
pub use registry::{components, LiveRegistry};
pub use routes::{runtime_js, update, UpdatePayload};
