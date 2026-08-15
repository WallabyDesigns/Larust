use larust_core::Application;
use larust_http::{Route, Router};
use larust_support::auth::{redirect_authenticated, require_auth};

use blog::controllers::{AuthController, PostController};
use blog::events::PostCreated;
use blog::jobs::NotifyPostCreatedJob;
use blog::notifications::PostPublished;

#[tokio::main]
async fn main() -> Result<(), larust_core::AppError> {
    let app = Application::at_root(env!("CARGO_MANIFEST_DIR"))?;
    let command = std::env::args().nth(1);

    if command.as_deref() == Some("migrate") {
        connect_database(app.paths()).await?;
        larust_support::orm::migrate(&app.paths().migrations()).await?;
        return Ok(());
    }

    if command.as_deref() == Some("queue:work") {
        connect_database(app.paths()).await?;
        let registry = larust_support::queue::JobRegistry::new().register::<NotifyPostCreatedJob>();
        return larust_support::queue::work(registry).await;
    }

    if command.as_deref() == Some("schedule:work") {
        connect_database(app.paths()).await?;
        let schedule = larust_support::schedule::Schedule::new().daily(|| async {
            let count = blog::models::Post::all().await?.len();
            larust_support::tracing::info!(post_count = count, "daily post count (scheduler demo)");
            Ok(())
        });
        return larust_support::schedule::work(schedule).await;
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

    connect_database(app.paths()).await?;
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

            // A second, independently-composed channel alongside the
            // queue dispatch above — no framework-level dispatch table,
            // just an ordinary call. See docs/ARCHITECTURE.md's
            // "Notifications" section.
            match blog::models::User::find(event.user_id).await {
                Ok(Some(author)) => {
                    if let Err(error) = larust_support::notification::notify(
                        &author,
                        &PostPublished {
                            post_id: event.post_id,
                            title: event.title.clone(),
                        },
                    )
                    .await
                    {
                        larust_support::tracing::warn!(%error, post_id = event.post_id, "failed to record post-published notification");
                    }
                }
                Ok(None) => {
                    larust_support::tracing::warn!(post_id = event.post_id, user_id = event.user_id, "post's author no longer exists");
                }
                Err(error) => {
                    larust_support::tracing::warn!(%error, post_id = event.post_id, "failed to look up post's author for notification");
                }
            }
        })
        .publish();

    app.with_health_route("/up")
        .router(route.into_axum_router())
        .serve()
        .await
}

async fn connect_database(paths: &larust_core::AppPaths) -> Result<(), larust_core::AppError> {
    let database_url = database_url(paths);
    larust_support::orm::connect(&database_url).await
}

fn database_url(paths: &larust_core::AppPaths) -> String {
    let configured = std::env::var("DATABASE_URL").ok();
    let relative_sqlite_path = configured
        .as_deref()
        .and_then(|url| url.strip_prefix("sqlite://"))
        .filter(|path| !path.starts_with('/') && !path.starts_with(":memory:"))
        .map(str::to_string);

    match relative_sqlite_path {
        Some(relative) => {
            let path = paths.join(relative);
            format!("sqlite:///{}", path.to_string_lossy().replace('\\', "/"))
        }
        None => match configured {
            Some(configured) => configured,
            None => {
                let path = paths.database().join("database.sqlite");
                format!("sqlite:///{}", path.to_string_lossy().replace('\\', "/"))
            }
        },
    }
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
