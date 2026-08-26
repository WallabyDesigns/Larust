use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, HeaderName, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use larust_http::session::Session;
use larust_http::{csrf, Route, Router};
use tower::ServiceExt;

async fn show_token(session: Session) -> String {
    csrf::token(&session).await
}

async fn submit() -> &'static str {
    "ok"
}

/// Every test in this file that needs a pool shares one process-wide
/// pool — `larust_orm::connect()` is a real once-per-process singleton
/// (like every other test suite in this codebase that uses it), so the
/// first call here wins and every later call's "already connected" error
/// is deliberately swallowed. A real temp-file database, not
/// `sqlite::memory:`: a pool can open more than one physical connection,
/// and pooled `:memory:` connections each get their own private, empty
/// database without explicit shared-cache URI mode — the same reasoning
/// `larust_testing::db::test_db`'s own doc comment gives for avoiding it.
/// Harmless for what these tests exercise (cookie/CSRF-attribute checks
/// through independent request pairs, never cross-test data isolation).
/// Exercises the same `AnySessionStore`/migration code path production
/// uses either way.
async fn test_pool() -> sqlx::AnyPool {
    let dir = tempfile::tempdir().unwrap().keep();
    let database_url = format!("sqlite://{}/test.sqlite", dir.display());
    let _ = larust_orm::connect(&database_url).await;
    larust_orm::pool().unwrap().clone()
}

