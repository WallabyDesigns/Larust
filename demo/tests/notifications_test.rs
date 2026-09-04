//! End-to-end proof that creating a post actually notifies its author
//! through both channels this test cares about: a `PostPublishedMail`
//! (asserted via `larust_testing::fake()`/`assert_sent`, mirroring
//! `mail_test.rs`'s own pattern) and a database notification the shared
//! header drawer can list and clear.
//!
//! Registers its own scoped-down copy of just the mail+notify half of
//! `src/main.rs`'s real `PostCreated` listener - the same "duplicate only
//! what this test needs" convention `live_ticker_test.rs` already
//! established for the push-broadcast half; there's no shared, testable
//! listener function to call into instead, by design (the real one is a
//! closure registered once in `main()`).

use demo::controllers::{AuthController, NotificationController, PostController};
use demo::events::PostCreated;
use demo::mail::PostPublishedMail;
use demo::models::User;
use demo::notifications::PostPublished;
use demo::wire_components::PostForm;
use larust_http::Route;
use larust_support::axum::http::StatusCode;
use larust_support::notification::{notifications_for, unread_count};
use larust_testing::TestClient;
use std::sync::Once;

static REGISTER_ONCE: Once = Once::new();

fn ensure_registered() {
    REGISTER_ONCE.call_once(|| {
        larust_support::wire::components()
            .register::<PostForm>()
            .publish();
        larust_support::event::listeners()
            .on::<PostCreated, _, _>(|event: PostCreated| async move {
                let Ok(Some(author)) = User::find(event.user_id).await else {
                    return;
                };
                let _ = larust_support::notification::notify(
                    &author,
                    &PostPublished {
                        post_id: event.post_id,
                        title: event.title.clone(),
                    },
                )
                .await;
                let _ = larust_support::mail::mail()
                    .to(&author.email)
                    .send(PostPublishedMail {
                        author: &author,
                        post_title: &event.title,
                        post_id: event.post_id,
                    })
                    .await;
            })
            .publish();
    });
}

async fn build_router(pool: &sqlx::AnyPool) -> larust_support::axum::Router {
    ensure_registered();
    Route::get("/posts", PostController::index)
        .name("posts.index")
        .get("/posts/create", PostController::create)
        .post("/posts", PostController::store)
        .name("posts.store")
        .get("/register", AuthController::show_register)
        .name("register")
        .post("/register", AuthController::register)
        .name("register.store")
        .get("/notifications", NotificationController::index)
        .name("notifications.index")
        .get("/notifications/drawer", NotificationController::drawer)
        .name("notifications.drawer")
        .post(
            "/notifications/{id}/read",
            NotificationController::mark_read,
        )
        .name("notifications.read")
        .post("/notifications/{id}/clear", NotificationController::clear)
        .name("notifications.clear")
        .middleware(larust_http::axum::middleware::from_fn(
            larust_http::csrf::verify,
        ))
        .with_sessions(pool, false)
        .await
        .unwrap()
        .into_axum_router()
}

#[tokio::test]
async fn creating_a_post_emails_and_notifies_its_author_and_the_drawer_shows_it() {
    larust_core::Application::new(demo::config::app::config).unwrap();
    larust_testing::fake();

    let pool = larust_testing::test_db(std::path::Path::new("database/migrations"))
        .await
        .unwrap();
    let router = build_router(&pool).await;
    let mut client = TestClient::new(router, &pool);

    let csrf_token = client
        .get("/posts/create")
        .await
        .csrf_token()
        .expect("create page should render a CSRF token");
    client
        .post_form(
            "/register",
            &[
                ("_csrf_token", &csrf_token),
                ("name", "Alice"),
                ("email", "alice-notifications@example.com"),
                ("password", "password123"),
                ("password_confirmation", "password123"),
            ],
        )
        .await
        .assert_status(StatusCode::SEE_OTHER);

    let csrf_token = client
        .get("/posts/create")
        .await
        .csrf_token()
        .expect("create page should render a CSRF token");
    client
        .post_form(
            "/posts",
            &[
                ("_csrf_token", &csrf_token),
                ("title", "Hello, Notifications"),
                ("content", "<p>First post</p>"),
                ("tags", ""),
            ],
        )
        .await
        .assert_status(StatusCode::SEE_OTHER);

    // The real proof for mail: a `PostPublishedMail`, addressed to the
    // author, with a subject containing the post's own title.
    larust_testing::assert_sent::<PostPublishedMail<'_>>(|sent| {
        sent.to == vec!["alice-notifications@example.com".to_string()]
            && sent.subject.contains("Hello, Notifications")
    });

    let (user_id,): (i64,) = sqlx::query_as("SELECT id FROM users WHERE email = ?")
        .bind("alice-notifications@example.com")
        .fetch_one(&pool)
        .await
        .unwrap();
    let author = User::find(user_id).await.unwrap().unwrap();

    // The real proof for the database channel: a stored notification,
    // unread, carrying the post's own title.
    let stored = notifications_for(&author, 10).await.unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].notification_type, "post_published");
    assert_eq!(stored[0].data["title"], "Hello, Notifications");
    assert_eq!(unread_count(&author).await.unwrap(), 1);

    // The shared drawer fragment actually shows it - not just an invisible row.
    let drawer = client.get("/notifications/drawer").await;
    drawer.assert_status(StatusCode::OK);
    assert!(
        drawer.body().contains("Hello, Notifications"),
        "drawer should show the new post's title: {}",
        drawer.body()
    );

    // Clearing the notification through its line action removes it and
    // therefore drops the unread badge count.
    let csrf_token = client
        .get("/posts/create")
        .await
        .csrf_token()
        .expect("page shell should render a CSRF token for drawer forms");
    client
        .post_form(
            &format!("/notifications/{}/clear", stored[0].id),
            &[("_csrf_token", &csrf_token)],
        )
        .await
        .assert_status(StatusCode::SEE_OTHER);
    assert_eq!(unread_count(&author).await.unwrap(), 0);
    assert!(notifications_for(&author, 10).await.unwrap().is_empty());
}
