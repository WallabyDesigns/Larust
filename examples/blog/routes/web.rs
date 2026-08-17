//! Laravel's `routes/web.php` equivalent — every browser-facing route,
//! CSRF-protected as a whole (see the trailing `.middleware(csrf::verify)`
//! below). Read `main.rs` for how this gets composed with `routes/api.rs`
//! and served.

use crate::controllers::{AuthController, PostController};
use larust_http::{Route, Router};
use larust_support::auth::{redirect_authenticated, require_auth};

pub fn routes() -> Router {
    Route::get("/", index)
        .get("/posts", PostController::index)
        .name("posts.index")
        .get("/posts/{post}", PostController::show)
        .name("posts.show")
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
        // entries too. See `demo`'s own `routes/web.rs` for the fuller
        // explanation of why this call has to live here.
        .middleware(larust_http::axum::middleware::from_fn(
            larust_http::csrf::verify,
        ))
}

async fn index() -> &'static str {
    "Larust"
}
