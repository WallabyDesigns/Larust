use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use larust_support::Model;
use tower::ServiceExt;

#[derive(Model, sqlx::FromRow, Debug, PartialEq)]
#[table("posts")]
pub struct Post {
    #[primary_key]
    pub id: i64,
    pub title: String,
}

#[derive(Model, sqlx::FromRow, Debug, PartialEq)]
#[table("categories")]
#[route_key("slug")]
pub struct Category {
    #[primary_key]
    pub id: i64,
    pub slug: String,
    pub name: String,
}

async fn show_post(post: Post) -> String {
    format!("post: {}", post.title)
}

async fn show_category(category: Category) -> String {
    format!("category: {}", category.name)
}

fn app() -> axum::Router {
    axum::Router::new()
        .route("/posts/:post", get(show_post))
        .route("/categories/:category", get(show_category))
}

async fn body_text(response: axum::response::Response) -> String {
    String::from_utf8(
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

#[tokio::test]
async fn route_model_binding_resolves_by_primary_key_and_route_key_and_404s_on_miss() {
    let db_dir = tempfile::tempdir().unwrap();
    let database_url = format!("sqlite://{}/test.sqlite", db_dir.path().display());
    larust_support::orm::connect(&database_url).await.unwrap();

    let pool = larust_support::orm::pool().unwrap();
    sqlx::query("CREATE TABLE posts (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL)")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE categories (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT NOT NULL, name TEXT NOT NULL)",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO posts (title) VALUES ('Hello')")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO categories (slug, name) VALUES ('rust', 'Rust')")
        .execute(pool)
        .await
        .unwrap();

    let router = app();

    // Default: resolves by primary key.
    let ok = router
        .clone()
        .oneshot(Request::get("/posts/1").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);
    assert_eq!(body_text(ok).await, "post: Hello");

    // 404, not a 500, for a nonexistent id.
    let missing = router
        .clone()
        .oneshot(Request::get("/posts/999").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    // 404, not a 500, for a non-numeric id (parse failure).
    let non_numeric = router
        .clone()
        .oneshot(
            Request::get("/posts/not-a-number")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(non_numeric.status(), StatusCode::NOT_FOUND);

    // #[route_key("slug")]: resolves by the named field, not the primary key.
    let by_slug = router
        .clone()
        .oneshot(
            Request::get("/categories/rust")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(by_slug.status(), StatusCode::OK);
    assert_eq!(body_text(by_slug).await, "category: Rust");

    // The primary key ("1") must NOT resolve via this route now that
    // #[route_key("slug")] is set.
    let by_id_should_miss = router
        .oneshot(Request::get("/categories/1").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(by_id_should_miss.status(), StatusCode::NOT_FOUND);
}
