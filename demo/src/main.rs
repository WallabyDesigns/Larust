use larust_core::Application;
use larust_http::{session::Session, Route, Router};
use larust_support::auth::{redirect_authenticated, require_auth};

use demo::controllers::{AuthController, PostController, ProfileController, UploadController};
use demo::events::PostCreated;
use demo::jobs::NotifyPostCreatedJob;
use demo::notifications::PostPublished;
use demo::wire_components::{PostForm, PostList};

mod seed;

/// Enforced by axum's `DefaultBodyLimit` layer on the `/uploads` route
/// (`main.rs`'s route table) — well above a real image's typical size,
/// still bounded so a client can't stream an unbounded body at the server.
const MAX_UPLOAD_BYTES: usize = 5 * 1024 * 1024;

#[tokio::main]
async fn main() -> Result<(), larust_core::AppError> {
    let app = Application::at_root(env!("CARGO_MANIFEST_DIR"))?;
    let command = std::env::args().nth(1);

    if command.as_deref() == Some("migrate") {
        connect_database(app.paths()).await?;
        larust_support::orm::migrate(&app.paths().migrations()).await?;
        return Ok(());
    }

    if command.as_deref() == Some("db:seed") {
        connect_database(app.paths()).await?;
        return seed::run().await;
    }

    if command.as_deref() == Some("queue:work") {
        connect_database(app.paths()).await?;
        let registry = larust_support::queue::JobRegistry::new().register::<NotifyPostCreatedJob>();
        return larust_support::queue::work(registry).await;
    }

    if command.as_deref() == Some("schedule:work") {
        connect_database(app.paths()).await?;
        let schedule = larust_support::schedule::Schedule::new().daily(|| async {
            let count = post_count().await?;
            larust_support::tracing::info!(post_count = count, "daily post count (scheduler demo)");
            Ok(())
        });
        return larust_support::schedule::work(schedule).await;
    }

    larust_support::wire::components()
        .register::<PostList>()
        .register::<PostForm>()
        .publish();

    let route = Route::get("/", index)
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

            // One event, three independently-composed channels: queued
            // (above), database (here), and live-pushed (below) — no
            // framework-level dispatch table, just ordinary calls at the
            // same call site. See docs/ARCHITECTURE.md's "Notifications"
            // section for why this crate doesn't unify the three itself.
            match demo::models::User::find(event.user_id).await {
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

            // The `@live("posts.count")` ticker on the home page — every
            // browser tab currently sitting on `/` sees the new count with
            // nobody in that tab doing anything at all, the one thing
            // neither `@wire(...)` nor a plain page reload can express.
            match post_count().await {
                Ok(count) => {
                    let fragment =
                        larust_support::view!("components.post-count-ticker", { count })
                            .into_html();
                    larust_support::push::broadcast(
                        "posts.count",
                        larust_support::push::wrap("posts.count", &fragment),
                    );
                }
                Err(error) => {
                    larust_support::tracing::warn!(%error, "failed to re-query post count for the live ticker broadcast");
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

async fn index(
    session: Session,
) -> Result<impl larust_support::axum::response::IntoResponse, larust_core::AppError> {
    let csrf_token = larust_http::csrf::token(&session).await;
    let is_authenticated = larust_support::auth::check(&session).await?;
    let nav_active = "home";
    let count = post_count().await?;
    Ok(larust_support::view!("welcome", { csrf_token, is_authenticated, nav_active, count }))
}

/// The live-updating count `@live("posts.count")` on the home page shows —
/// shared by `index()`'s own initial render and the `PostCreated` listener
/// below, which re-queries and broadcasts a fresh count through the exact
/// same `components.post-count-ticker` template so the two can never drift
/// out of the shape the client's DOM patcher expects.
async fn post_count() -> Result<i64, larust_core::AppError> {
    let (count,): (i64,) = larust_support::orm::sqlx::query_as("SELECT COUNT(*) FROM posts")
        .fetch_one(larust_support::orm::pool()?)
        .await
        .map_err(|error| larust_core::AppError::Internal(Box::new(error)))?;
    Ok(count)
}
