use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;

/// Establishes a fresh `larust_view::push_registry` scope around every
/// request - see that module's own doc comment for what it's for (letting
/// `@push` content cross a `<wire:...>` mount boundary, or a
/// `.into_html()`-glued layout, that `@push`/`@stack`'s normal compile-time
/// resolution can't see across).
///
/// Wired in unconditionally by [`crate::Router::into_axum_router`] -
/// deliberately not an opt-in `.middleware(...)` call the way CSRF is, the
/// way `with_sessions`'s own session layer isn't either: an app that forgot
/// to opt in would silently reintroduce the exact "renders as nothing, no
/// error anywhere" failure this whole mechanism exists to close, for a
/// feature with no visible symptom until someone actually depends on it.
pub async fn scope(request: Request, next: Next) -> Response {
    larust_view::push_registry::with_scope(next.run(request)).await
}
