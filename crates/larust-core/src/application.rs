use crate::{
    debug, dev_reload, error, lifecycle, AppError, AppPaths, AppState, Config, GracefulShutdown,
};
use axum::http::{header, HeaderName, HeaderValue, StatusCode};
use axum::response::Response;
use axum::Router;
use std::any::Any;
use std::net::SocketAddr;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;
use tracing_subscriber::EnvFilter;

/// Upper bound on how long a restart-handoff replacement gets to report
/// readiness (see `lifecycle::handoff`) before this process gives up on
/// that attempt and keeps serving normally. Deliberately generous
/// compared to `GracefulShutdown::drain_timeout` — a slow build/startup
/// shouldn't fail a restart outright the way a stuck in-flight request
/// should eventually force an exit.
const HANDOFF_READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Drain timeout used automatically under `LARUST_DEV_RELOAD`, when the
/// app itself never opted into graceful shutdown explicitly — deliberately
/// much shorter than `GracefulShutdown::default()`'s own 30s. `dev_reload`'s
/// `/__larust_dev` endpoint is an SSE stream that never completes by
/// design (`Never` + `KeepAlive`, forever), so a graceful drain can never
/// finish *naturally* for it — the only thing that ever actually closes
/// that connection is this timeout's own hard backstop
/// (`tokio::time::sleep(drain_timeout)` → `std::process::exit(0)`,
/// further down in `serve()`). Since the browser's reload detection is
/// "the SSE connection dropped and reconnected," reload latency is
/// directly bounded by whatever this constant is set to — a
/// production-sized timeout here would make reload noticeably *slower*
/// than the plain hard-kill behavior this replaces, the opposite of what
/// this feature is for.
const DEV_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

pub struct Application {
    config: Config,
    paths: AppPaths,
    state: AppState,
    router: Router,
    graceful_shutdown: Option<GracefulShutdown>,
    health_route: Option<String>,
}

impl Application {
    /// Loads config, initializes logging, flips the process-wide debug flag
    /// (`crate::debug`) that gates descriptive error pages, and publishes
    /// config process-wide (`crate::config::config()`, used by
    /// `larust_support::url()`/`asset()`/`config()`).
    ///
    /// This does a small amount of synchronous filesystem I/O (`Config::load`)
    /// even when called from inside an async runtime. That's intentional: it
    /// runs once at startup, before any other async work is scheduled, so
    /// the blocking cost is negligible — not worth the complexity of
    /// `spawn_blocking` for a few KB of config file.
    pub fn new() -> Result<Self, AppError> {
        Self::with_paths(AppPaths::default())
    }

    /// Creates an application rooted at `root`, independent of the process
    /// working directory. New binaries should prefer this over `new()`.
    pub fn at_root(root: impl Into<std::path::PathBuf>) -> Result<Self, AppError> {
        Self::with_paths(AppPaths::new(root))
    }

    fn with_paths(paths: AppPaths) -> Result<Self, AppError> {
        let config = Config::load_from(&paths)?;
        init_logging(&config);
        debug::set(config.app_debug);
        config.clone().publish();
        let state = AppState::new(config.clone(), paths.clone());

        Ok(Self {
            config,
            paths,
            state,
            router: Router::new(),
            graceful_shutdown: None,
            health_route: None,
        })
    }

