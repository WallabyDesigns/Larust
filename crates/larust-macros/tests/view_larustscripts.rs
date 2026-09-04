//! End-to-end proof of `@larustscripts` (Livewire's `@livewireScripts`
//! equivalent): a layout-placed marker that expands to the runtime
//! `<script>` tag only on pages whose resolved tree actually mounts a
//! `@wire(...)` component - decided once, at compile time, per template
//! (see `larust-macros/src/view.rs`'s `emit_wire_scripts` threading), not a
//! runtime branch. `larustscripts_layout.blade.xr` is `@extends`ed by both
//! `larustscripts_with_wire.blade.xr` (which mounts a component) and
//! `larustscripts_without_wire.blade.xr` (which doesn't), proving the same
//! shared layout produces different output for each.

use axum::body::Body;
use axum::http::Request;
use axum::routing::get;
use axum::Router;
use larust_http::session::{session_layer as build_session_layer, Session};
use larust_support::view;
use larust_support::wire::{components, WireComponent};
use larust_support::AppError;
use larust_view::View;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Once;
use tower::ServiceExt;

#[derive(Debug, Serialize, Deserialize)]
struct Counter {
    count: i64,
}

impl WireComponent for Counter {
    const NAME: &'static str = "counter";

    async fn mount(_session: &Session, props: &HashMap<String, serde_json::Value>) -> Self {
        let count = props.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
        Counter { count }
    }

    async fn render(&self) -> View {
        View::new(format!("<span>{}</span>", self.count))
    }
}

static REGISTER_ONCE: Once = Once::new();

fn ensure_registered() {
    REGISTER_ONCE.call_once(|| {
        components().register::<Counter>().publish();
    });
}

async fn with_wire(session: Session) -> Result<axum::response::Response, AppError> {
    let view = view!("larustscripts_with_wire", { session: &session });
    Ok(axum::response::IntoResponse::into_response(view))
}

async fn without_wire() -> Result<axum::response::Response, AppError> {
    let view = view!("larustscripts_without_wire", {});
    Ok(axum::response::IntoResponse::into_response(view))
}

async fn with_live() -> Result<axum::response::Response, AppError> {
    let view = view!("larustscripts_with_live", {});
    Ok(axum::response::IntoResponse::into_response(view))
}

async fn body_of(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn larustscripts_expands_only_on_pages_that_mount_a_wire_component() {
    ensure_registered();
    let dir = tempfile::tempdir().unwrap().keep();
    let database_url = format!("sqlite://{}/test.sqlite", dir.display());
    larust_orm::connect(&database_url).await.unwrap();
    let pool = larust_orm::pool().unwrap().clone();
    let session_layer = build_session_layer(&pool, true).await.unwrap();
    let router = Router::new()
        .route("/with-wire", get(with_wire))
        .route("/without-wire", get(without_wire))
        .layer(session_layer);

    let with_wire_html = body_of(
        router
            .clone()
            .oneshot(Request::get("/with-wire").body(Body::empty()).unwrap())
            .await
            .unwrap(),
    )
    .await;
    assert!(
        with_wire_html.contains(r#"<script src="/__larust_wire/runtime.js" defer></script>"#),
        "html was: {with_wire_html}"
    );

    let without_wire_html = body_of(
        router
            .oneshot(Request::get("/without-wire").body(Body::empty()).unwrap())
            .await
            .unwrap(),
    )
    .await;
    assert!(
        !without_wire_html.contains("__larust_wire"),
        "html was: {without_wire_html}"
    );
    assert!(
        !without_wire_html.contains("__larust_push"),
        "a page using neither @wire nor @live should get neither script: {without_wire_html}"
    );
}

#[tokio::test]
async fn larustscripts_also_emits_the_push_runtime_script_for_pages_using_live_but_not_wire() {
    let router = Router::new().route("/with-live", get(with_live));

    let html = body_of(
        router
            .oneshot(Request::get("/with-live").body(Body::empty()).unwrap())
            .await
            .unwrap(),
    )
    .await;
    assert!(
        html.contains(r#"<script src="/__larust_push/runtime.js" defer></script>"#),
        "html was: {html}"
    );
    assert!(
        !html.contains("__larust_wire"),
        "a page using @live but not @wire shouldn't get the wire script: {html}"
    );
}
