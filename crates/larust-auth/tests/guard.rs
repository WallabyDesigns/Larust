//! End-to-end coverage for `guard.rs`/`extractor.rs`/`middleware.rs` against
//! a real (in-memory) session store and a real axum router - these three
//! modules have no unit tests of their own since they're only meaningful
//! wired together through a request/response cycle. Also serves as the
//! first real proof that `Authenticatable::find_for_auth`'s `-> impl
//! Future<...> + Send` signature (see `authenticatable.rs`'s doc comment)
//! is actually implementable with a plain `async fn` in the impl, as
//! claimed.

use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, StatusCode};
use larust_auth::{
    check, check_for, login, logout, logout_for, require_auth, require_auth_for, Auth,
    Authenticatable,
};
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

// A plain `async fn` implementation - the exact shape the doc comment on
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

#[derive(Clone, Debug, PartialEq)]
struct AdminUser {
    id: i64,
    name: String,
}

impl Authenticatable for AdminUser {
    const GUARD: &'static str = "admin";

    fn auth_id(&self) -> i64 {
        self.id
    }

    async fn find_for_auth(id: i64) -> Result<Option<Self>, AppError> {
        Ok(fake_admins().lock().unwrap().get(&id).cloned())
    }
}

fn fake_admins() -> &'static Mutex<HashMap<i64, AdminUser>> {
    static ADMINS: OnceLock<Mutex<HashMap<i64, AdminUser>>> = OnceLock::new();
    ADMINS.get_or_init(|| {
        let mut admins = HashMap::new();
        admins.insert(
            1,
            AdminUser {
                id: 1,
                name: "Root".to_string(),
            },
        );
        Mutex::new(admins)
    })
}

async fn do_admin_login(session: Session) -> &'static str {
    let admin = fake_admins().lock().unwrap().get(&1).cloned().unwrap();
    login(&session, &admin).await.unwrap();
    "admin logged in"
}

async fn do_admin_logout(session: Session) -> &'static str {
    logout_for::<AdminUser>(&session).await.unwrap();
    "admin logged out"
}

async fn do_admin_check(session: Session) -> String {
    check_for::<AdminUser>(&session).await.unwrap().to_string()
}

async fn admin_whoami(Auth(admin): Auth<AdminUser>) -> String {
    admin.name
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
        })
        .post("/admin/login", do_admin_login)
        .name("admin.login")
        .post("/admin/logout", do_admin_logout)
        .get("/admin/check", do_admin_check)
        .get("/admin/whoami", admin_whoami)
        .group("", |r: Router| {
            r.middleware(axum::middleware::from_fn(require_auth_for::<AdminUser>))
                .get("/admin/dashboard", || async { "admin dashboard" })
        });
    let dir = tempfile::tempdir().unwrap().keep();
    let database_url = format!("sqlite://{}/test.sqlite", dir.display());
    larust_orm::connect(&database_url).await.unwrap();
    let pool = larust_orm::pool().unwrap().clone();
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

    // Log in - capture the post-login session cookie (login() rotates the
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

    // Log out - the same cookie should no longer be treated as
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

    // Two guards, one session: log back in as the web user, then log in as
    // the admin guard too - each login rotates the session id, so the
    // cookie changes each time and every later request must use the
    // latest one.
    let login_response = router.clone().oneshot(post("/login", None)).await.unwrap();
    let cookie = session_cookie(&login_response);
    let admin_login_response = router
        .clone()
        .oneshot(post("/admin/login", Some(&cookie)))
        .await
        .unwrap();
    let cookie = session_cookie(&admin_login_response);

    // Both guards are independently authenticated on the same session.
    assert_eq!(
        response_body(
            router
                .clone()
                .oneshot(get("/check", Some(&cookie)))
                .await
                .unwrap()
        )
        .await,
        "true"
    );
    assert_eq!(
        response_body(
            router
                .clone()
                .oneshot(get("/admin/check", Some(&cookie)))
                .await
                .unwrap()
        )
        .await,
        "true"
    );
    assert_eq!(
        router
            .clone()
            .oneshot(get("/dashboard", Some(&cookie)))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        router
            .clone()
            .oneshot(get("/admin/dashboard", Some(&cookie)))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        response_body(
            router
                .clone()
                .oneshot(get("/admin/whoami", Some(&cookie)))
                .await
                .unwrap()
        )
        .await,
        "Root"
    );

    // Logging out just the admin guard doesn't touch the web guard's own
    // login state - the exact bug this feature exists to fix.
    let _ = router
        .clone()
        .oneshot(post("/admin/logout", Some(&cookie)))
        .await
        .unwrap();
    assert_eq!(
        response_body(
            router
                .clone()
                .oneshot(get("/admin/check", Some(&cookie)))
                .await
                .unwrap()
        )
        .await,
        "false"
    );
    assert_eq!(
        response_body(
            router
                .clone()
                .oneshot(get("/check", Some(&cookie)))
                .await
                .unwrap()
        )
        .await,
        "true",
        "logging out the admin guard must not affect the web guard's own session"
    );
    let admin_dashboard_response = router
        .clone()
        .oneshot(get("/admin/dashboard", Some(&cookie)))
        .await
        .unwrap();
    assert!(admin_dashboard_response.status().is_redirection());
    assert_eq!(
        admin_dashboard_response
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok()),
        Some("/admin/login"),
        "require_auth_for::<AdminUser> should redirect to the named `admin.login` route's path"
    );
    assert_eq!(
        router
            .clone()
            .oneshot(get("/dashboard", Some(&cookie)))
            .await
            .unwrap()
            .status(),
        StatusCode::OK,
        "the web guard should still be reachable after the admin guard alone logged out"
    );
}

async fn response_body(response: axum::response::Response) -> String {
    String::from_utf8(
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}
