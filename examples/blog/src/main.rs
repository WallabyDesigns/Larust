use larust_core::Application;
use larust_http::Router;

use blog::events::PostCreated;
use blog::jobs::NotifyPostCreatedJob;
use blog::notifications::PostPublished;

#[tokio::main]
async fn main() -> Result<(), larust_core::AppError> {
    let app = Application::at_root(env!("CARGO_MANIFEST_DIR"), blog::config::app::config)?;
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
        return larust_support::schedule::work(blog::routes::console::schedule()).await;
    }

    // `.merge`, not `.group` — keeps `routes::api`'s own middleware stack
    // independent of `routes::web`'s (CSRF among others); see
    // `Router::merge`'s own doc comment and `docs/GOTCHAS.md`.
    let route =
        blog::routes::web::routes().merge(&app.config().api_prefix, blog::routes::api::routes());

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
    let database_url = database_url(paths)?;
    larust_support::orm::connect(&database_url).await
}

/// Resolves `config/database.rs`'s active connection to the URL
/// `larust_support::orm::connect()` needs — with one extra step
/// `ConnectionConfig::to_url()` itself can't do: a *relative* sqlite path
/// (`config/database.rs`'s own default, `database/database.sqlite`) needs
/// resolving against this app's own root (`AppPaths`), not the process's
/// current working directory. An absolute path or `:memory:` is left
/// untouched.
fn database_url(paths: &larust_core::AppPaths) -> Result<String, larust_core::AppError> {
    let url = blog::config::database::config().default_connection_url()?;
    let relative_sqlite_path = url
        .strip_prefix("sqlite://")
        .filter(|path| !path.starts_with('/') && !path.starts_with(":memory:"))
        .map(str::to_string);

    Ok(match relative_sqlite_path {
        Some(relative) => {
            let path = paths.join(relative);
            format!("sqlite:///{}", path.to_string_lossy().replace('\\', "/"))
        }
        None => url,
    })
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
