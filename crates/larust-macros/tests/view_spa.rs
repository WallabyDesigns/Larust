//! End-to-end proof of `@spa ... @endspa` through the real `view!` macro
//! pipeline (parse -> resolve -> codegen -> render), mirroring
//! `view_larustscripts.rs`'s reasoning: `larust-view`'s own parser/resolve
//! unit tests pin the AST shape and yield-substitution in isolation; this
//! is what actually catches a regression reaching all the way through to
//! rendered output. `spa_layout.blade.xr` is `@extends`ed by
//! `spa_page.blade.xr` (plain content), `spa_page_with_wire.blade.xr`
//! (proving `@spa` and `@wire` coexist and emit their scripts
//! independently — including a `@wire(...)` mount nested *inside* the
//! `@spa` block, exercising `contains_wire`'s own `Node::Spa` recursion),
//! and `spa_page_without_spa.blade.xr` extends the pre-existing
//! `larustscripts_layout` (no `@spa` at all), proving a page that never
//! uses the directive gets none of its markup or script tag.

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

async fn page() -> Result<axum::response::Response, AppError> {
    let view = view!("spa_page", {});
    Ok(axum::response::IntoResponse::into_response(view))
}

async fn page_with_wire(session: Session) -> Result<axum::response::Response, AppError> {
    let view = view!("spa_page_with_wire", { session: &session });
    Ok(axum::response::IntoResponse::into_response(view))
}

async fn page_without_spa() -> Result<axum::response::Response, AppError> {
    let view = view!("spa_page_without_spa", {});
    Ok(axum::response::IntoResponse::into_response(view))
}

async fn body_of(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn spa_wraps_yielded_content_in_the_sentinel_div_and_emits_its_script() {
    let router = Router::new().route("/", get(page));

    let html = body_of(
        router
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap(),
    )
    .await;

    assert!(
        html.contains("<div id=\"__larust_spa_root\">"),
        "html was: {html}"
    );
    assert!(html.contains("hi"), "html was: {html}");
    assert!(
        html.contains(r#"<script src="/__larust_spa/runtime.js" defer></script>"#),
        "html was: {html}"
    );
}

#[tokio::test]
async fn a_page_never_using_spa_gets_neither_the_sentinel_div_nor_the_script() {
    let router = Router::new().route("/", get(page_without_spa));

    let html = body_of(
        router
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap(),
    )
    .await;

    assert!(
        !html.contains("__larust_spa"),
        "a page never using @spa should get neither the sentinel div nor the runtime script: \
         {html}"
    );
}

#[tokio::test]
async fn spa_and_wire_scripts_are_emitted_independently_even_when_wire_is_nested_inside_spa() {
    ensure_registered();
    let dir = tempfile::tempdir().unwrap().keep();
    let database_url = format!("sqlite://{}/test.sqlite", dir.display());
    larust_orm::connect(&database_url).await.unwrap();
    let pool = larust_orm::pool().unwrap().clone();
    let session_layer = build_session_layer(&pool, true).await.unwrap();
    let router = Router::new()
        .route("/", get(page_with_wire))
        .layer(session_layer);

    let html = body_of(
        router
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap(),
    )
    .await;

    assert!(
        html.contains("<div id=\"__larust_spa_root\">"),
        "html was: {html}"
    );
    assert!(
        html.contains(r#"<script src="/__larust_spa/runtime.js" defer></script>"#),
        "html was: {html}"
    );
    assert!(
        html.contains(r#"<script src="/__larust_wire/runtime.js" defer></script>"#),
        "html was: {html}"
    );
    assert!(
        html.contains("data-wire-id="),
        "the @wire(...) mount nested inside @spa should still actually mount: {html}"
    );
}
