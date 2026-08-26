//! Regression test for the M31 Storage refactor of `UploadController` —
//! there was no test for the upload flow at all before this (confirmed via
//! `grep` across `demo/tests/` and `examples/blog/tests/`). Exercises the
//! real, CSRF-protected `/uploads` route end to end and confirms the file
//! actually lands on disk via `larust_support::storage::public()`, not
//! just that the handler returned 200.
//!
//! Deliberately a single `#[tokio::test]` fn, not several — matching
//! `larust-testing/tests/db_test.rs`'s own established reasoning:
//! `cargo test` doesn't guarantee execution order (or even non-overlap)
//! between separate test functions in the same file/binary.

use demo::controllers::{AuthController, PostController, UploadController};
use demo::wire_components::PostForm;
use larust_http::Route;
use larust_support::axum::http::StatusCode;
use larust_testing::TestClient;
use std::sync::Once;

/// The 8-byte PNG signature `bytes_match_extension` checks for, padded
/// with arbitrary bytes — `UploadController::store` never validates full
/// PNG structure, only this signature (see its own doc comments).
const FAKE_PNG_BYTES: &[u8] = &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3, 4];

// `/posts/create` (visited below only to fetch a CSRF token) renders
// `posts.create`, which mounts `@wire('post-form')` — must be registered in
// this file's own process-wide registry or `mount()` 500s.
static REGISTER_ONCE: Once = Once::new();

fn ensure_registered() {
    REGISTER_ONCE.call_once(|| {
        larust_support::wire::components()
            .register::<PostForm>()
            .publish();
    });
}

async fn build_router(pool: &sqlx::AnyPool) -> larust_support::axum::Router {
    ensure_registered();
    // `posts.index` is never actually visited by this test — it's only
    // here because `AuthController::register`'s success path redirects to
    // it by name, and `larust_support::redirect().route(name)` resolves
    // against this router's own name registry (same gotcha
    // `posts_policy_test.rs`'s own `build_router` comment documents).
    Route::get("/posts", PostController::index)
        .name("posts.index")
        .get("/posts/create", PostController::create)
        .get("/register", AuthController::show_register)
        .name("register")
        .post("/register", AuthController::register)
        .name("register.store")
        .post("/uploads", UploadController::store)
        .name("uploads.store")
        .middleware(larust_http::axum::middleware::from_fn(
            larust_http::csrf::verify,
        ))
        .with_sessions(pool, false)
        .await
        .unwrap()
        .into_axum_router()
}

/// Best-effort cleanup for a file this test wrote into the real, tracked
/// `public/uploads/` directory (there's no scratch-root override for
/// `storage::public()` yet). A plain `delete()` call at the end of the
/// test only runs if every earlier assertion passes — an assertion
/// failure between the upload and that call would otherwise panic and
/// leave the file behind, polluting the repo's working tree for every
/// run after. `Drop` can't be `async`, so this uses a blocking
/// `std::fs::remove_file` directly; errors are ignored the same way
/// `Disk::delete` treats an already-missing file as a non-error.
struct CleanupOnDrop(String);

impl Drop for CleanupOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(std::path::Path::new("public").join(&self.0));
    }
}

async fn csrf_token_for(client: &mut TestClient) -> String {
    client
        .get("/posts/create")
        .await
        .csrf_token()
        .expect("create page should render a CSRF token")
}

#[tokio::test]
async fn upload_flow_stores_valid_images_and_rejects_non_images() {
    // `Application::new()` populates `larust_core::config()` — required by
    // `AuthController::register`'s welcome-mail send (see
    // `posts_policy_test.rs` for the same requirement/reasoning).
    larust_core::Application::new(demo::config::app::config).unwrap();

    let pool = larust_testing::test_db(std::path::Path::new("database/migrations"))
        .await
        .unwrap();
    let router = build_router(&pool).await;
    let mut client = TestClient::new(router, &pool);

    let csrf_token = csrf_token_for(&mut client).await;
    client
        .post_form(
            "/register",
            &[
                ("_csrf_token", &csrf_token),
                ("name", "Uploader"),
                ("email", "uploader@example.com"),
                ("password", "password123"),
                ("password_confirmation", "password123"),
            ],
        )
        .await
        .assert_status(StatusCode::SEE_OTHER);

    // A non-image is rejected before ever reaching storage — same
    // pre-existing validation (`allowed_extension`), now just confirmed
    // under test for the first time.
    let csrf_token = csrf_token_for(&mut client).await;
    client
        .post_multipart(
            "/uploads",
            &csrf_token,
            "not-an-image.txt",
            "text/plain",
            b"just some text",
        )
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);

    // A valid image is stored for real.
    let csrf_token = csrf_token_for(&mut client).await;
    let response = client
        .post_multipart(
            "/uploads",
            &csrf_token,
            "photo.png",
            "image/png",
            FAKE_PNG_BYTES,
        )
        .await;
    response.assert_status(StatusCode::OK);

    let url = response
        .body()
        .split("\"url\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("response body should contain a url field")
        .to_string();
    assert!(
        url.starts_with("/uploads/"),
        "expected a /uploads/-prefixed url, got {url}"
    );

    // From here on, an assertion failure must not leave the uploaded file
    // behind in the tracked `public/uploads/` directory — see
    // `CleanupOnDrop`'s own doc comment.
    let storage_path = url.trim_start_matches('/').to_string();
    let _cleanup = CleanupOnDrop(storage_path.clone());

    // The real proof: the file is actually readable back from the same
    // disk `UploadController::store` wrote it to, not just that the
    // handler claimed success.
    let stored = larust_support::storage::public()
        .get(&storage_path)
        .await
        .unwrap();
    assert_eq!(stored, Some(FAKE_PNG_BYTES.to_vec()));

    // Exercises `delete()` for real on the success path — `CleanupOnDrop`
    // is the safety net for a failure between here and the assertion
    // above, not a substitute for this.
    larust_support::storage::public()
        .delete(&storage_path)
        .await
        .unwrap();
}
