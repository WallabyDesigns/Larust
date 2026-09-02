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
    let app = Application::at_root(env!("CARGO_MANIFEST_DIR"), demo::config::app::config)?;
    let command = std::env::args().nth(1);

    if command.as_deref() == Some("migrate") {
        connect_database(app.paths()).await?;
        larust_support::orm::migrate(&app.paths().migrations()).await?;
        return Ok(());
    }

    if command.as_deref() == Some("migrate:fresh") {
        connect_database(app.paths()).await?;
        larust_support::orm::migrate_fresh(&app.paths().migrations()).await?;
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

    // larust-db's embedded key-value store — a separate subsystem from the
    // `db:seed` command above despite the shared `db:` prefix ("db:seed"
    // predates this and refers to the SQL database this app's models live
    // in; these four operate on the unrelated embedded KV store the
    // `xr-db` dashboard also browses). See docs/ARCHITECTURE.md's
    // "Embedded key-value store" section.
    if command.as_deref() == Some("db:list") {
        larust_support::db::connect(db_path(app.paths())).await?;
        for key in larust_support::db::keys().await? {
            println!("{key}");
        }
        return Ok(());
    }

    if command.as_deref() == Some("db:get") {
        larust_support::db::connect(db_path(app.paths())).await?;
        let key = std::env::args().nth(2).expect("usage: xr db:get <key>");
        match larust_support::db::get_raw(&key).await? {
            Some(value) => println!("{value}"),
            None => println!("(no value for {key})"),
        }
        return Ok(());
    }

    if command.as_deref() == Some("db:put") {
        larust_support::db::connect(db_path(app.paths())).await?;
        let key = std::env::args()
            .nth(2)
            .expect("usage: xr db:put <key> <value>");
        let raw = std::env::args()
            .nth(3)
            .expect("usage: xr db:put <key> <value>");
        larust_support::db::put_raw(&key, larust_support::db::parse_cli_value(&raw)).await?;
        return Ok(());
    }

    if command.as_deref() == Some("db:forget") {
        larust_support::db::connect(db_path(app.paths())).await?;
        let key = std::env::args().nth(2).expect("usage: xr db:forget <key>");
        larust_support::db::forget(&key).await?;
        return Ok(());
    }

    larust_support::wire::components()
        .register::<PostList>()
        .register::<PostForm>()
        .publish();

    // `.merge`, not `.group` — `routes::api`'s own middleware stack (rate
    // limiting) and `routes::web`'s own (CSRF) must stay fully independent;
    // `.group` deliberately shares the parent's top-level middleware with
    // whatever it registers (see `Router::group`'s own doc comment), which
    // previously leaked `csrf::verify` onto every `/api/*` route — see
    // `Router::merge`'s own doc comment and `docs/GOTCHAS.md`.
    let route =
        demo::routes::web::routes().merge(&app.config().api_prefix, demo::routes::api::routes());

    if command.as_deref() == Some("route:list") {
        print_routes(&route);
        return Ok(());
    }

    connect_database(app.paths()).await?;
    // See the `db:list`/`db:get`/`db:put`/`db:forget` arms above for why
    // this is also needed here: the dashboard (`routes/web.rs`'s
    // `DbPlugin` registration) is reached through *this* path, which
    // otherwise never touches the CLI-only connect calls above it — a real
    // bug caught in `larust-db`'s own live sanity check the first time
    // this pattern was scaffolded (see docs/ARCHITECTURE.md).
    larust_support::db::connect(db_path(app.paths())).await?;
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
    let database_url = database_url(paths)?;
    larust_support::orm::connect(&database_url).await
}

/// `larust-db`'s embedded store file, resolved against this app's own root
/// rather than the process's current working directory — the same
/// `AppPaths`-relative treatment [`database_url`] already gives the SQL
/// database's own relative path, and for the identical reason: `xr dev`/
/// `cargo run` need to land on the same file regardless of where they're
/// invoked from.
fn db_path(paths: &larust_core::AppPaths) -> std::path::PathBuf {
    paths.join("database/db.redb")
}

/// Resolves `config/database.rs`'s active connection to the URL
/// `larust_support::orm::connect()` needs — with one extra step
/// `ConnectionConfig::to_url()` itself can't do: a *relative* sqlite path
/// (`config/database.rs`'s own default, `database/database.sqlite`) needs
/// resolving against this app's own root (`AppPaths`), not the process's
/// current working directory, so `xr dev`/`cargo run` behave identically
/// regardless of where they're invoked from. An absolute path or `:memory:`
/// is left untouched.
fn database_url(paths: &larust_core::AppPaths) -> Result<String, larust_core::AppError> {
    let url = demo::config::database::config().default_connection_url()?;
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
