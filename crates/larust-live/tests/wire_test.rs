//! `mount()` + `routes::update` exercised together against a real
//! `Session`/`AnySessionStore`, mirroring
//! `crates/larust-http/tests/csrf.rs`'s router-building pattern.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::routing::{get, post};
use axum::Router;
use larust_core::AppError;
use larust_http::session::{session_layer as build_session_layer, Session};
use larust_live::{components, WireComponent};
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

    async fn call(
        &mut self,
        _session: &Session,
        action: &str,
        _args: &serde_json::Value,
    ) -> Result<Option<String>, AppError> {
        match action {
            "increment" => {
                self.count += 1;
                Ok(None)
            }
            "reset_and_leave" => {
                self.count = 0;
                Ok(Some("/home".to_string()))
            }
            other => Err(AppError::Http {
                status: StatusCode::NOT_FOUND,
                message: format!("unknown action `{other}`"),
            }),
        }
    }
}

/// A unit-struct, `wire:click`-only component - no `wire:model` fields at
/// all, so `serde_json::to_value(Ping)` serializes as `Value::Null`, not a
/// JSON object. Regression coverage for the empty-props fast path in
/// `LiveRegistry::register`'s `set_many` closure: without it, dispatching
/// `noop` (which always sends an empty `props` object, since there's
/// nothing to bind) would 500 trying to merge props into a non-object
/// state.
#[derive(Debug, Serialize, Deserialize)]
struct Ping;

impl WireComponent for Ping {
    const NAME: &'static str = "ping";

    async fn mount(_session: &Session, _props: &HashMap<String, serde_json::Value>) -> Self {
        Ping
    }

    async fn render(&self) -> View {
        View::new("<span>pong</span>".to_string())
    }

    async fn call(
        &mut self,
        _session: &Session,
        action: &str,
        _args: &serde_json::Value,
    ) -> Result<Option<String>, AppError> {
        match action {
            "noop" => Ok(None),
            other => Err(AppError::Http {
                status: StatusCode::NOT_FOUND,
                message: format!("unknown action `{other}`"),
            }),
        }
    }
}

// `LiveRegistry::publish` is a process-wide `OnceLock` (first writer wins,
// same as `larust_events::ListenerRegistry`) - every `#[tokio::test]` fn in
// this file shares one process, so registration happens exactly once via
// `Once`, not once per test.
static REGISTER_ONCE: Once = Once::new();

fn ensure_registered() {
    REGISTER_ONCE.call_once(|| {
        components()
            .register::<Counter>()
            .register::<Ping>()
            .publish();
    });
}

async fn mount_counter(session: Session) -> String {
    let mut props = HashMap::new();
    props.insert("count".to_string(), serde_json::json!(5));
    larust_live::mount(&session, "counter", props)
        .await
        .unwrap()
}

async fn mount_ping(session: Session) -> String {
    larust_live::mount(&session, "ping", HashMap::new())
        .await
        .unwrap()
}

/// Every test in this file shares one process-wide pool -
/// `larust_orm::connect()` is a real once-per-process singleton, so the
/// first call here wins and every later call's "already connected" error
/// is deliberately swallowed. A real temp-file database, not
/// `sqlite::memory:`: a pool can open more than one physical connection,
/// and pooled `:memory:` connections each get their own private, empty
/// database without explicit shared-cache URI mode. Exercises the same
/// `AnySessionStore`/migration code path production uses either way.
async fn app() -> Router {
    ensure_registered();
    let dir = tempfile::tempdir().unwrap().keep();
    let database_url = format!("sqlite://{}/test.sqlite", dir.display());
    let _ = larust_orm::connect(&database_url).await;
    let pool = larust_orm::pool().unwrap().clone();
    let session_layer = build_session_layer(&pool, true).await.unwrap();
    Router::new()
        .route("/mount", get(mount_counter))
        .route("/mount-ping", get(mount_ping))
        .route("/__larust_wire/:id", post(larust_live::update))
        .layer(session_layer)
}

async fn get_with(router: &Router, path: &str, cookie: Option<&str>) -> (String, Option<String>) {
    let mut builder = Request::get(path);
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    let response = router
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();

    let set_cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string());

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (String::from_utf8(body.to_vec()).unwrap(), set_cookie)
}

fn extract_wire_id(html: &str) -> String {
    let start =
        html.find("data-wire-id=\"").expect("missing data-wire-id") + "data-wire-id=\"".len();
    let end = html[start..].find('"').unwrap() + start;
    html[start..end].to_string()
}

async fn post_update(
    router: &Router,
    id: &str,
    cookie: &str,
    body: serde_json::Value,
) -> (StatusCode, String) {
    let (status, _redirect, body) = post_update_full(router, id, cookie, body).await;
    (status, body)
}