    /// Sets the router that will handle incoming requests.
    pub fn router(mut self, router: Router) -> Self {
        self.router = router;
        self
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Explicit application state suitable for application-owned Axum state.
    pub fn state(&self) -> AppState {
        self.state.clone()
    }

    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    /// Opts into graceful shutdown: on Ctrl+C (or, on Unix, SIGTERM),
    /// `serve()` stops accepting new connections and waits for in-flight
    /// ones to finish (bounded by `config.drain_timeout`) before exiting,
    /// instead of exiting instantly. See [`GracefulShutdown`]'s own doc
    /// comment for why this is opt-in rather than the default.
    pub fn with_graceful_shutdown(mut self, config: GracefulShutdown) -> Self {
        self.graceful_shutdown = Some(config);
        self
    }

    /// Registers Laravel-style health routing. Laravel applications use
    /// `'/up'` by default, so generated Larust applications should call
    /// `.with_health_route("/up")` during bootstrap.
    ///
    /// The endpoint is deliberately opt-in so applications keep control of
    /// their route namespace. It returns `200 OK` once Larust has completed
    /// bootstrap; a future diagnostic registry can add dependency checks
    /// without changing the route contract.
    pub fn with_health_route(mut self, path: impl Into<String>) -> Self {
        let path = path.into();
        assert!(
            path.starts_with('/'),
            "health route must start with '/'; received {path:?}"
        );
        self.health_route = Some(path);
        self
    }

    /// Binds to `config.app_port` on localhost and serves until the process
    /// is terminated.
    pub async fn serve(self) -> Result<(), AppError> {
        let addr = SocketAddr::from(([127, 0, 0, 1], self.config.app_port));
        tracing::info!(%addr, app = %self.config.app_name, env = %self.config.app_env, "starting server");

        // Set only on the child process `xr dev` spawns itself — never on
        // a plain `cargo run`, and never touched by any generated app
        // code (see the fuller explanation further down, at the route-
        // mounting site that originally introduced this check).
        let is_dev_reload = std::env::var_os("LARUST_DEV_RELOAD").is_some();

        // Auto-enables graceful shutdown (short, dev-appropriate timeout)
        // plus the restart-admin-channel specifically under `xr dev`'s own
        // reload flag — never for a plain production app, and never
        // overriding an app author's own explicit `.with_graceful_shutdown
        // (...)` call (that app just keeps today's kill-based dev
        // behavior, a documented, acceptable edge case: someone testing
        // their own production graceful-shutdown config locally under
        // `xr dev` gets what they asked for, not this override). This is
        // what lets `xr dev` perform a real zero-downtime handoff on every
        // rebuild instead of hard-killing the previous process first.
        let graceful_shutdown = self.graceful_shutdown.or_else(|| {
            is_dev_reload.then_some(GracefulShutdown {
                drain_timeout: DEV_DRAIN_TIMEOUT,
                restart_channel: true,
            })
        });
        let app_name = self.config.app_name.clone();

        // A process spawned as a restart-handoff replacement (see
        // `lifecycle::handoff`) inherits the *same* listening socket its
        // predecessor was already using, read from its own stdin as one
        // line of encoded text, instead of binding `addr` fresh — the
        // whole point of the handoff being able to start serving with no
        // gap at all. Ordinary startup (a plain `cargo run`/`xr dev`, or
        // any generated app not using the restart-handoff feature) never
        // sets this env var and binds fresh exactly as before this
        // feature existed.
        let is_handoff_replacement =
            std::env::var_os(lifecycle::listener::INHERIT_LISTENER_ENV).is_some();
        let std_listener = if is_handoff_replacement {
            let mut line = String::new();
            tokio::io::AsyncBufReadExt::read_line(
                &mut tokio::io::BufReader::new(tokio::io::stdin()),
                &mut line,
            )
            .await
            .map_err(|source| AppError::Internal(Box::new(source)))?;
            lifecycle::listener::inherit(&line)
                .map_err(|source| AppError::Internal(Box::new(source)))?
        } else {
            lifecycle::listener::bind(addr)
                .map_err(|source| AppError::Internal(Box::new(source)))?
        };
        // Kept as a plain std listener, separate from the tokio-wrapped
        // one below — the restart-handoff machinery (`lifecycle::admin`,
        // `lifecycle::handoff`) works with std sockets directly (it needs
        // the raw fd/socket handle, not an async wrapper around one), and
        // needs its own independent handle to the same underlying kernel
        // socket regardless of whether graceful shutdown/the admin
        // channel end up being configured at all.
        let admin_listener = std_listener
            .try_clone()
            .map_err(|source| AppError::Internal(Box::new(source)))?;
        std_listener
            .set_nonblocking(true)
            .map_err(|source| AppError::Internal(Box::new(source)))?;
        let listener = tokio::net::TcpListener::from_std(std_listener)
            .map_err(|source| AppError::Internal(Box::new(source)))?;

        // `.route(...)` panics on an exact-path collision with a route the
        // app already registered — acceptable here given how unlikely a
        // real app is to independently choose the `__larust_dev` path, but
        // worth knowing if this route's name ever needs to change.
        let router = if is_dev_reload {
            self.router
                .route("/__larust_dev", axum::routing::get(dev_reload::handler))
        } else {
            self.router
        };

        let router = if let Some(path) = self.health_route {
            router.route(&path, axum::routing::get(health))
        } else {
            router
        };

        // Served at the URL root (`public/logo.png` → `/logo.png`), not
        // under a `/public` prefix — matching Laravel's own convention,
        // where `public/` *is* the webserver's docroot. A registered route
        // wins over a same-path file for any *literal* request path:
        // `fallback_service` is only ever consulted when axum's own router
        // finds no match. That precedence is per byte, not per resolved
        // path, though — axum matches on the raw, undecoded request path,
        // while `ServeDir` percent-decodes before resolving a file, so a
        // percent-encoded request (`/app%2Ejs`) can reach a file the
        // registered route at `/app.js` would otherwise have handled. Not
        // a traversal/disclosure risk (it can only ever reach content
        // that's already sitting in `public/`), just worth knowing the
        // "route always wins" framing isn't byte-for-byte absolute. A
        // missing `public/` directory isn't an error here — `ServeDir`
        // checks the filesystem per-request, not at construction, so
        // every request just 404s until the directory exists.
        let router = router.fallback_service(ServeDir::new(self.paths.public()));

        // Applies to every response, not just `public/`'s — `nosniff` is a
        // broadly-correct default (OWASP baseline), but it matters
        // specifically here: `ServeDir` infers a served file's Content-Type
        // from its *extension* alone (via `mime_guess`), never its actual
        // bytes, so anything written into `public/` under an
        // extension-spoofed name (e.g. an app that validates an upload's
        // declared MIME type but not its real bytes) would otherwise be
        // subject to the browser's own content-sniffing — this header is
        // what keeps a browser from second-guessing the declared type.
        let router = router.layer(SetResponseHeaderLayer::overriding(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ));
        // Safe defaults which do not depend on whether TLS is terminated by
        // this process or a reverse proxy. Applications can set a stricter
        // CSP/HSTS policy at their own edge, where their asset and proxy
        // topology is known.
        let router = router.layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("referrer-policy"),
            HeaderValue::from_static("strict-origin-when-cross-origin"),
        ));
        let router = router.layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("x-frame-options"),
            HeaderValue::from_static("DENY"),
        ));

        let router = router.layer(CatchPanicLayer::custom(handle_panic));

        // Signals the predecessor process (see `lifecycle::handoff`) that
        // this replacement is genuinely about to start accepting
        // connections on the inherited listener — the predecessor is
        // waiting on exactly this line before it begins its own graceful
        // shutdown. A no-op on any ordinary boot.
        if is_handoff_replacement {
            lifecycle::readiness::announce_ready();
        }

        let Some(graceful_shutdown) = graceful_shutdown else {
            // Today's exact behavior, byte-for-byte unchanged: a bare
            // `axum::serve` that exits the instant the process is killed.
            axum::serve(listener, router)
                .await
                .map_err(|source| AppError::Internal(Box::new(source)))?;
            return Ok(());
        };

        // `shutdown_tx` fires once, on Ctrl+C/SIGTERM — `with_graceful_shutdown`
        // then stops accepting new connections and waits for in-flight ones
        // to finish. The `drain_timeout` sleep in this same spawned task is
        // a hard backstop: if the graceful drain hasn't finished naturally
        // by then (a stuck connection, a hung upstream call), force the
        // process to exit anyway rather than hang a deploy forever. If the
        // drain finishes first, `serve()` below returns `Ok(())`, the
        // process exits normally, and this still-sleeping task is simply
        // dropped along with it — nothing to clean up either way.
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let drain_timeout = graceful_shutdown.drain_timeout;
        let restart_channel_enabled = graceful_shutdown.restart_channel;
        tokio::spawn(async move {
            if restart_channel_enabled {
                let address = lifecycle::admin::channel_address(&app_name);
                tokio::select! {
                    _ = lifecycle::wait_for_termination() => {
                        tracing::info!(
                            ?drain_timeout,
                            "shutdown signal received; draining in-flight requests"
                        );
                    }
                    outcome = lifecycle::admin::run_until_command(
                        &address,
                        &admin_listener,
                        HANDOFF_READY_TIMEOUT,
                    ) => {
                        match outcome {
                            lifecycle::admin::AdminOutcome::Handoff(child) => {
                                // Dropping this handle does *not* kill the
                                // child — `tokio::process::Command` only
                                // does that with `.kill_on_drop(true)`,
                                // which this code path never sets. It's
                                // already running and serving on the
                                // listener this process just handed off;
                                // nothing further needs doing with the
                                // handle itself.
                                tracing::info!(
                                    pid = child.id(),
                                    ?drain_timeout,
                                    "restart handoff succeeded; draining in-flight requests"
                                );
                            }
                            lifecycle::admin::AdminOutcome::Stop => {
                                tracing::info!(
                                    ?drain_timeout,
                                    "stop command received; draining in-flight requests"
                                );
                            }
                        }
                    }
                }
            } else {
                lifecycle::wait_for_termination().await;
                tracing::info!(
                    ?drain_timeout,
                    "shutdown signal received; draining in-flight requests"
                );
            }
            let _ = shutdown_tx.send(());
            tokio::time::sleep(drain_timeout).await;
            tracing::warn!(
                "drain timeout elapsed; forcing exit with any remaining connections dropped"
            );
            std::process::exit(0);
        });

        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await
            .map_err(|source| AppError::Internal(Box::new(source)))?;

        Ok(())
    }
}

