use crate::lock::with_session_lock;
use crate::mount::wrap;
use crate::registry;
use crate::state::{load_components, save_components};
use axum::extract::Path;
use axum::http::header::{HeaderName, HeaderValue, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use axum::Json;
use larust_core::AppError;
use larust_http::session::Session;
use serde::Deserialize;
use std::collections::HashMap;

/// A `wire:submit`/`wire:click` action that returns `Some(path)` from
/// `LiveComponent::call` (Livewire's own `redirect()`) signals "navigate
/// the browser to `path`" through this response header — read by the
/// client runtime's `sync()` before it ever looks at the body. A header,
/// not a body-shaped convention, so the response's `Content-Type` stays
/// `text/html` and its body stays the same `<div data-live-id="...">`
/// fragment shape in both cases (still saved/rendered normally either way,
/// in case the redirect target itself reads this component's now-updated
/// state back out of the session).
const REDIRECT_HEADER: &str = "x-live-redirect";

const RUNTIME_JS: &str = include_str!("../assets/live-runtime.js");

/// A deeply-nested `props`/`args` payload (an attacker-controlled input,
/// gated only by CSRF) can't stack-overflow this endpoint's JSON parsing:
/// `axum::Json`'s `serde_json::from_slice` runs with `serde_json`'s default
/// recursion limit (128 levels, `Deserializer::disable_recursion_limit`
/// left uncalled) already in force — confirmed against `serde_json`'s own
/// source, not assumed.
#[derive(Debug, Deserialize)]
pub struct UpdatePayload {
    #[serde(default)]
    props: HashMap<String, serde_json::Value>,
    #[serde(default)]
    action: Option<ActionCall>,
}

#[derive(Debug, Deserialize)]
struct ActionCall {
    name: String,
    #[serde(default)]
    args: serde_json::Value,
}

/// `POST /__larust_live/{component_id}` — handles both a `wire:model`-style
/// prop sync and a `wire:click`-style action call in one request (see
/// `UpdatePayload`): every sync carries the component's *entire* current
/// `wire:model` field set, not a delta, which is what correctly threads a
/// deferred field's just-typed value through when a different element's
/// click/live-sync is what actually triggers the request.
///
/// The whole sync is atomic: if the prop merge or the action call fails,
/// nothing is written back to the session — the previously-stored state is
/// left untouched. Response is `200 text/html`, the same
/// `<div data-live-id="...">` wrapper shape `mount()` produces.
pub async fn update(
    session: Session,
    Path(component_id): Path<String>,
    Json(payload): Json<UpdatePayload>,
) -> Result<Response, AppError> {
    with_session_lock(&session, || async {
        let mut components = load_components(&session).await?;
        let index = components
            .iter()
            .position(|(id, _)| id == &component_id)
            .ok_or(AppError::NotFound)?;

        let entry = registry::lookup(&components[index].1.name).ok_or(AppError::NotFound)?;

        let mut state = (entry.set_many)(components[index].1.state.clone(), &payload.props)?;
        let mut redirect = None;
        if let Some(action) = &payload.action {
            let (new_state, action_redirect) =
                (entry.call)(state, &session, &action.name, &action.args).await?;
            state = new_state;
            redirect = action_redirect;
        }
        let html = (entry.render)(&state).await?;

        components[index].1.state = state;
        save_components(&session, &components).await?;

        Ok(html_response(wrap(&component_id, &html), redirect))
    })
    .await
}

fn html_response(html: String, redirect: Option<String>) -> Response {
    let mut response = ([(CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response();
    if let Some(path) = redirect {
        // A malformed path (non-ASCII, a stray control character) can't
        // become a valid header value — falls back to a safe default
        // rather than let an otherwise-successful action response 500 on
        // header construction. Component authors control this string, but
        // it's still worth not panicking/erroring on a typo.
        let value = HeaderValue::from_str(&path).unwrap_or_else(|_| HeaderValue::from_static("/"));
        response
            .headers_mut()
            .insert(HeaderName::from_static(REDIRECT_HEADER), value);
    }
    response
}

/// `GET /__larust_live/runtime.js` — the vendored client runtime, served
/// from the installed `larust-live` crate itself (`include_str!`'d, not
/// vendored into every scaffolded app's `public/js/`) so it stays
/// version-locked to the framework with zero drift/"forgot to re-copy
/// after an upgrade" risk.
pub async fn runtime_js() -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "text/javascript; charset=utf-8")],
        RUNTIME_JS,
    )
}
