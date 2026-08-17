//! Laravel's `routes/web.php` equivalent — every browser-facing route,
//! CSRF-protected as a whole (see the trailing `.middleware(csrf::verify)`
//! below). Read `main.rs` for how this gets composed with `routes/api.rs`
//! and served.

use crate::controllers::{
    AuthController, NotificationController, PostController, ProfileController, UploadController,
};
use larust_http::session::Session;
use larust_http::{Route, Router};
use larust_support::auth::{redirect_authenticated, require_auth};

/// Enforced by axum's `DefaultBodyLimit` layer on the `/uploads` route
/// below — well above a real image's typical size, still bounded so a
/// client can't stream an unbounded body at the server.
const MAX_UPLOAD_BYTES: usize = 5 * 1024 * 1024;

pub fn routes() -> Router {
    Route::get("/", index)
        .get("/posts", PostController::index)
        .name("posts.index")
        .get("/posts/{post}", PostController::show)
        .name("posts.show")
        .get(
            "/__larust_wire/runtime.js",
            larust_support::wire::runtime_js,
        )
        .post(
            "/__larust_wire/{component_id}",
            larust_support::wire::update,
        )
        .get(
            "/__larust_push/runtime.js",
            larust_support::push::runtime_js,
        )
        .get("/__larust_push/{channel}", larust_support::push::socket)
        // Creating a post requires login (Laravel's
        // `Route::middleware('auth')->group(...)`) — group-scoped
        // middleware only wraps the routes registered inside this closure,
        // it never affects the read-only routes above.
        .group("", |r: Router| {
            r.middleware(larust_http::axum::middleware::from_fn(require_auth))
                .get("/posts/create", PostController::create)
                .name("posts.create")
                .post("/posts", PostController::store)
                .name("posts.store")
                .get("/posts/{post}/edit", PostController::edit)
                .name("posts.edit")
                .post("/posts/{post}/update", PostController::update)
                .name("posts.update")
                .post("/posts/{post}/delete", PostController::destroy)
                .name("posts.destroy")
                .get("/profile", ProfileController::show)
                .name("profile")
                .post("/profile", ProfileController::update)
                .name("profile.update")
                .post("/profile/password", ProfileController::update_password)
                .name("profile.password")
                .get("/notifications", NotificationController::index)
                .name("notifications.index")
                .get("/notifications/drawer", NotificationController::drawer)
                .name("notifications.drawer")
                .post(
                    "/notifications/{id}/read",
                    NotificationController::mark_read,
                )
                .name("notifications.read")
                .post(
                    "/notifications/mark-all-read",
                    NotificationController::mark_all_read,
                )
                .name("notifications.read_all")
                .post("/notifications/{id}/clear", NotificationController::clear)
                .name("notifications.clear")
                .post(
                    "/notifications/clear-all",
                    NotificationController::clear_all,
                )
                .name("notifications.clear_all")
                // Nested so `DefaultBodyLimit` only scopes to this one
                // route, not every auth-gated route in the outer group —
                // same "group-scoped middleware composes by nesting"
                // pattern `docs/ARCHITECTURE.md` already documents.
                .group("", |r: Router| {
                    r.middleware(larust_http::axum::extract::DefaultBodyLimit::max(
                        MAX_UPLOAD_BYTES,
                    ))
                    .post("/uploads", UploadController::store)
                    .name("uploads.store")
                })
        })
        // The inverse: an already-logged-in user is bounced away from
        // register/login (Laravel's `guest` middleware).
        .group("", |r: Router| {
            r.middleware(larust_http::axum::middleware::from_fn(
                redirect_authenticated,
            ))
            .get("/register", AuthController::show_register)
            .name("register")
            .post("/register", AuthController::register)
            .name("register.store")
            .get("/login", AuthController::show_login)
            .name("login")
            .post("/login", AuthController::login)
            .name("login.store")
        })
        .post("/logout", AuthController::logout)
        .name("logout")
        // Applied here, not in `main.rs` — CSRF is a web-routes-only
        // concern (it protects cookie-authenticated browser form
        // submissions), so it must never end up folded onto `routes/api.rs`'s
        // entries too. `.group("/api", ...)` in `main.rs` merges this
        // router's own `.middleware()` list onto every entry already in it
        // at merge time, so keeping this call inside `routes::web::routes()`
        // itself (rather than a top-level call in `main.rs` after both are
        // combined) is what keeps it scoped to web routes only.
        .middleware(larust_http::axum::middleware::from_fn(
            larust_http::csrf::verify,
        ))
}

async fn index(
    session: Session,
) -> Result<impl larust_support::axum::response::IntoResponse, larust_core::AppError> {
    let csrf_token = larust_http::csrf::token(&session).await;
    let is_authenticated = larust_support::auth::check(&session).await?;
    let unread_count = crate::controllers::unread_count_for(&session).await?;
    let nav_active = "home";
    let count = post_count().await?;
    Ok(
        larust_support::view!("welcome", { csrf_token, is_authenticated, unread_count, nav_active, count }),
    )
}

/// The live-updating count `@live("posts.count")` on the home page shows —
/// shared by `index()`'s own initial render and `main.rs`'s `PostCreated`
/// listener (which re-queries and broadcasts a fresh count) and
/// `routes::console::schedule()`'s daily log line, through the exact same
/// `components.post-count-ticker` template so none of the three can ever
/// drift out of the shape the client's DOM patcher expects.
pub async fn post_count() -> Result<i64, larust_core::AppError> {
    let (count,): (i64,) = larust_support::orm::sqlx::query_as("SELECT COUNT(*) FROM posts")
        .fetch_one(larust_support::orm::pool()?)
        .await
        .map_err(|error| larust_core::AppError::Internal(Box::new(error)))?;
    Ok(count)
}
