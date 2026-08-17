use larust_core::Application;
use larust_http::Router;

use demo::events::PostCreated;
use demo::jobs::NotifyPostCreatedJob;
use demo::mail::PostPublishedMail;
use demo::notifications::PostPublished;
use demo::wire_components::{PostForm, PostList};

mod seed;

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
        return larust_support::schedule::work(demo::routes::console::schedule()).await;
    }

    larust_support::wire::components()
        .register::<PostList>()
        .register::<PostForm>()
        .publish();

    let route = demo::routes::web::routes().group(&app.config().api_prefix, |_r: Router| {
        demo::routes::api::routes()
    });

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

                    if let Err(error) = larust_support::mail::mail()
                        .to(&author.email)
                        .send(PostPublishedMail {
                            author: &author,
                            post_title: &event.title,
                            post_id: event.post_id,
                        })
                        .await
                    {
                        larust_support::tracing::warn!(%error, post_id = event.post_id, "failed to send post-published email");
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
            match demo::routes::web::post_count().await {
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
