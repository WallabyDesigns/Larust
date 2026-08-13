use axum::http::StatusCode;
use larust_core::AppError;
use larust_http::session::Session;
use larust_view::View;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::HashMap;
use std::future::Future;

/// A server-state-backed reactive component — Larust's Livewire-equivalent.
/// Unlike Livewire, `Self`'s state never crosses the wire: it lives
/// server-side, keyed by the user's session (see `larust-live::state`), and
/// only an opaque component id is ever sent to/from the browser.
///
/// Implementors are typically small `#[derive(Serialize, Deserialize)]`
/// structs holding just the fields a `wire:model`/`wire:model.live` field
/// binds to, plus whatever else `render` needs to recompute its output.
/// Prefer a struct with named fields — even a `wire:click`-only component
/// with no `wire:model` bindings at all — over a fieldless unit struct
/// (`struct Foo;`, which `serde_json` serializes as `Value::Null` rather
/// than a JSON object): a client sending a non-empty `props` object for
/// such a component (a bug, or a crafted request) is rejected with a
/// clean 422, whereas an entirely-empty prop sync (the normal case for a
/// `wire:click`-only component) works either way.
///
/// `mount`/`render`/`call` are `async` (spelled as `-> impl Future<..> +
/// Send` rather than `async fn`, so the `+ Send` bound is explicit rather
/// than relying on an external `#[trait_variant]`-style macro) because a
/// real component routinely needs to do real async work — querying the
/// database in `render` (a search box), or persisting a change to it from
/// an action `wire:click` dispatches to in `call`. A sync-only trait would
/// make that impossible to express here at all.
pub trait WireComponent: Serialize + DeserializeOwned + Send + Sync + 'static {
    /// A stable, app-chosen registration name — the string a
    /// `@wire('name', ...)` call site names. Deliberately not
    /// `std::any::type_name::<Self>()`: that string isn't stable across a
    /// rename, and every already-mounted, session-stored instance would
    /// silently stop resolving the moment it changed.
    const NAME: &'static str;

    /// Builds this component's initial state from the props its
    /// `@wire(...)` call site passed. A prop name this component doesn't
    /// recognize is simply ignored — the same "unset becomes nothing"
    /// tolerance `@global`/`@stack` already have. `session` is the same
    /// real session `call`'s own `session` param is — available here so a
    /// component can capture per-viewer identity once, at mount time (e.g.
    /// "does the current user own this record, so should they see edit/
    /// delete controls" — `demo`'s `PostList` does exactly this), rather
    /// than needing to be told it again on every subsequent render.
    fn mount(
        session: &Session,
        props: &HashMap<String, serde_json::Value>,
    ) -> impl Future<Output = Self> + Send;

    /// Renders the current state. Typically ends with a
    /// `larust_support::view!("components.name", { .. })` call — use
    /// `View::into_html()`, not `into_response()`, since this is a fragment,
    /// not a full page (a fragment must never get the dev-reload script
    /// spliced into it).
    fn render(&self) -> impl Future<Output = View> + Send;

    /// Dispatches a `wire:click="action_name"`/`wire:submit="action_name"`-
    /// style call, mutating `self` in place. `session` is the requesting
    /// user's real session — the same one `mount()`'s surrounding
    /// `@wire(...)` call site has, threaded through here so an action can
    /// look up the logged-in user (`larust_support::auth::id(session)`) to
    /// do real, per-user work (creating a record as its author, say), not
    /// just mutate local component state. The default rejects every action
    /// name — a display-only, `wire:model`-only component needs zero
    /// action boilerplate; there's no silent-security-gap risk in
    /// defaulting to "reject everything" the way `Policy<U>`'s
    /// deliberately-defaultless abilities avoid a different risk
    /// (accidentally-permissive authorization).
    ///
    /// Returning `Ok(Some(path))` tells the client to navigate the browser
    /// to `path` instead of patching the current fragment in place —
    /// Livewire's own `redirect()`, for the common case of an action that
    /// finishes by taking the user somewhere else entirely (a form
    /// `wire:submit` that creates a record and sends the browser to it).
    /// `Ok(None)` is the normal case: re-render this component in place
    /// with whatever `self` mutation the action made (including, e.g.,
    /// setting a validation-errors field for the next render to display).
    fn call(
        &mut self,
        _session: &Session,
        action: &str,
        _args: &serde_json::Value,
    ) -> impl Future<Output = Result<Option<String>, AppError>> + Send {
        async move {
            Err(AppError::Http {
                status: StatusCode::NOT_FOUND,
                message: format!("component `{}` has no action `{action}`", Self::NAME),
            })
        }
    }
}
