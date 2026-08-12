//! End-to-end coverage for `guard.rs`/`extractor.rs`/`middleware.rs` against
//! a real (in-memory) session store and a real axum router — these three
//! modules have no unit tests of their own since they're only meaningful
//! wired together through a request/response cycle. Also serves as the
//! first real proof that `Authenticatable::find_for_auth`'s `-> impl
//! Future<...> + Send` signature (see `authenticatable.rs`'s doc comment)
//! is actually implementable with a plain `async fn` in the impl, as
//! claimed.

use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, StatusCode};
use larust_auth::{check, login, logout, require_auth, Auth, Authenticatable};
use larust_core::AppError;
use larust_http::session::Session;
use larust_http::{Route, Router};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use tower::ServiceExt;

#[derive(Clone, Debug, PartialEq)]
struct TestUser {
    id: i64,
    name: String,
}

// A plain `async fn` implementation — the exact shape the doc comment on
// `Authenticatable::find_for_auth` promises works despite the trait
// declaring `-> impl Future<...> + Send` rather than `async fn`.
impl Authenticatable for TestUser {
    fn auth_id(&self) -> i64 {
        self.id
    }

    async fn find_for_auth(id: i64) -> Result<Option<Self>, AppError> {
        Ok(fake_users().lock().unwrap().get(&id).cloned())
    }
}

fn fake_users() -> &'static Mutex<HashMap<i64, TestUser>> {
    static USERS: OnceLock<Mutex<HashMap<i64, TestUser>>> = OnceLock::new();
    USERS.get_or_init(|| {
        let mut users = HashMap::new();
        users.insert(
            1,
            TestUser {
                id: 1,
                name: "Alice".to_string(),
            },
        );
        Mutex::new(users)
    })
}

async fn do_login(session: Session) -> &'static str {
    let user = fake_users().lock().unwrap().get(&1).cloned().unwrap();
    login(&session, &user).await.unwrap();
    "logged in"
}

async fn do_logout(session: Session) -> &'static str {
    logout(&session).await.unwrap();
    "logged out"
}

async fn do_check(session: Session) -> String {
    check(&session).await.unwrap().to_string()
}

async fn whoami(Auth(user): Auth<TestUser>) -> String {
    user.name
}

fn get(path: &str, cookie: Option<&str>) -> Request {
    let mut builder = Request::get(path);
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    builder.body(Body::empty()).unwrap()
}

fn post(path: &str, cookie: Option<&str>) -> Request {
    let mut builder = Request::post(path);
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    builder.body(Body::empty()).unwrap()
}

fn session_cookie(response: &axum::response::Response) -> String {
    response
        .headers()
        .get(header::SET_COOKIE)
        .expect("response should set a session cookie")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn login_logout_and_the_auth_extractor_round_trip_through_a_real_router() {
    let router = Route::post("/login", do_login)
        .name("login")
        .post("/logout", do_logout)
        .get("/check", do_check)
        .get("/whoami", whoami)
        .group("", |r: Router| {
            r.middleware(axum::middleware::from_fn(require_auth))
                .get("/dashboard", || async { "dashboard" })
        });
    let pool = sqlx::SqlitePool::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite pool");
    let router = router
        .with_sessions(&pool, true)
        .await
        .unwrap()
        .into_axum_router();

    // Logged out: check() is false, the Auth<U> extractor 401s, and
    // require_auth redirects away from the protected route.
    let check_response = router.clone().oneshot(get("/check", None)).await.unwrap();
    let body = String::from_utf8(
        axum::body::to_bytes(check_response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert_eq!(body, "false");

    let whoami_response = router.clone().oneshot(get("/whoami", None)).await.unwrap();
    assert_eq!(whoami_response.status(), StatusCode::UNAUTHORIZED);

    let dashboard_response = router
        .clone()
        .oneshot(get("/dashboard", None))
        .await
        .unwrap();
    assert!(dashboard_response.status().is_redirection());
    assert_eq!(
        dashboard_response
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok()),
        Some("/login"),
        "require_auth should redirect to the named `login` route's path"
    );

    // Log in — capture the post-login session cookie (login() rotates the
    // session id via cycle_id(), so this is a fresh cookie, not whatever
    // anonymous session existed before, if any).
    let login_response = router.clone().oneshot(post("/login", None)).await.unwrap();
    let cookie = session_cookie(&login_response);

    // Logged in: check() is true, the extractor resolves the real user,
    // and the protected route is reachable.
    let check_response = router
        .clone()
        .oneshot(get("/check", Some(&cookie)))
        .await
        .unwrap();
    let body = String::from_utf8(
        axum::body::to_bytes(check_response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert_eq!(body, "true");

    let whoami_response = router
        .clone()
        .oneshot(get("/whoami", Some(&cookie)))
        .await
        .unwrap();
    assert_eq!(whoami_response.status(), StatusCode::OK);
    let name = String::from_utf8(
        axum::body::to_bytes(whoami_response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert_eq!(name, "Alice");

    let dashboard_response = router
        .clone()
        .oneshot(get("/dashboard", Some(&cookie)))
        .await
        .unwrap();
    assert_eq!(dashboard_response.status(), StatusCode::OK);

    // Log out — the same cookie should no longer be treated as
    // authenticated (logout flushes the whole session).
    let _ = router
        .clone()
        .oneshot(post("/logout", Some(&cookie)))
        .await
        .unwrap();
    let check_response = router
        .clone()
        .oneshot(get("/check", Some(&cookie)))
        .await
        .unwrap();
    let body = String::from_utf8(
        axum::body::to_bytes(check_response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert_eq!(body, "false");
}