async fn health() -> StatusCode {
    StatusCode::OK
}

/// Converts a panicking handler into a response instead of dropping the
/// connection with nothing — before this, a panic anywhere in a handler
/// meant that one request just failed silently, with no framework-level
/// response at all.
fn handle_panic(payload: Box<dyn Any + Send + 'static>) -> Response {
    // `downcast` (consuming) rather than `downcast_ref` + `.clone()` for the
    // `String` case — avoids cloning a payload that's about to be dropped
    // anyway; the panic path is cold, but there's no reason to allocate
    // twice when ownership is right there.
    let message = match payload.downcast::<String>() {
        Ok(s) => *s,
        Err(payload) => match payload.downcast_ref::<&str>() {
            Some(s) => s.to_string(),
            None => "unknown panic payload".to_string(),
        },
    };
    error::render_panic(&message)
}

fn init_logging(config: &Config) {
    // Plain "debug" would also turn on sqlx's and tower-sessions' own
    // per-query/per-request DEBUG spans, each of which logs the full,
    // often multi-line SQL statement as a single unindented field — wraps
    // unreadably in a normal-width terminal and drowns out the app's own
    // logs. `sqlx=warn`/`tower_sessions=warn` keeps their genuinely
    // actionable output (sqlx's own slow-query warning still fires) while
    // dropping the per-call noise; set `RUST_LOG` to override this
    // entirely, e.g. `RUST_LOG=sqlx=debug` to see every query again.
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(if config.app_env == "local" {
            "debug,sqlx=warn,tower_sessions=warn"
        } else {
            "info"
        })
    });

    // `try_init` (not `init`) so re-initializing in tests/examples doesn't
    // panic; any other init failure just means the default global
    // subscriber didn't get set, which is safe to ignore here.
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use tower::ServiceExt;

    /// A unit test (not an integration test under `tests/`) specifically
    /// because `handle_panic` is private — only code inside this crate can
    /// reach it directly. This compiles to its own isolated test binary
    /// (there are no other `src/`-local unit tests in this crate today),
    /// so it doesn't race `debug::set()`'s `OnceLock` against the
    /// `tests/error_response_*.rs` integration tests, which each run in
    /// their own separate process anyway.
    ///
    /// Only covers the production-mode (default, unset) branch — flipping
    /// to debug mode here would permanently commit this test binary's
    /// `OnceLock` for any test added later in this file. The debug-mode
    /// rendering path (the same `debug_page` helper `AppError::Internal`
    /// already exercises in `tests/error_response_debug_mode.rs`) was
    /// verified live against a real running app instead: a deliberately
    /// panicking handler correctly rendered the panic message as HTML with
    /// `APP_DEBUG=true`, and the server kept serving subsequent requests
    /// afterward.
    #[tokio::test]
    async fn panicking_handler_is_caught_and_rendered_instead_of_dropping_the_connection() {
        async fn always_panics() -> &'static str {
            panic!("boom");
        }

        let router = Router::new()
            .route("/panic", get(always_panics))
            .layer(CatchPanicLayer::custom(handle_panic));

        let response = router
            .oneshot(Request::get("/panic").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(bytes, "internal server error".as_bytes());
    }

    #[tokio::test]
    async fn laravel_style_health_handler_returns_ok() {
        assert_eq!(health().await, StatusCode::OK);
    }

    /// Exercises the exact `.fallback_service(ServeDir::new(...))` pattern
    /// `serve()` wires up — against a `tempfile::tempdir()` rather than the
    /// real, hardcoded `"public"` path (relative to the process's CWD, not
    /// something a unit test should depend on), so this proves the
    /// underlying tower-http integration behaves correctly without needing
    /// to touch the real filesystem convention. Only covers the literal,
    /// byte-identical-path case — see the comment above `fallback_service`
    /// in `serve()` for the percent-encoded-path caveat this doesn't
    /// (and can't easily) pin without depending on tower-http/axum
    /// internals more closely than a unit test here should.
    #[tokio::test]
    async fn fallback_service_serves_static_files_but_registered_routes_still_win() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("logo.png"), b"fake-image-bytes").unwrap();
        std::fs::write(
            dir.path().join("app.js"),
            b"real file, should lose to the route",
        )
        .unwrap();

        async fn app_js_route() -> &'static str {
            "handled by a registered route"
        }

        let router = Router::new()
            .route("/app.js", get(app_js_route))
            .fallback_service(ServeDir::new(dir.path()));

        // A path with no registered route, but a real file on disk, is
        // served directly from that file.
        let logo_response = router
            .clone()
            .oneshot(Request::get("/logo.png").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(logo_response.status(), StatusCode::OK);
        let logo_bytes = axum::body::to_bytes(logo_response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(logo_bytes, "fake-image-bytes".as_bytes());

        // A path that exists as *both* a registered route and a real file
        // is handled by the route — `fallback_service` is only ever
        // consulted when nothing else matched.
        let app_js_response = router
            .clone()
            .oneshot(Request::get("/app.js").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let app_js_bytes = axum::body::to_bytes(app_js_response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(app_js_bytes, "handled by a registered route".as_bytes());

        // A path matching neither a route nor a file 404s, same as today.
        let missing_response = router
            .oneshot(
                Request::get("/does-not-exist.css")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing_response.status(), StatusCode::NOT_FOUND);
    }
}
