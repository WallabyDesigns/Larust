//! Proves `TestClient`/`acting_as` actually drive a real router end to
//! end - the same scenario `larust-auth/tests/guard.rs` covers by hand,
//! rewritten against this crate to demonstrate what it eliminates.

use axum::http::StatusCode;
use larust_core::AppError;
use larust_http::{Route, Router};
use larust_support::auth::{require_auth, Auth, Authenticatable};
use larust_testing::TestClient;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Debug, PartialEq)]
struct TestUser {
    id: i64,
    name: String,
}

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

async fn whoami(Auth(user): Auth<TestUser>) -> String {
    user.name
}

/// Every test in this file shares one process-wide pool -
/// `larust_orm::connect()` is a real once-per-process singleton, so the
/// first call here wins and every later call's "already connected" error
/// is deliberately swallowed. A real temp-file database, not
/// `sqlite::memory:`: a pool can open more than one physical connection,
/// and pooled `:memory:` connections each get their own private, empty
/// database without explicit shared-cache URI mode.
async fn shared_pool() -> sqlx::AnyPool {
    let dir = tempfile::tempdir().unwrap().keep();
    let database_url = format!("sqlite://{}/test.sqlite", dir.display());
    let _ = larust_orm::connect(&database_url).await;
    larust_orm::pool().unwrap().clone()
}

async fn build_router(pool: &sqlx::AnyPool) -> axum::Router {
    Route::get("/whoami", whoami)
        .name("whoami")
        .group("", |r: Router| {
            r.middleware(axum::middleware::from_fn(require_auth))
                .get("/dashboard", || async { "dashboard" })
        })
        .with_sessions(pool, true)
        .await
        .unwrap()
        .into_axum_router()
}

#[tokio::test]
async fn acting_as_authenticates_the_client_for_every_later_request() {
    let pool = shared_pool().await;
    let router = build_router(&pool).await;
    let mut client = TestClient::new(router, &pool);

    client
        .get("/whoami")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);

    let alice = fake_users().lock().unwrap().get(&1).cloned().unwrap();
    client.acting_as(&alice).await.unwrap();

    client
        .get("/whoami")
        .await
        .assert_status(StatusCode::OK)
        .assert_body_contains("Alice");

    // The adopted cookie also satisfies `require_auth`-gated routes, not
    // just the `Auth<U>` extractor.
    client.get("/dashboard").await.assert_status(StatusCode::OK);
}

#[tokio::test]
async fn a_fresh_client_is_independent_of_another_clients_acting_as() {
    let pool = shared_pool().await;
    let router = build_router(&pool).await;

    let mut alice_client = TestClient::new(router.clone(), &pool);
    let alice = fake_users().lock().unwrap().get(&1).cloned().unwrap();
    alice_client.acting_as(&alice).await.unwrap();
    alice_client
        .get("/whoami")
        .await
        .assert_status(StatusCode::OK);

    // A second `TestClient` built from the same (cheaply cloned) router is
    // its own independent actor - it never saw Alice's cookie.
    let mut anonymous_client = TestClient::new(router, &pool);
    anonymous_client
        .get("/whoami")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}
