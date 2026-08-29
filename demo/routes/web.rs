//! Laravel's `routes/web.php` equivalent — every browser-facing route,
//! CSRF-protected as a whole (see the trailing `.middleware(csrf::verify)`
//! below). Read `main.rs` for how this gets composed with `routes/api.rs`
//! and served.

use crate::controllers::{
    AuthController, CommentController, NotificationController, PostController, ProfileController,
    UploadController,
};
use larust_http::session::Session;
use larust_http::{Route, Router};
use larust_support::auth::{redirect_authenticated, require_auth};
use larust_support::preferences::CookieJar;

/// Enforced by axum's `DefaultBodyLimit` layer on the `/uploads` route
/// below — well above a real image's typical size, still bounded so a
/// client can't stream an unbounded body at the server.
const MAX_UPLOAD_BYTES: usize = 5 * 1024 * 1024;

pub fn routes() -> Router {
    Route::get("/", index)
        // Nested in its own group (same "scope a single route's middleware"
        // pattern the `/uploads` group below uses) rather than a top-level
        // `.middleware()` call, which would cover every route on this
        // router — `/sitemap.xml` is the one page in this app with no
        // per-viewer state at all (no CSRF token, no auth status), so it's
        // the one page actually safe to cache; see its own doc comment and
        // `docs/GOTCHAS.md` for why every other page here isn't.
        .group("", |r: Router| {
            r.middleware(larust_http::responsecache::for_minutes(60))
                .get("/sitemap.xml", sitemap)
        })
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
        .get(
            "/__larust_reverb/runtime.js",
            larust_support::reverb::runtime_js,
        )
        .get("/__larust_reverb/{channel}", larust_support::reverb::socket)
        .get("/__larust_spa/runtime.js", larust_support::spa::runtime_js)
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
                .post("/posts/{post}/comments", CommentController::store)
                .name("posts.comments.store")
                .post(
                    "/posts/{post}/comments/{comment}/delete",
                    CommentController::destroy,
                )
                .name("posts.comments.destroy")
                .post("/posts/{post}/comments/typing", CommentController::typing)
                .name("posts.comments.typing")
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
        // CSRF is a web-routes-only concern (it protects cookie-
        // authenticated browser form submissions) — it must never reach
        // `routes/api.rs`'s entries. That isolation comes from `main.rs`
        // combining this router with `routes::api::routes()` via
        // `Router::merge` (not `.group`, which deliberately shares a
        // parent's top-level middleware with whatever it registers — the
        // wrong tool here, and the source of a real bug once: see
        // `docs/GOTCHAS.md`) — this call itself doesn't need to know or
        // care where in the chain it sits relative to that.
        .middleware(larust_http::axum::middleware::from_fn(
            larust_http::csrf::verify,
        ))
}

async fn index(
    session: Session,
    cookies: CookieJar,
) -> Result<impl larust_support::axum::response::IntoResponse, larust_core::AppError> {
    let csrf_token = larust_http::csrf::token(&session).await;
    let is_authenticated = larust_support::auth::check(&session).await?;
    let unread_count = crate::controllers::unread_count_for(&session).await?;
    let nav_active = "home";
    let count = post_count().await?;
    Ok(
        larust_support::view!("welcome", { cookies: &cookies, csrf_token, is_authenticated, unread_count, nav_active, count }),
    )
}

/// Path prefixes `sitemap()` below excludes, for two distinct reasons
/// `RouteInfo` (what `larust_support::sitemap::from_static_routes` filters
/// on) has no way to tell apart by itself, since it only carries
/// method/path/name:
/// - `/posts/create`, `/profile`, `/notifications` sit inside `routes()`'s
///   own `require_auth`-gated `.group(...)` — an unauthenticated crawler
///   hitting one would just get redirected to `/login`, not real content.
/// - `/__larust_wire`, `/__larust_push`, `/__larust_reverb`, `/__larust_spa`
///   are framework-internal JS asset/WebSocket routes (the wire/live/
///   reverb/spa runtime scripts and the reverb socket endpoint), not pages
///   meant for a search index at all.
///
/// See `docs/GOTCHAS.md` if this list and `routes()`'s own group
/// membership ever drift apart.
const EXCLUDED_FROM_SITEMAP_PATH_PREFIXES: &[&str] = &[
    "/posts/create",
    "/profile",
    "/notifications",
    "/__larust_wire",
    "/__larust_push",
    "/__larust_reverb",
    "/__larust_spa",
];

/// `GET /sitemap.xml` — `larust_support::sitemap::from_static_routes`
/// covers every static, public `GET` page (`/`, `/posts`, `/login`, ...)
/// discovered straight from this router's own route table (minus whatever
/// [`EXCLUDED_FROM_SITEMAP_PATH_PREFIXES`] excludes); per-post URLs
/// are added by hand below since `larust-sitemap` has no visibility into
/// this app's own `Post` model. Rebuilds `routes()` fresh on every request
/// rather than threading a cached route list through app state — cheap
/// (registering axum's route table involves no I/O), and it means the
/// sitemap can never drift from whatever routes are actually live.
/// Wrapped in `larust_http::responsecache::for_minutes(60)` (see
/// `routes()`'s own `.group(...)` above) — `larust-sitemap` itself owns no
/// caching (see its own doc comment for why), but unlike every other page
/// in this app, this response has no per-viewer state (no CSRF token, no
/// auth status) baked into it, so it's actually safe to cache here — the
/// one exception to `docs/GOTCHAS.md`'s own responsecache warning.
async fn sitemap() -> impl larust_support::axum::response::IntoResponse {
    let public_routes: Vec<_> = routes()
        .routes()
        .into_iter()
        .filter(|route| {
            !EXCLUDED_FROM_SITEMAP_PATH_PREFIXES
                .iter()
                .any(|prefix| route.path.starts_with(prefix))
        })
        .collect();
    let mut entries =
        larust_support::sitemap::from_static_routes(&larust_support::url(""), &public_routes);

    match crate::models::Post::all().await {
        Ok(posts) => {
            for post in posts {
                entries.push(
                    larust_support::sitemap::SitemapEntry::new(larust_support::url(&format!(
                        "/posts/{}",
                        post.id
                    )))
                    .change_freq(larust_support::sitemap::ChangeFreq::Weekly),
                );
            }
        }
        Err(error) => {
            larust_support::tracing::warn!(%error, "failed to list posts for sitemap.xml");
        }
    }

    larust_support::sitemap::response(&entries)
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
