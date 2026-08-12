use larust_http::axum::body::Body;
use larust_http::axum::extract::Request;
use larust_http::axum::http::{HeaderName, HeaderValue};
use larust_http::axum::middleware::{from_fn, Next};
use larust_http::axum::response::Response;
use larust_http::{Route, Router};
use tower::ServiceExt;

async fn index() -> &'static str {
    "index"
}
async fn create() -> &'static str {
    "create"
}
async fn store() -> &'static str {
    "store"
}
async fn show() -> &'static str {
    "show"
}
async fn edit() -> &'static str {
    "edit"
}
async fn update() -> &'static str {
    "update"
}
async fn destroy() -> &'static str {
    "destroy"
}

#[test]
fn resource_registers_all_seven_routes_with_laravel_naming() {
    let router = Route::resource(
        "posts", "post", index, create, store, show, edit, update, destroy,
    );

    let routes = router.routes();
    assert_eq!(routes.len(), 7);

    let find = |method: &str, path: &str| {
        routes
            .iter()
            .find(|r| r.method == method && r.path == path)
            .unwrap_or_else(|| panic!("no {method} {path} route registered"))
    };

    assert_eq!(find("GET", "/posts").name.as_deref(), Some("posts.index"));
    assert_eq!(
        find("GET", "/posts/create").name.as_deref(),
        Some("posts.create")
    );
    assert_eq!(find("POST", "/posts").name.as_deref(), Some("posts.store"));
    assert_eq!(
        find("GET", "/posts/{post}").name.as_deref(),
        Some("posts.show")
    );
    assert_eq!(
        find("GET", "/posts/{post}/edit").name.as_deref(),
        Some("posts.edit")
    );
    assert_eq!(
        find("PUT", "/posts/{post}").name.as_deref(),
        Some("posts.update")
    );
    assert_eq!(
        find("DELETE", "/posts/{post}").name.as_deref(),
        Some("posts.destroy")
    );
}

async fn mark(req: Request, next: Next) -> Response {
    let mut response = next.run(req).await;
    response.headers_mut().append(
        HeaderName::from_static("x-marked"),
        HeaderValue::from_static("1"),
    );
    response
}

#[test]
fn resource_composes_with_other_routes_and_a_shared_prefix_group() {
    async fn home() -> &'static str {
        "home"
    }

    let router = Route::get("/", home).group("/admin", |r: Router| {
        r.middleware(from_fn(mark)).resource(
            "posts", "post", index, create, store, show, edit, update, destroy,
        )
    });

    let routes = router.routes();
    // The top-level "/" route plus all 7 resource routes, nested under the
    // group's "/admin" prefix — proves route accumulation/naming survives
    // `resource()` being called inside a `.group()` closure after
    // `.middleware()`. Whether that middleware actually wraps the
    // resource's routes at request time is a separate claim, checked by
    // `resource_routes_are_wrapped_by_group_middleware` below — `RouteInfo`
    // (what `.routes()` returns) carries no information about middleware,
    // only path/method/name.
    assert_eq!(routes.len(), 8);
    assert!(routes.iter().any(|r| r.path == "/" && r.method == "GET"));
    assert!(routes.iter().any(|r| r.path == "/admin/posts"
        && r.method == "GET"
        && r.name.as_deref() == Some("posts.index")));
    assert!(routes.iter().any(|r| r.path == "/admin/posts/{post}/edit"
        && r.method == "GET"
        && r.name.as_deref() == Some("posts.edit")));
}

#[tokio::test]
async fn resource_routes_are_wrapped_by_group_middleware() {
    async fn home() -> &'static str {
        "home"
    }

    let router = Route::get("/", home)
        .group("/admin", |r: Router| {
            r.middleware(from_fn(mark)).resource(
                "posts", "post", index, create, store, show, edit, update, destroy,
            )
        })
        .into_axum_router();

    // A resource route wrapped by the group's middleware carries its mark...
    let posts_response = router
        .clone()
        .oneshot(Request::get("/admin/posts").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert!(posts_response.headers().contains_key("x-marked"));

    let post_response = router
        .clone()
        .oneshot(Request::get("/admin/posts/1").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert!(post_response.headers().contains_key("x-marked"));

    // ...but a route outside the group does not, proving the middleware is
    // genuinely scoped to the resource's routes rather than applied
    // globally by accident.
    let home_response = router
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert!(!home_response.headers().contains_key("x-marked"));
}
