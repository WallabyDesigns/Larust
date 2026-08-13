//! End-to-end proof that `<wire:name attr="literal" :attr2="expr" />` —
//! the HTML-tag-flavored alternative to `@wire('name', { ... })` — works
//! through the real `view!` macro pipeline and renders identically to the
//! directive syntax proven in `view_wire.rs`. Reuses that file's own
//! `Counter` component and router setup; only the template
//! (`wire_tag_test.blade.xr`, mounting `<wire:counter :count="5" />`
//! instead of `@wire('counter', { count: 5 })`) differs.

use axum::body::Body;
use axum::http::Request;
use axum::routing::get;
use axum::Router;
use larust_http::session::{sqlite_session_layer, Session};
use larust_support::view;
use larust_support::wire::{components, WireComponent};
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

async fn page(session: Session) -> Result<axum::response::Response, AppError> {
    let view = view!("wire_tag_test", { session: &session });
    Ok(axum::response::IntoResponse::into_response(view))
}

#[tokio::test]
async fn wire_tag_mounts_and_renders_a_component_through_the_view_macro() {
    ensure_registered();
    let pool = SqlitePool::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite pool");
    let session_layer = sqlite_session_layer(&pool, true).await.unwrap();
    let router = Router::new().route("/", get(page)).layer(session_layer);

    let response = router
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(bytes.to_vec()).unwrap();

    assert!(html.contains(r#"data-wire-id=""#), "html was: {html}");
    assert!(html.contains("<span>5</span>"), "html was: {html}");
    assert!(html.trim_start().starts_with("<div id=\"page\">"));
}
