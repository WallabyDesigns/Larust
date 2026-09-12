//! Proves `routes::update`'s new `x-wire-head-patch` response header: any
//! content a component's `render()` pushes to `'head'` (via
//! `larust_view::push_registry`) during a live `wire:click`/`wire:submit`-
//! triggered re-render is shipped as this header, base64-encoded, for the
//! client runtime to patch into the matching `<!--wire-head:{id}-->...
//! <!--/wire-head:{id}-->` marker `mount()` already wrote into
//! `document.head` at initial render. Mirrors `wire_test.rs`'s own
//! router-building pattern; a component here calls
//! `larust_view::push_registry::record` directly rather than going through
//! a real `.blade.xr` template + `view!` macro - `crates/larust-macros`'s
//! `view_wire_head_push.rs` already proves the codegen side of that; this
//! file is specifically about `routes::update`'s own header plumbing.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::routing::{get, post};
use axum::Router;
use larust_http::session::{session_layer as build_session_layer, Session};
use larust_live::{components, WireComponent};
use larust_view::View;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Once;
use tower::ServiceExt;

#[derive(Debug, Serialize, Deserialize)]
struct HeadPusher {
    title: String,
}

impl WireComponent for HeadPusher {
    const NAME: &'static str = "head_pusher";

    async fn mount(_session: &Session, _props: &HashMap<String, serde_json::Value>) -> Self {
        HeadPusher {
            title: "Initial".to_string(),
        }
    }

    async fn render(&self) -> View {
        larust_view::push_registry::record("head", format!("<title>{}</title>", self.title));
        View::new("<span>ok</span>".to_string())
    }

    async fn call(
        &mut self,
        _session: &Session,
        action: &str,
        _args: &serde_json::Value,
    ) -> Result<Option<String>, larust_core::AppError> {
        match action {
            "set_title" => {
                self.title = "Updated".to_string();
                Ok(None)
            }
            other => Err(larust_core::AppError::Http {
                status: StatusCode::NOT_FOUND,
                message: format!("unknown action `{other}`"),
            }),
        }
    }
}

/// A component that never touches `push_registry` at all - the ordinary
/// case (no dynamic head content) that must see no new header.
#[derive(Debug, Serialize, Deserialize)]
struct Quiet;

impl WireComponent for Quiet {
    const NAME: &'static str = "quiet";

    async fn mount(_session: &Session, _props: &HashMap<String, serde_json::Value>) -> Self {
        Quiet
    }

    async fn render(&self) -> View {
        View::new("<span>quiet</span>".to_string())
    }

    async fn call(
        &mut self,
        _session: &Session,
        action: &str,
        _args: &serde_json::Value,
    ) -> Result<Option<String>, larust_core::AppError> {
        match action {
            "noop" => Ok(None),
            other => Err(larust_core::AppError::Http {
                status: StatusCode::NOT_FOUND,
                message: format!("unknown action `{other}`"),
            }),
        }
    }
}

static REGISTER_ONCE: Once = Once::new();

fn ensure_registered() {
    REGISTER_ONCE.call_once(|| {
        components()
            .register::<HeadPusher>()
            .register::<Quiet>()
            .publish();
    });
}

async fn mount_head_pusher(session: Session) -> String {
    larust_live::mount(&session, "head_pusher", HashMap::new())
        .await
        .unwrap()
}

async fn mount_quiet(session: Session) -> String {
    larust_live::mount(&session, "quiet", HashMap::new())
        .await
        .unwrap()
}

async fn app() -> Router {
    ensure_registered();
    let dir = tempfile::tempdir().unwrap().keep();
    let database_url = format!("sqlite://{}/test.sqlite", dir.display());
    let _ = larust_orm::connect(&database_url).await;
    let pool = larust_orm::pool().unwrap().clone();
    let session_layer = build_session_layer(&pool, true).await.unwrap();
    Router::new()
        .route("/mount", get(mount_head_pusher))
        .route("/mount-quiet", get(mount_quiet))
        .route("/__larust_wire/:id", post(larust_live::update))
        .layer(session_layer)
        // The real per-request registry scope - see
        // `larust_http::push_registry_scope::scope`'s own doc comment.
        // `routes::update` is inert without this (its `drain` call just
        // finds nothing, same as before this whole mechanism existed).
        .layer(axum::middleware::from_fn(
            larust_http::push_registry_scope::scope,
        ))
}

fn extract_wire_id(html: &str) -> String {
    let start =
        html.find("data-wire-id=\"").expect("missing data-wire-id") + "data-wire-id=\"".len();
    let end = html[start..].find('"').unwrap() + start;
    html[start..end].to_string()
}

async fn get_mount(router: &Router, path: &str) -> (String, String) {
    let response = router
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string())
        .expect("mount should set a session cookie");
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (String::from_utf8(bytes.to_vec()).unwrap(), cookie)
}

async fn post_update(
    router: &Router,
    id: &str,
    cookie: &str,
    body: serde_json::Value,
) -> (StatusCode, Option<String>) {
    let response = router
        .clone()
        .oneshot(
            Request::post(format!("/__larust_wire/{id}"))
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let head_patch = response
        .headers()
        .get("x-wire-head-patch")
        .map(|v| v.to_str().unwrap().to_string());
    (status, head_patch)
}

#[tokio::test]
async fn an_action_that_pushes_to_head_sets_the_head_patch_header_with_the_fresh_content() {
    let router = app().await;
    let (html, cookie) = get_mount(&router, "/mount").await;
    let id = extract_wire_id(&html);

    let (status, head_patch) = post_update(
        &router,
        &id,
        &cookie,
        serde_json::json!({ "props": {}, "action": { "name": "set_title", "args": null } }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let encoded = head_patch.expect("expected an x-wire-head-patch header");
    let decoded = String::from_utf8(
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded).unwrap(),
    )
    .unwrap();
    assert_eq!(decoded, "<title>Updated</title>");
}

#[tokio::test]
async fn a_component_that_never_pushes_gets_no_head_patch_header() {
    let router = app().await;
    let (html, cookie) = get_mount(&router, "/mount-quiet").await;
    let id = extract_wire_id(&html);

    let (status, head_patch) = post_update(
        &router,
        &id,
        &cookie,
        serde_json::json!({ "props": {}, "action": { "name": "noop", "args": null } }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(head_patch, None);
}

#[tokio::test]
async fn each_update_only_carries_that_requests_own_fresh_push_not_an_accumulation() {
    let router = app().await;
    let (html, cookie) = get_mount(&router, "/mount").await;
    let id = extract_wire_id(&html);

    let (_status, first) = post_update(
        &router,
        &id,
        &cookie,
        serde_json::json!({ "props": {}, "action": { "name": "set_title", "args": null } }),
    )
    .await;
    let (_status, second) = post_update(
        &router,
        &id,
        &cookie,
        serde_json::json!({ "props": {}, "action": { "name": "set_title", "args": null } }),
    )
    .await;

    // Both updates set the title to the same value ("Updated"), so an
    // accumulating (rather than per-request-fresh) registry would still
    // pass an equality check - the real point is that `second` isn't
    // *first plus a duplicate*, confirmed by exact content below.
    let decode = |encoded: String| {
        String::from_utf8(
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded).unwrap(),
        )
        .unwrap()
    };
    assert_eq!(decode(first.unwrap()), "<title>Updated</title>");
    assert_eq!(decode(second.unwrap()), "<title>Updated</title>");
}
