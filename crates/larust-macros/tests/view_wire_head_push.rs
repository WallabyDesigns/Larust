//! End-to-end proof that a `<wire:...>`-mounted component's own `@push`
//! reaches a *surrounding page's* `@stack('head')` on initial render - the
//! specific gap `docs/GOTCHAS.md`'s `@push`/`@stack` entry describes, and
//! `larust_view::push_registry`/`larust_http::push_registry_scope` exist to
//! close. Unlike `view_push_registry.rs` (which drives the registry
//! directly), this test goes through a real `larust_http::Router` -
//! `into_axum_router()` - so the actual, shipped middleware wiring (not
//! just the registry primitive) is what's under test.

use axum::body::Body;
use axum::http::Request;
use larust_http::session::Session;
use larust_support::view;
use larust_support::wire::{components, WireComponent};
use larust_support::AppError;
use larust_view::View;
use serde::{Deserialize, Serialize};
use std::sync::Once;
use tower::ServiceExt;

#[derive(Debug, Serialize, Deserialize)]
struct HeadPushComponent;

impl WireComponent for HeadPushComponent {
    const NAME: &'static str = "head_push";

    async fn mount(
        _session: &Session,
        _props: &std::collections::HashMap<String, serde_json::Value>,
    ) -> Self {
        HeadPushComponent
    }

    async fn render(&self) -> View {
        view!("wire_head_push_component", {})
    }
}

static REGISTER_ONCE: Once = Once::new();

fn ensure_registered() {
    REGISTER_ONCE.call_once(|| {
        components().register::<HeadPushComponent>().publish();
    });
}

async fn page(session: Session) -> Result<axum::response::Response, AppError> {
    let content = view!("wire_head_push_page_content", { session: &session }).into_html();
    let view = view!("wire_head_push_page_layout", { slot: content });
    Ok(axum::response::IntoResponse::into_response(view))
}

#[tokio::test]
async fn a_wire_components_push_reaches_the_pages_layout_stack_on_initial_render() {
    ensure_registered();
    let dir = tempfile::tempdir().unwrap().keep();
    let database_url = format!("sqlite://{}/test.sqlite", dir.display());
    larust_orm::connect(&database_url).await.unwrap();
    let pool = larust_orm::pool().unwrap().clone();

    let router = larust_http::Router::new()
        .get("/", page)
        .with_sessions(&pool, true)
        .await
        .unwrap()
        .into_axum_router();

    let response = router
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(bytes.to_vec()).unwrap();

    // The component's own head contribution reaches the page layout's
    // `@stack('head')`, wrapped in an addressable `<!--wire-head:{id}-->
    // ...<!--/wire-head:{id}-->` marker pair (see `larust_live::mount::mount`'s
    // own comment) - the same `id` as the component's `data-wire-id`, so a
    // later live re-render's head patch has somewhere to land.
    let wire_id = html
        .split("data-wire-id=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap_or_else(|| panic!("no data-wire-id found in: {html}"));
    let expected_head = format!(
        "<head><!--wire-head:{wire_id}--><title>Component Title</title><!--/wire-head:{wire_id}--></head>"
    );
    assert!(html.starts_with(&expected_head), "html was: {html}");
    assert!(html.contains("<span>hi</span>"), "html was: {html}");
}