async fn post_update_full(
    router: &Router,
    id: &str,
    cookie: &str,
    body: serde_json::Value,
) -> (StatusCode, Option<String>, String) {
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
    let redirect = response
        .headers()
        .get("x-wire-redirect")
        .map(|v| v.to_str().unwrap().to_string());
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, redirect, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn mount_renders_initial_state_from_props() {
    let router = app().await;
    let (html, _cookie) = get_with(&router, "/mount", None).await;
    assert!(html.contains("data-wire-id="));
    assert!(html.contains("<span>5</span>"));
}

#[tokio::test]
async fn two_mounts_in_one_session_get_independent_ids() {
    let router = app().await;
    let (html1, cookie) = get_with(&router, "/mount", None).await;
    let (html2, _) = get_with(&router, "/mount", cookie.as_deref()).await;

    assert_ne!(extract_wire_id(&html1), extract_wire_id(&html2));
}

#[tokio::test]
async fn update_syncs_a_prop_and_reflects_it_in_the_rendered_fragment() {
    let router = app().await;
    let (html, cookie) = get_with(&router, "/mount", None).await;
    let id = extract_wire_id(&html);
    let cookie = cookie.unwrap();

    let (status, body) = post_update(
        &router,
        &id,
        &cookie,
        serde_json::json!({ "props": { "count": 42 }, "action": null }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<span>42</span>"));
}

#[tokio::test]
async fn update_dispatches_an_action_after_applying_props() {
    let router = app().await;
    let (html, cookie) = get_with(&router, "/mount", None).await;
    let id = extract_wire_id(&html);
    let cookie = cookie.unwrap();

    let (status, body) = post_update(
        &router,
        &id,
        &cookie,
        serde_json::json!({ "props": { "count": 10 }, "action": { "name": "increment", "args": null } }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<span>11</span>"));
}

#[tokio::test]
async fn update_with_an_unknown_action_returns_not_found_and_leaves_state_unchanged() {
    let router = app().await;
    let (html, cookie) = get_with(&router, "/mount", None).await;
    let id = extract_wire_id(&html);
    let cookie = cookie.unwrap();

    let (status, _) = post_update(
        &router,
        &id,
        &cookie,
        serde_json::json!({ "props": {}, "action": { "name": "nonexistent", "args": null } }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // State wasn't mutated by the failed action - a follow-up sync with no
    // action still reports the original value.
    let (_status, body) = post_update(
        &router,
        &id,
        &cookie,
        serde_json::json!({ "props": {}, "action": null }),
    )
    .await;
    assert!(body.contains("<span>5</span>"));
}

#[tokio::test]
async fn an_action_returning_a_redirect_path_sets_the_redirect_header_and_still_saves_state() {
    let router = app().await;
    let (html, cookie) = get_with(&router, "/mount", None).await;
    let id = extract_wire_id(&html);
    let cookie = cookie.unwrap();

    let (status, redirect, _body) = post_update_full(
        &router,
        &id,
        &cookie,
        serde_json::json!({ "props": { "count": 99 }, "action": { "name": "reset_and_leave", "args": null } }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(redirect.as_deref(), Some("/home"));

    // The action's own mutation (`count = 0`) was still applied and saved
    // - a redirect signal doesn't bypass the normal save path.
    let (_status, body) = post_update(
        &router,
        &id,
        &cookie,
        serde_json::json!({ "props": {}, "action": null }),
    )
    .await;
    assert!(body.contains("<span>0</span>"));
}

#[tokio::test]
async fn update_against_a_stale_or_unknown_component_id_is_not_found() {
    let router = app().await;
    let (_html, cookie) = get_with(&router, "/mount", None).await;
    let cookie = cookie.unwrap();

    let (status, _) = post_update(
        &router,
        "not-a-real-id",
        &cookie,
        serde_json::json!({ "props": {}, "action": null }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn action_dispatch_on_a_unit_struct_component_with_no_props_succeeds() {
    let router = app().await;
    let (html, cookie) = get_with(&router, "/mount-ping", None).await;
    let id = extract_wire_id(&html);
    let cookie = cookie.unwrap();

    let (status, body) = post_update(
        &router,
        &id,
        &cookie,
        serde_json::json!({ "props": {}, "action": { "name": "noop", "args": null } }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<span>pong</span>"));
}

#[tokio::test]
async fn non_empty_props_against_a_unit_struct_component_is_rejected_with_422_not_a_500() {
    let router = app().await;
    let (html, cookie) = get_with(&router, "/mount-ping", None).await;
    let id = extract_wire_id(&html);
    let cookie = cookie.unwrap();

    let (status, _) = post_update(
        &router,
        &id,
        &cookie,
        serde_json::json!({ "props": { "unexpected": 1 }, "action": null }),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn update_with_a_type_mismatched_prop_is_rejected_with_422() {
    let router = app().await;
    let (html, cookie) = get_with(&router, "/mount", None).await;
    let id = extract_wire_id(&html);
    let cookie = cookie.unwrap();

    let (status, _) = post_update(
        &router,
        &id,
        &cookie,
        serde_json::json!({ "props": { "count": "not-a-number" }, "action": null }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}