#[tokio::test]
async fn router_middleware_and_with_sessions_compose_regardless_of_call_order() {
    // `.middleware(csrf::verify)` is declared *before* `.with_sessions()`
    // here — session must still end up outermost (available to CSRF's
    // extractors) regardless of that ordering, per `Router`'s contract.
    let pool = test_pool().await;
    let router = Route::get("/token", show_token)
        .post("/submit", submit)
        .middleware(axum::middleware::from_fn(csrf::verify))
        .with_sessions(&pool, true)
        .await
        .unwrap()
        .into_axum_router();

    let response = router
        .clone()
        .oneshot(Request::get("/token").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .expect("Router::with_sessions() should set a session cookie")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let token = String::from_utf8(
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();

    let ok_response = router
        .clone()
        .oneshot(
            Request::post("/submit")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(format!("_csrf_token={token}")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ok_response.status(), StatusCode::OK);

    let rejected_response = router
        .oneshot(
            Request::post("/submit")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from("_csrf_token=wrong"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        rejected_response.status(),
        StatusCode::from_u16(419).unwrap()
    );
}

#[tokio::test]
async fn with_sessions_called_before_middleware_still_places_sessions_outermost() {
    // The reverse of the call order in the test above — `.with_sessions()`
    // comes first this time.
    let pool = test_pool().await;
    let router = Route::get("/token", show_token)
        .post("/submit", submit)
        .with_sessions(&pool, true)
        .await
        .unwrap()
        .middleware(axum::middleware::from_fn(csrf::verify))
        .into_axum_router();

    let response = router
        .clone()
        .oneshot(Request::get("/token").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .expect("session cookie should still be set regardless of call order")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let token = String::from_utf8(
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();

    let ok_response = router
        .oneshot(
            Request::post("/submit")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(format!("_csrf_token={token}")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ok_response.status(), StatusCode::OK);
}

#[tokio::test]
async fn with_sessions_secure_flag_controls_the_cookies_secure_attribute() {
    // `with_sessions(true)` (the default apps get unless SESSION_SECURE_COOKIE=false
    // is set) must keep the `Secure` attribute — regression guard for the
    // framework's safe-by-default posture.
    let secure_pool = test_pool().await;
    let secure_router = Route::get("/token", show_token)
        .with_sessions(&secure_pool, true)
        .await
        .unwrap()
        .into_axum_router();
    let secure_response = secure_router
        .oneshot(Request::get("/token").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let secure_cookie = secure_response
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        secure_cookie.to_lowercase().contains("secure"),
        "with_sessions(true) should set a Secure cookie, got: {secure_cookie}"
    );

    // `with_sessions(false)` (SESSION_SECURE_COOKIE=false, for local dev on
    // a custom hostname like a `.test` domain) must drop it — this is the
    // actual fix: without it, browsers silently discard the Set-Cookie
    // header on any host outside their loopback/localhost allowlist, and
    // sessions (so CSRF) break with no error surfaced anywhere.
    let insecure_pool = test_pool().await;
    let insecure_router = Route::get("/token", show_token)
        .with_sessions(&insecure_pool, false)
        .await
        .unwrap()
        .into_axum_router();
    let insecure_response = insecure_router
        .oneshot(Request::get("/token").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let insecure_cookie = insecure_response
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        !insecure_cookie.to_lowercase().contains("secure"),
        "with_sessions(false) should not set a Secure cookie, got: {insecure_cookie}"
    );
}

async fn mark_a(req: Request, next: Next) -> Response {
    let mut response = next.run(req).await;
    response.headers_mut().append(
        HeaderName::from_static("x-order"),
        HeaderValue::from_static("a"),
    );
    response
}

async fn mark_b(req: Request, next: Next) -> Response {
    let mut response = next.run(req).await;
    response.headers_mut().append(
        HeaderName::from_static("x-order"),
        HeaderValue::from_static("b"),
    );
    response
}

async fn mark_c(req: Request, next: Next) -> Response {
    let mut response = next.run(req).await;
    response.headers_mut().append(
        HeaderName::from_static("x-order"),
        HeaderValue::from_static("c"),
    );
    response
}

fn order_header(response: &Response) -> Vec<&str> {
    response
        .headers()
        .get_all("x-order")
        .iter()
        .map(|v| v.to_str().unwrap())
        .collect()
}

#[tokio::test]
async fn middleware_call_order_is_execution_order() {
    // A registered first, B registered second. A middleware's own logic
    // that runs *after* `next.run()` (like this header append) only fires
    // once everything nested inside it — every later-registered middleware,
    // then the handler — has finished. So if call order is execution order
    // (A runs first / is outermost, per the fix), B's post-`next.run()`
    // append happens first (it's innermost) and A's happens last: headers
    // come out ["b", "a"]. Before the ordering fix, this would have come
    // out reversed ("a" first) since the last-registered middleware used
    // to end up outermost instead.
    let router = Route::get("/", || async { "ok" })
        .middleware(axum::middleware::from_fn(mark_a))
        .middleware(axum::middleware::from_fn(mark_b))
        .into_axum_router();

    let response = router
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();

    let order: Vec<&str> = response
        .headers()
        .get_all("x-order")
        .iter()
        .map(|v| v.to_str().unwrap())
        .collect();
    assert_eq!(order, vec!["b", "a"]);
}

#[tokio::test]
async fn group_scoped_middleware_only_affects_routes_inside_the_group() {
    let router = Route::get("/public", || async { "public" })
        .group("/admin", |r: Router| {
            r.middleware(axum::middleware::from_fn(mark_a))
                .get("/dashboard", || async { "dashboard" })
        })
        .into_axum_router();

    let public_response = router
        .clone()
        .oneshot(Request::get("/public").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert!(
        !public_response.headers().contains_key("x-order"),
        "middleware registered inside a group must not affect routes outside it"
    );

    let admin_response = router
        .oneshot(
            Request::get("/admin/dashboard")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        admin_response
            .headers()
            .get("x-order")
            .and_then(|v| v.to_str().ok()),
        Some("a"),
        "middleware registered inside a group must affect that group's own routes"
    );
}

#[tokio::test]
async fn top_level_middleware_still_covers_routes_added_via_group() {
    let router = Route::get("/public", || async { "public" })
        .group("/admin", |r: Router| {
            r.get("/dashboard", || async { "dashboard" })
        })
        .middleware(axum::middleware::from_fn(mark_a))
        .into_axum_router();

    let admin_response = router
        .oneshot(
            Request::get("/admin/dashboard")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        admin_response
            .headers()
            .get("x-order")
            .and_then(|v| v.to_str().ok()),
        Some("a"),
        "a top-level .middleware() call must still cover routes registered via .group()"
    );
}

#[tokio::test]
async fn group_and_top_level_middleware_compose_with_group_innermost() {
    // A group's middleware is baked into its entries before those entries
    // are merged into the parent router, and the parent's own top-level
    // middleware is applied on top of that afterwards — so top-level
    // middleware is always outermost relative to a group's own, regardless
    // of registration order between the two.
    let router = Route::get("/public", || async { "public" })
        .middleware(axum::middleware::from_fn(mark_a))
        .group("/admin", |r: Router| {
            r.middleware(axum::middleware::from_fn(mark_b))
                .get("/dashboard", || async { "dashboard" })
        })
        .into_axum_router();

    let admin_response = router
        .oneshot(
            Request::get("/admin/dashboard")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let order: Vec<&str> = admin_response
        .headers()
        .get_all("x-order")
        .iter()
        .map(|v| v.to_str().unwrap())
        .collect();
    // Group middleware (mark_b) is baked into the entry's own MethodRouter
    // first, then the top-level middleware (mark_a) wraps the whole
    // MethodRouter afterwards in `into_axum_router` — so mark_a is
    // outermost/runs first, mark_b is innermost. A post-`next.run()` header
    // append therefore fires on mark_b first (innermost finishes first),
    // then mark_a: ["b", "a"].
    assert_eq!(order, vec!["b", "a"]);
}

#[tokio::test]
async fn group_registered_before_top_level_middleware_still_composes_with_group_innermost() {
    // The reverse call order of the test above (`.group()` before
    // `.middleware()` this time) — `self.middlewares` is only applied
    // uniformly to every entry at `into_axum_router()` time, so which of
    // `.group()`/`.middleware()` was called first on the *parent* router
    // doesn't change the result: group middleware is always innermost.
    let router = Route::get("/public", || async { "public" })
        .group("/admin", |r: Router| {
            r.middleware(axum::middleware::from_fn(mark_b))
                .get("/dashboard", || async { "dashboard" })
        })
        .middleware(axum::middleware::from_fn(mark_a))
        .into_axum_router();

    let admin_response = router
        .oneshot(
            Request::get("/admin/dashboard")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(order_header(&admin_response), vec!["b", "a"]);
}

#[tokio::test]
async fn nested_groups_compose_middleware_innermost_by_depth() {
    // mark_a: outer group's own middleware. mark_b: inner group's own
    // middleware. The inner group is built and merged first (innermost),
    // then the outer group's middleware wraps around the merged result —
    // so mark_b should be innermost (fires first) and mark_a outermost.
    let router = Route::group("/outer", |outer: Router| {
        outer
            .middleware(axum::middleware::from_fn(mark_a))
            .group("/inner", |inner: Router| {
                inner
                    .middleware(axum::middleware::from_fn(mark_b))
                    .get("/leaf", || async { "leaf" })
            })
    })
    .into_axum_router();

    let response = router
        .oneshot(
            Request::get("/outer/inner/leaf")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(order_header(&response), vec!["b", "a"]);
}

#[tokio::test]
async fn sibling_groups_do_not_share_each_others_middleware() {
    let router = Route::group("/a", |r: Router| {
        r.middleware(axum::middleware::from_fn(mark_a))
            .get("/route", || async { "a" })
    })
    .group("/b", |r: Router| {
        r.middleware(axum::middleware::from_fn(mark_b))
            .get("/route", || async { "b" })
    })
    .into_axum_router();

    let a_response = router
        .clone()
        .oneshot(Request::get("/a/route").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(order_header(&a_response), vec!["a"]);

    let b_response = router
        .oneshot(Request::get("/b/route").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(order_header(&b_response), vec!["b"]);
}

#[tokio::test]
async fn top_level_middleware_covers_routes_added_both_before_and_after_a_group() {
    // mark_c is registered *between* a route added before the group and a
    // route added after it — global middleware doesn't care about
    // registration order relative to routes/groups, only about being
    // applied once at `into_axum_router()` time to every entry that ends
    // up in `self.entries`.
    let router = Route::get("/before", || async { "before" })
        .group("/admin", |r: Router| {
            r.get("/dashboard", || async { "dashboard" })
        })
        .middleware(axum::middleware::from_fn(mark_c))
        .get("/after", || async { "after" })
        .into_axum_router();

    for path in ["/before", "/admin/dashboard", "/after"] {
        let response = router
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            order_header(&response),
            vec!["c"],
            "top-level middleware should cover `{path}` regardless of when it was registered relative to .middleware()"
        );
    }
}

#[tokio::test]
async fn merge_does_not_leak_either_sides_top_level_middleware_onto_the_other() {
    // The exact regression this method exists to fix: `web`-shaped router
    // has its own top-level middleware (mark_a), `api`-shaped router has a
    // *different* one (mark_b) — after merging, each side's routes must
    // carry only their own, never the other's. `.group(...)` deliberately
    // does NOT have this property (see `top_level_middleware_still_covers_
    // routes_added_via_group` above) — `.merge` exists specifically for
    // when that sharing is unwanted.
    let web = Route::get("/", || async { "home" }).middleware(axum::middleware::from_fn(mark_a));
    let api =
        Route::get("/users", || async { "users" }).middleware(axum::middleware::from_fn(mark_b));
    let router = web.merge("/api", api).into_axum_router();

    let home_response = router
        .clone()
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        order_header(&home_response),
        vec!["a"],
        "the web router's own middleware must still cover its own routes"
    );
    assert!(
        !order_header(&home_response).contains(&"b"),
        "the api router's middleware must never leak onto the web router's routes"
    );

    let users_response = router
        .oneshot(Request::get("/api/users").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        order_header(&users_response),
        vec!["b"],
        "the api router's own middleware must still cover its own (merged) routes"
    );
    assert!(
        !order_header(&users_response).contains(&"a"),
        "the web router's middleware must never leak onto the api router's merged-in routes"
    );
}

#[tokio::test]
async fn merge_prefixes_the_other_routers_paths_and_preserves_names() {
    let web = Route::get("/", || async { "home" });
    let api = Route::get("/users", || async { "users" }).name("api.users");
    let router = web.merge("/api", api);

    let infos = router.routes();
    let api_users = infos
        .iter()
        .find(|info| info.path == "/api/users")
        .expect("merged route should be prefixed");
    assert_eq!(api_users.name.as_deref(), Some("api.users"));
}

#[tokio::test]
async fn merge_leaves_a_csrf_protected_web_router_from_rejecting_a_merged_in_api_post() {
    // The literal real-world bug this method fixes: a `web`-shaped router
    // wraps itself in `csrf::verify`, then merges in an `api`-shaped
    // router with its own unrelated middleware and no CSRF at all — a POST
    // to the merged-in api route must succeed with no CSRF token at all,
    // proving CSRF genuinely never reaches it (unlike `.group`, which the
    // `top_level_middleware_covers_routes_added_both_before_and_after_a_
    // group` test above proves *would* leak it).
    let web =
        Route::get("/", || async { "home" }).middleware(axum::middleware::from_fn(csrf::verify));
    let api = Route::post("/tokens", || async { "token" });
    let router = web.merge("/api", api).into_axum_router();

    let response = router
        .oneshot(Request::post("/api/tokens").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a merged-in api route must never be subject to the web router's own CSRF middleware"
    );
}
