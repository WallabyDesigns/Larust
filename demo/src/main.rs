use larust_core::Application;
use larust_http::{session::Session, Route, Router};
use larust_support::auth::{redirect_authenticated, require_auth};

use demo::controllers::{AuthController, PostController, ProfileController, UploadController};
use demo::events::PostCreated;
use demo::jobs::NotifyPostCreatedJob;
use demo::live_components::{PostForm, PostList};

mod seed;

/// Enforced by axum's `DefaultBodyLimit` layer on the `/uploads` route
/// (`main.rs`'s route table) — well above a real image's typical size,
/// still bounded so a client can't stream an unbounded body at the server.
const MAX_UPLOAD_BYTES: usize = 5 * 1024 * 1024;

#[tokio::main]
async fn main() -> Result<(), larust_core::AppError> {
    let app = Application::new()?;
    let command = std::env::args().nth(1);

    if command.as_deref() == Some("migrate") {
        connect_database().await?;
        larust_support::orm::migrate(std::path::Path::new("database/migrations")).await?;
        return Ok(());
    }

    if command.as_deref() == Some("db:seed") {
        connect_database().await?;
        return seed::run().await;
    }

    if command.as_deref() == Some("queue:work") {
        connect_database().await?;
        let registry = larust_support::queue::JobRegistry::new().register::<NotifyPostCreatedJob>();
        return larust_support::queue::work(registry).await;
    }

    larust_support::live::components()
        .register::<PostList>()
        .register::<PostForm>()
        .publish();

    let route = Route::get("/", index)
        .get("/posts", PostController::index)
        .name("posts.index")
        .get("/posts/{post}", PostController::show)
        .name("posts.show")
        .get(
            "/__larust_live/runtime.js",
            larust_support::live::runtime_js,
        )
        .post(
            "/__larust_live/{component_id}",
            larust_support::live::update,
        )
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
        .middleware(larust_http::axum::middleware::from_fn(
            larust_http::csrf::verify,
        ));

    if command.as_deref() == Some("route:list") {
        print_routes(&route);
        return Ok(());
    }

    connect_database().await?;
    let route = route
        .with_sessions(
            larust_support::orm::pool()?,
            app.config().session_secure_cookie,
        )
        .await?;

    // Decouples "a post was created" from "notify about it" —
    // `PostController::store` only knows about `PostCreated`, not about
    // `NotifyPostCreatedJob`. Registered once, here, before serving.
    larust_support::event::listeners()
        .on::<PostCreated, _, _>(|event: PostCreated| async move {
            larust_support::tracing::info!(post_id = event.post_id, title = %event.title, "post created");
            if let Err(error) =
                larust_support::queue::dispatch(&NotifyPostCreatedJob { post_id: event.post_id })
                    .await
            {
                larust_support::tracing::warn!(%error, post_id = event.post_id, "failed to enqueue post-created notification");
            }
        })
        .publish();

    app.router(route.into_axum_router()).serve().await
}

async fn connect_database() -> Result<(), larust_core::AppError> {
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "sqlite://database/database.sqlite".to_string());
    larust_support::orm::connect(&database_url).await
}

fn print_routes(route: &Router) {
    for info in route.routes() {
        println!(
            "{:<7} {:<24} {}",
            info.method,
            info.path,
            info.name.as_deref().unwrap_or("")
        );
    }
}

async fn index(
    session: Session,
) -> Result<impl larust_support::axum::response::IntoResponse, larust_core::AppError> {
    let csrf_token = larust_http::csrf::token(&session).await;
    let is_authenticated = larust_support::auth::check(&session).await?;
    let nav_active = "home";
    Ok(larust_support::view!("welcome", { csrf_token, is_authenticated, nav_active }))
}
