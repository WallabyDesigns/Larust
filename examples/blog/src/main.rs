use larust_core::Application;
use larust_http::{Route, Router};
use larust_support::auth::{redirect_authenticated, require_auth};

use blog::controllers::{AuthController, PostController};
use blog::events::PostCreated;
use blog::jobs::NotifyPostCreatedJob;

#[tokio::main]
async fn main() -> Result<(), larust_core::AppError> {
    let app = Application::new()?;
    let command = std::env::args().nth(1);

    if command.as_deref() == Some("migrate") {
        connect_database().await?;
        larust_support::orm::migrate(std::path::Path::new("database/migrations")).await?;
        return Ok(());
    }

    if command.as_deref() == Some("queue:work") {
        connect_database().await?;
        let registry = larust_support::queue::JobRegistry::new().register::<NotifyPostCreatedJob>();
        return larust_support::queue::work(registry).await;
    }

    let route = Route::get("/", index)
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

async fn index() -> &'static str {
    "Larust"
}
