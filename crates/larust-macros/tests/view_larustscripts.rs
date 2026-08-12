//! End-to-end proof of `@larustscripts` (Livewire's `@livewireScripts`
//! equivalent): a layout-placed marker that expands to the runtime
//! `<script>` tag only on pages whose resolved tree actually mounts a
//! `@live(...)` component — decided once, at compile time, per template
//! (see `larust-macros/src/view.rs`'s `emit_live_scripts` threading), not a
//! runtime branch. `larustscripts_layout.blade.xr` is `@extends`ed by both
//! `larustscripts_with_live.blade.xr` (which mounts a component) and
//! `larustscripts_without_live.blade.xr` (which doesn't), proving the same
//! shared layout produces different output for each.

use axum::body::Body;
use axum::http::Request;
use axum::routing::get;
use axum::Router;
use larust_http::session::{sqlite_session_layer, Session};
use larust_support::live::{components, LiveComponent};
use larust_support::view;
use larust_support::AppError;
use larust_view::View;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::Once;
use tower::ServiceExt;

#[derive(Debug, Serialize, Deserialize)]
struct Counter {
    count: i64,
}

impl LiveComponent for Counter {
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

async fn with_live(session: Session) -> Result<axum::response::Response, AppError> {
    let view = view!("larustscripts_with_live", { session: &session });
    Ok(axum::response::IntoResponse::into_response(view))
}

async fn without_live() -> Result<axum::response::Response, AppError> {
    let view = view!("larustscripts_without_live", {});
    Ok(axum::response::IntoResponse::into_response(view))
}

async fn body_of(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn larustscripts_expands_only_on_pages_that_mount_a_live_component() {
    ensure_registered();
    let pool = SqlitePool::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite pool");
    let session_layer = sqlite_session_layer(&pool, true).await.unwrap();
    let router = Router::new()
        .route("/with-live", get(with_live))
        .route("/without-live", get(without_live))
        .layer(session_layer);

    let with_live_html = body_of(
        router
            .clone()
            .oneshot(Request::get("/with-live").body(Body::empty()).unwrap())
            .await
            .unwrap(),
    )
    .await;
    assert!(
        with_live_html.contains(r#"<script src="/__larust_live/runtime.js" defer></script>"#),
        "html was: {with_live_html}"
    );

    let without_live_html = body_of(
        router
            .oneshot(Request::get("/without-live").body(Body::empty()).unwrap())
            .await
            .unwrap(),
    )
    .await;
    assert!(
        !without_live_html.contains("__larust_live"),
        "html was: {without_live_html}"
    );
}
