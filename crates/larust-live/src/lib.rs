//! Server-state-backed reactive components - Larust's Livewire equivalent.
//! Unlike Livewire's client-held, HMAC-signed state snapshot (a workaround
//! for PHP/Laravel's stateless-between-requests model), a component's state
//! lives entirely server-side, keyed by the user's session: only an opaque
//! component id ever crosses the wire. See `docs/ARCHITECTURE.md`'s
//! "Reactive components" section for the full design rationale.
//!
//! The crate itself keeps its original name (`larust-live`) - only the
//! user-facing directive/trait/route surface is `@wire`/`WireComponent`/
//! `/__larust_wire/...`.
//!
//! App authors implement [`WireComponent`], register instances via
//! [`components`]/[`LiveRegistry`], mount them from a template with
//! `@wire('name', { prop: expr, ... })`, and wire the two routes this crate
//! provides ([`update`], [`runtime_js`]) into their app's router - nothing
//! here is auto-mounted, matching this framework's "explicit over magic"
//! convention for routes/middleware/listeners elsewhere.
//!
//! [`push`] is a separate, unrelated feature living in the same crate:
//! genuine server-*pushed* updates (`@live('channel') ... @endlive`,
//! WebSocket-based) rather than `@wire`'s client-*initiated* AJAX sync -
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

/// The two routes `@wire(...)` needs, bundled for [`larust_http::Router::plugin`] -
/// sugar for the exact `.get`/`.post` pair this module's own doc comment
/// shows an app writing by hand today. Registration itself is still the
/// app's explicit choice (`.plugin(WirePlugin)` in `routes/web.rs`), this
/// only removes the need to know or copy the two literal route strings.
pub struct WirePlugin;

impl larust_http::Plugin for WirePlugin {
    fn routes(&self) -> larust_http::Router {
        larust_http::Router::new()
            .get("/__larust_wire/runtime.js", routes::runtime_js)
            .post("/__larust_wire/{component_id}", routes::update)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_plugin_contributes_exactly_the_two_routes_an_app_used_to_hand_write() {
        let routes = larust_http::Router::new().plugin(WirePlugin).routes();

        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0].method, "GET");
        assert_eq!(routes[0].path, "/__larust_wire/runtime.js");
        assert_eq!(routes[1].method, "POST");
        assert_eq!(routes[1].path, "/__larust_wire/{component_id}");
    }
}
