use crate::{
    debug, dev_reload, error, error_pages, lifecycle, AppError, AppPaths, AppState, Config,
    ErrorPages, GracefulShutdown,
};
use axum::http::{header, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use std::any::Any;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;

/// Upper bound on how long a restart-handoff replacement gets to report
/// readiness (see `lifecycle::handoff`) before this process gives up on
/// that attempt and keeps serving normally. Deliberately generous
/// compared to `GracefulShutdown::drain_timeout` - a slow build/startup
/// shouldn't fail a restart outright the way a stuck in-flight request
/// should eventually force an exit.
const HANDOFF_READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Drain timeout used automatically under `LARUST_DEV_RELOAD`, when the
/// app itself never opted into graceful shutdown explicitly - deliberately
/// much shorter than `GracefulShutdown::default()`'s own 30s. `dev_reload`'s
/// `/__larust_dev` endpoint is an SSE stream that never completes by
/// design (`Never` + `KeepAlive`, forever), so a graceful drain can never
/// finish *naturally* for it - the only thing that ever actually closes
/// that connection is this timeout's own hard backstop
/// (`tokio::time::sleep(drain_timeout)` → `std::process::exit(0)`,
/// further down in `serve()`). Since the browser's reload detection is
/// "the SSE connection dropped and reconnected," reload latency is
/// directly bounded by whatever this constant is set to - a
/// production-sized timeout here would make reload noticeably *slower*
/// than the plain hard-kill behavior this replaces, the opposite of what
/// this feature is for.
const DEV_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Warns once, at startup, if [`Config::session_driver`] resolved to
/// anything other than the two real, supported values (`"database"`,
/// `"file"`). Originally this field didn't exist at all - `SESSION_DRIVER`
/// was silently never read anywhere in this codebase, reported directly as
/// a real production confusion (a `.env` value that looked like it should
/// matter turning out to have done nothing, with no error, no log line,
/// nothing). Once support for `"file"` was added, that exact same silent-
/// no-op shape became newly possible again for anyone who sets
/// `SESSION_DRIVER` to a typo, or to a Laravel value this framework
/// doesn't implement (`"array"`, `"redis"`, `"cookie"`, ...) - so this
/// still warns for those, even though the field itself is real now.
/// Checking the *resolved* `Config` value (not the raw environment, unlike
/// this function's own predecessor) is deliberate: it's the one thing that
/// actually determines behavior, and it's already been through the exact
/// `env_or("SESSION_DRIVER", "database")` resolution every other config
/// field uses, so there's no separate "did the user actually set this"
/// check to duplicate here the way `default_log_channel_to_file_if_
/// stdio_detached` needs for its own, different reason.
fn warn_if_session_driver_is_unsupported(config: &Config) {
    if !is_supported_session_driver(&config.session_driver) {
        tracing::warn!(
            session_driver = %config.session_driver,
            "SESSION_DRIVER is set to an unsupported value - Larust only supports \"database\" \
             (the default) or \"file\". Falling back to \"database\". See \
             docs/the-basics/middleware-sessions-and-csrf.md for what each one means."
        );
    }
}

/// The two real values [`Config::session_driver`]/`SESSION_DRIVER` accept -
/// pulled out as its own pure function (rather than inlined into
/// `warn_if_session_driver_is_unsupported`'s own `if`) specifically so a
/// test can check the actual list of supported values directly, without
/// needing to capture a `tracing::warn!` call to prove anything about it.
/// `larust_http::session::session_layer`'s own driver dispatch is the
/// other, independent place this same two-value split matters - kept as a
/// literal `match` there rather than calling this (a `larust-core` type
/// calling back into `larust-http` would be a dependency cycle), so if a
/// third driver is ever added, both places need the identical update.
fn is_supported_session_driver(value: &str) -> bool {
    matches!(value, "database" | "file")
}

/// Set only on the child `xr deploy --run`'s `start_detached` spawns
/// (`crates/larust-cli/src/deploy.rs`) - never on a plain `cargo run`/
/// `xr dev` boot. Bare string literal, not a shared `pub const`, matching
/// `LARUST_DEV_RELOAD`'s own precedent just above for the identical
/// reason: a CLI-sets/core-reads marker that only ever needs to agree by
/// name within this one workspace, not a real cross-crate API contract.
const STDIO_DETACHED_ENV: &str = "LARUST_STDIO_DETACHED";

/// Defaults [`Config::log_channel`] to `"file"` instead of its own
/// built-in `"stdout"` default, but *only* when both are true: the user
/// never explicitly set `LOG_CHANNEL` at all, and this process's
/// stdout/stderr were permanently redirected to the null device by `xr
/// deploy --run`'s `start_detached`. Closes a real, silent failure mode,
/// reported directly from a production app: `start_detached` sets
/// `Stdio::null()` on the very first generation's stdout/stderr (needed so
/// the detached child doesn't print into `xr deploy`'s own terminal), and
/// every later zero-downtime restart-handoff replacement inherits its own
/// stdout from its immediate predecessor (`lifecycle::handoff::
/// spawn_replacement_and_wait_for_ready`'s own `Stdio::inherit()`) - so
/// that null redirection propagates down an app's *entire* production
/// lifetime, the same way [`STDIO_DETACHED_ENV`] itself does (`Command`
/// inherits its parent's environment by default, and nothing here ever
/// clears it). With `LOG_CHANNEL` left at its default, that combination
/// produces zero log output, ever, for as long as that app runs - not an
/// error, just total silence, which is far more dangerous than a
/// merely-suboptimal default: this fixes it automatically, with no action
/// required from whoever's operating the app, rather than only warning
/// about it (see `docs/deployment-and-desktop-apps.md`'s own warning,
/// which still explains the mechanism for anyone who *does* want plain
/// `stdout` and reads why they can't have it silently here).
///
/// Checks the *real* environment (`std::env::var`), not `config.
/// log_channel` itself, deliberately: by the time `Config::from_value` has
/// run, `Config`'s own built-in default and an explicit, redundant
/// `LOG_CHANNEL=stdout` in `.env` both resolve to the identical string -
/// only the raw environment can tell "the user never touched this" apart
/// from "the user explicitly chose the same value the default already
/// was," and only the former should ever be overridden here.
fn default_log_channel_to_file_if_stdio_detached(config: &mut Config) {
    let user_set_log_channel = std::env::var("LOG_CHANNEL").is_ok();
    let stdio_is_detached = std::env::var_os(STDIO_DETACHED_ENV).is_some();
    if !user_set_log_channel && stdio_is_detached {
        config.log_channel = "file".to_string();
    }
}

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
    /// `config` is the app's own generated `config/app.rs`'s `pub fn
    /// config() -> serde_json::Value` (e.g. `my_app::config::app::config`) -
    /// a plain function item, not a closure, so every real caller can
    /// just pass the function itself. Called *after* `.env` is loaded (see
    /// `with_paths` below), so its own `env`/`env_bool`/`env_or` calls
    /// (`larust_support::config_env`) see whatever `.env` set.
    ///
    /// This does a small amount of synchronous filesystem I/O (loading
    /// `.env`) even when called from inside an async runtime. That's
    /// intentional: it runs once at startup, before any other async work is
    /// scheduled, so the blocking cost is negligible - not worth the
    /// complexity of `spawn_blocking` for a few KB of env file.
    pub fn new(config: fn() -> serde_json::Value) -> Result<Self, AppError> {
        Self::with_paths(AppPaths::default(), config)
    }

    /// Creates an application rooted at `root`, independent of the process
    /// working directory. New binaries should prefer this over `new()`.
    pub fn at_root(
        root: impl Into<std::path::PathBuf>,
        config: fn() -> serde_json::Value,
    ) -> Result<Self, AppError> {
        Self::with_paths(AppPaths::new(root), config)
    }

    fn with_paths(paths: AppPaths, config: fn() -> serde_json::Value) -> Result<Self, AppError> {
        dotenvy::from_path(paths.env()).ok();
        let mut config = Config::from_value(&config())?;
        default_log_channel_to_file_if_stdio_detached(&mut config);
        crate::logging::init(&config, &paths);
        warn_if_session_driver_is_unsupported(&config);
        debug::set(config.app_debug);
        config.clone().publish();
        paths.clone().publish();
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

    /// Registers the app's 404/500 pages, rendered once here (not per
    /// request - see `ErrorPages`' own doc comment) and used by every
    /// `AppError::NotFound`/`Internal`/`Config`/caught-panic response for
    /// the rest of the process's life. Entirely optional - `AppError`'s own
    /// `into_response()` falls back to Larust's built-in default pages if
    /// this is never called, so an app that skips this still gets styled
    /// error pages, just not the option to override them.
    pub fn with_error_pages(self, pages: ErrorPages) -> Self {
        error_pages::set(pages);
        self
    }

    /// Binds to `config.app_port` on localhost and serves until the process
    /// is terminated.
    pub async fn serve(self) -> Result<(), AppError> {
        let addr = SocketAddr::from(([127, 0, 0, 1], self.config.app_port));
        tracing::info!(%addr, app = %self.config.app_name, env = %self.config.app_env, "starting server");

        // Set only on the child process `xr dev` spawns itself - never on
        // a plain `cargo run`, and never touched by any generated app
        // code (see the fuller explanation further down, at the route-
        // mounting site that originally introduced this check).
        let is_dev_reload = std::env::var_os("LARUST_DEV_RELOAD").is_some();

        // Auto-enables graceful shutdown (short, dev-appropriate timeout)
        // plus the restart-admin-channel specifically under `xr dev`'s own
        // reload flag - never for a plain production app, and never
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
        // line of encoded text, instead of binding `addr` fresh - the
        // whole point of the handoff being able to start serving with no
        // gap at all. Ordinary startup (a plain `cargo run`/`xr dev`, or
        // any generated app not using the restart-handoff feature) never
        // sets this env var and binds fresh exactly as before this
        // feature existed.
        let is_handoff_replacement =
            std::env::var_os(lifecycle::listener::INHERIT_LISTENER_ENV).is_some();
        if is_handoff_replacement {
            // Arms this process's own death-signal delivery (Linux) /
            // relies on already having inherited job-object membership
            // (Windows) - see `lifecycle::supervisor`'s own doc comment
            // for why this now happens *here*, inside the already-exec'd
            // replacement, rather than via a `Command::pre_exec` hook run
            // by the parent between `fork()` and `exec()` (that design's
            // real, confirmed bug).
            lifecycle::supervisor::arm_pdeathsig();
        }
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
        // one below - the restart-handoff machinery (`lifecycle::admin`,
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
        // app already registered - acceptable here given how unlikely a
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
        // under a `/public` prefix - matching Laravel's own convention,
        // where `public/` *is* the webserver's docroot. A registered route
        // wins over a same-path file for any *literal* request path:
        // `fallback_service` is only ever consulted when axum's own router
        // finds no match. That precedence is per byte, not per resolved
        // path, though - axum matches on the raw, undecoded request path,
        // while `ServeDir` percent-decodes before resolving a file, so a
        // percent-encoded request (`/app%2Ejs`) can reach a file the
        // registered route at `/app.js` would otherwise have handled. Not
        // a traversal/disclosure risk (it can only ever reach content
        // that's already sitting in `public/`), just worth knowing the
        // "route always wins" framing isn't byte-for-byte absolute. A
        // missing `public/` directory isn't an error here - `ServeDir`
        // checks the filesystem per-request, not at construction, so
        // every request just 404s until the directory exists.
        //
        // `.not_found_service(...)` (not a bare `ServeDir`) so a request
        // matching *neither* a registered route *nor* a real file gets
        // Larust's own styled 404 (default or app-overridden, see
        // `ErrorPages`) instead of `ServeDir`'s own bare, empty-body one -
        // this is the only path a genuinely dead URL a visitor hits ever
        // takes; `AppError::NotFound` is otherwise only ever constructed by
        // a handler explicitly returning it (e.g. a failed route-model-
        // binding lookup). Scoped precisely to `ServeDir`'s own not-found
        // response - a handler elsewhere in the app that builds its own
        // `(StatusCode::NOT_FOUND, ...)` response directly is untouched.
        let router = router.fallback_service(ServeDir::new(self.paths.public()).not_found_service(
            tower::service_fn(|_req: axum::extract::Request| async {
                Ok::<_, std::convert::Infallible>(AppError::NotFound.into_response())
            }),
        ));

        // Applies to every response, not just `public/`'s - `nosniff` is a
        // broadly-correct default (OWASP baseline), but it matters
        // specifically here: `ServeDir` infers a served file's Content-Type
        // from its *extension* alone (via `mime_guess`), never its actual
        // bytes, so anything written into `public/` under an
        // extension-spoofed name (e.g. an app that validates an upload's
        // declared MIME type but not its real bytes) would otherwise be
        // subject to the browser's own content-sniffing - this header is
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

        install_backtrace_capturing_panic_hook();
        let router = router.layer(CatchPanicLayer::custom(handle_panic));

        // Signals the predecessor process (see `lifecycle::handoff`) that
        // this replacement is genuinely about to start accepting
        // connections on the inherited listener - the predecessor is
        // waiting on exactly this line before it begins its own graceful
        // shutdown. A no-op on any ordinary boot.
        if is_handoff_replacement {
            lifecycle::readiness::announce_ready();
            // From this exact point on, the predecessor exiting is the
            // expected, successful conclusion of this handoff, not
            // something to react to - see `lifecycle::supervisor::linux::
            // disarm_pdeathsig`'s own doc comment for the real bug
            // leaving this armed past this point caused (a brand-new
            // replacement receiving its predecessor's own death signal
            // and mistakenly starting to drain itself).
            lifecycle::supervisor::disarm_pdeathsig();
        }

        let Some(graceful_shutdown) = graceful_shutdown else {
            // Today's exact behavior, byte-for-byte unchanged apart from
            // `into_make_service_with_connect_info`: a bare `axum::serve`
            // that exits the instant the process is killed.
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .map_err(|source| AppError::Internal(Box::new(source)))?;
            return Ok(());
        };

        // `shutdown_tx` fires once, on Ctrl+C/SIGTERM - `with_graceful_shutdown`
        // then stops accepting new connections and waits for in-flight ones
        // to finish. The `drain_timeout` sleep in this same spawned task is
        // a hard backstop: if the graceful drain hasn't finished naturally
        // by then (a stuck connection, a hung upstream call), force the
        // process to exit anyway rather than hang a deploy forever. If the
        // drain finishes first, `serve()` below returns `Ok(())`, the
        // process exits normally, and this still-sleeping task is simply
        // dropped along with it - nothing to clean up either way.
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let drain_timeout = graceful_shutdown.drain_timeout;
        let restart_channel_enabled = graceful_shutdown.restart_channel;
        // Set right before `shutdown_tx.send(())`, only on the `Handoff`
        // arm below - checked after `axum::serve()` returns to decide
        // whether the `std::process::exit(0)` bypass further down applies.
        // See that bypass's own doc comment for the Windows hang this
        // exists to sidestep: it used to key off `is_dev_reload` instead
        // of this, which was too narrow - a production app calling
        // `.with_graceful_shutdown(GracefulShutdown { restart_channel:
        // true, .. })` directly (never setting `LARUST_DEV_RELOAD`, e.g.
        // via `xr restart`) hits the *exact same* hang on a successful
        // handoff, and `is_dev_reload` being false meant the bypass never
        // fired for it - confirmed by reproducing this exact hang via
        // `tests/stale_binary_path.rs`, which uses this precise
        // configuration. "Did this process hand off a child that outlives
        // it" is the condition that actually matters, not which caller
        // happened to enable the restart channel.
        let handed_off_child = Arc::new(AtomicBool::new(false));
        tokio::spawn({
            let handed_off_child = Arc::clone(&handed_off_child);
            async move {
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
                                    // child - `tokio::process::Command` only
                                    // does that with `.kill_on_drop(true)`,
                                    // which this code path never sets. It's
                                    // already running and serving on the
                                    // listener this process just handed off;
                                    // nothing further needs doing with the
                                    // handle itself - see the forced
                                    // `std::process::exit(0)` after
                                    // `axum::serve()` below for *why* dropping
                                    // it here (rather than awaiting its exit)
                                    // is not just sufficient but necessary.
                                    tracing::info!(
                                        pid = child.id(),
                                        ?drain_timeout,
                                        "restart handoff succeeded; draining in-flight requests"
                                    );
                                    handed_off_child.store(true, Ordering::SeqCst);
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
            }
        });

        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        })
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

        // A restart-handoff replacement's `Child` handle (see the
        // `AdminOutcome::Handoff` arm above) is deliberately dropped, not
        // awaited - the whole point is that the replacement outlives this
        // process. But on Windows, that leaves an outstanding exit-watch
        // registration on the (still-running, by design) replacement's
        // process handle, and confirmed empirically (not from docs -
        // reproduced directly via instrumented tracing before landing this
        // fix): the ordinary return-from-`main()` path hangs on it. Once
        // `axum::serve()` returns here, the `#[tokio::main]`-generated
        // wrapper's own `Runtime` gets dropped as `serve()`'s `Ok(())`
        // unwinds back through `main()`, and that drop blocks the whole
        // process from ever actually exiting until every outstanding
        // Windows blocking-pool wait completes - including that
        // replacement's exit-watch, which by definition won't resolve
        // until the replacement itself exits, i.e. for the rest of the
        // dev session. The result: this process never actually terminates
        // - it just stops serving and lingers as an invisible zombie,
        // still holding the Windows Job Object it created to supervise
        // *its own* handoff target (`lifecycle::supervisor`) open. If that
        // zombie (or an earlier one in the same chain, or `xr dev` itself)
        // is ever force-killed later, `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`
        // cascades the kill down through every subsequent generation,
        // taking out the *currently serving* process as collateral
        // damage - this is what an earlier investigation session
        // mistakenly diagnosed as "generation N vanished." Calling
        // `std::process::exit` here - a real OS-level termination, no
        // destructors, no waiting for anything - sidesteps that hang
        // entirely; safe specifically because this whole method already
        // only reaches this point after a real (not stuck) graceful
        // drain, so there's nothing left this process still needs to do.
        //
        // Keyed on `handed_off_child` (set only on the `Handoff` arm
        // above), **not** `is_dev_reload` - a real, previously-unfixed gap
        // this comment used to describe as intentional ("a production app
        // without the restart-admin-channel enabled never hands off a
        // `Child` in the first place"), which conflated two different
        // conditions: `is_dev_reload` (this process was spawned by `xr
        // dev`) and "this process actually handed off a child that
        // outlives it" (true whenever a `RESTART` command produces a
        // successful handoff, via `xr dev` *or* `xr restart` against a
        // plain production app with `GracefulShutdown { restart_channel:
        // true, .. }` - the exact configuration `tests/
        // stale_binary_path.rs`'s own fixture uses, confirmed to reproduce
        // this exact hang before this fix). A `STOP` command or a plain
        // OS shutdown signal never sets `handed_off_child`, so this stays
        // a no-op for every path that never spawned a still-running
        // child - the ordinary `Ok(())` return below is already correct
        // for those. See `docs/GOTCHAS.md`.
        if handed_off_child.load(Ordering::SeqCst) {
            std::process::exit(0);
        }

        Ok(())
    }
}

async fn health() -> Response {
    // Laravel's built-in `/up` endpoint presents a deliberately small
    // browser-friendly status page. Keep ours entirely self-contained: a
    // health check should not depend on a CDN, font provider, database, or
    // application template being available in order to report bootstrap
    // success to a load balancer.
    let html = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Application</title>
  <style>
    :root { color-scheme: light dark; font-family: ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif; }
    body { margin: 0; min-height: 100vh; display: grid; place-items: center; background: #f3f4f6; color: #111827; }
    main { width: min(36rem, calc(100% - 3rem)); padding: 1.5rem; }
    article { display: flex; gap: 1.25rem; align-items: flex-start; padding: 1.5rem; border-radius: .75rem; background: #fff; box-shadow: 0 20px 45px rgb(17 24 39 / .15); }
    .status { position: relative; flex: 0 0 auto; width: .75rem; height: .75rem; margin-top: .38rem; border-radius: 999px; background: #4ade80; }
    .status::before { content: ""; position: absolute; inset: 0; border-radius: inherit; background: #4ade80; animation: pulse 1.5s ease-out infinite; }
    h1 { margin: 0; font-size: 1.25rem; }
    p { margin: .5rem 0 0; color: #6b7280; font-size: .875rem; }
    @keyframes pulse { from { transform: scale(1); opacity: .8; } to { transform: scale(2.4); opacity: 0; } }
    @media (prefers-color-scheme: dark) { body { background: #111827; color: #f9fafb; } article { background: #1f2937; } p { color: #9ca3af; } }
    @media (prefers-reduced-motion: reduce) { .status::before { animation: none; } }
  </style>
</head>
<body>
  <main>
    <article role="status" aria-live="polite">
      <span class="status" aria-hidden="true"></span>
      <div>
        <h1>Application up</h1>
        <p>HTTP request received. Response rendered in <span id="response-time">--</span>.</p>
      </div>
    </article>
  </main>
  <script>
    const responseTime = document.getElementById('response-time');
    responseTime.textContent = `${Math.max(0, Math.round(performance.now()))}ms`;
  </script>
</body>
</html>"#
        .to_string();
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        html,
    )
        .into_response()
}

thread_local! {
    /// Set by `install_backtrace_capturing_panic_hook`'s own hook, read
    /// (and cleared) by `handle_panic` right after - see that function's
    /// own doc comment for why a thread-local is sound here despite
    /// running on a shared tokio worker thread: nothing else can run on
    /// this exact OS thread between the hook firing and `handle_panic`
    /// reading it back, since `tower_http::catch_panic`'s own
    /// `ResponseFuture` calls `handle_panic` *synchronously*, within the
    /// very same `poll()` that caught the panic via `catch_unwind` -
    /// there's no `.await` point, and therefore no scheduling opportunity
    /// for the executor to run a *different* task on this thread, anywhere
    /// in between.
    static LAST_PANIC_BACKTRACE: std::cell::RefCell<Option<error::PanicBacktrace>> =
        const { std::cell::RefCell::new(None) };
}

/// Installs a custom panic hook that captures a backtrace at the exact
/// moment a panic happens, for `handle_panic` (below) to pick up and hand
/// to `error::render_panic` - capturing *after* `std::panic::catch_unwind`
/// returns (inside `handle_panic` itself) would be useless, since the stack
/// has already unwound back up to `tower_http::catch_panic`'s own catching
/// frame by then; a `Backtrace::capture()` taken there would just show
/// *that* frame, not the real panic site several frames further down that
/// no longer exist on the stack at all.
///
/// Guarded by `Once` so installing it is safe to call on every `serve()`
/// (including more than one `Application` in the same process, e.g. in
/// tests) without stacking an unbounded chain of hooks each wrapping the
/// last - `std::panic::set_hook` replaces whatever hook is currently
/// installed, so without this guard, a *second* `serve()` call would wrap
/// an already-wrapped hook in another layer forever.
///
/// Calling `std::backtrace::Backtrace::capture()` unconditionally, on
/// *every* panic regardless of debug mode, is deliberate and cheap: per
/// its own documentation, it only actually walks the stack when
/// `RUST_BACKTRACE`/`RUST_LIB_BACKTRACE` is set at all - otherwise this is
/// just one cached atomic read, the identical cost Rust's own default
/// panic hook already pays on every panic regardless of whether anything
/// downstream of it cares. Production mode still never *shows* whatever
/// was captured here (`render_panic` only renders it when `debug::
/// is_enabled()`), so this adds no new information disclosure risk - only
/// debug mode's own already-privileged detail gets an extra section.
fn install_backtrace_capturing_panic_hook() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            use std::backtrace::{Backtrace, BacktraceStatus};
            let backtrace = Backtrace::capture();
            let captured = match backtrace.status() {
                BacktraceStatus::Captured => error::PanicBacktrace::Captured(backtrace.to_string()),
                BacktraceStatus::Unsupported => error::PanicBacktrace::Unsupported,
                // `Disabled`, and any future non-exhaustive variant alike -
                // the same "degrade to the always-safe default" precedent
                // `logging::init`'s own unrecognized-`LOG_CHANNEL` handling
                // already uses.
                _ => error::PanicBacktrace::Disabled,
            };
            LAST_PANIC_BACKTRACE.with(|cell| *cell.borrow_mut() = Some(captured));
            // Preserves whatever the previous hook did (Rust's own default -
            // printing to stderr - unless something else already replaced
            // it) - this hook adds a capture, it doesn't replace existing
            // panic-reporting behavior.
            previous(info);
        }));
    });
}

/// Converts a panicking handler into a response instead of dropping the
/// connection with nothing - before this, a panic anywhere in a handler
/// meant that one request just failed silently, with no framework-level
/// response at all.
fn handle_panic(payload: Box<dyn Any + Send + 'static>) -> Response {
    // `downcast` (consuming) rather than `downcast_ref` + `.clone()` for the
    // `String` case - avoids cloning a payload that's about to be dropped
    // anyway; the panic path is cold, but there's no reason to allocate
    // twice when ownership is right there.
    let message = match payload.downcast::<String>() {
        Ok(s) => *s,
        Err(payload) => match payload.downcast_ref::<&str>() {
            Some(s) => s.to_string(),
            None => "unknown panic payload".to_string(),
        },
    };
    // Falls back to `Disabled` if the hook never ran at all (e.g. a test
    // that calls `handle_panic` directly without ever installing it) -
    // the same safe default the hook itself falls back to for an
    // unrecognized `BacktraceStatus`.
    let backtrace = LAST_PANIC_BACKTRACE
        .with(|cell| cell.borrow_mut().take())
        .unwrap_or(error::PanicBacktrace::Disabled);
    error::render_panic(&message, backtrace)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use tower::ServiceExt;

    /// A unit test (not an integration test under `tests/`) specifically
    /// because `handle_panic` is private - only code inside this crate can
    /// reach it directly. This compiles to its own isolated test binary
    /// (there are no other `src/`-local unit tests in this crate today),
    /// so it doesn't race `debug::set()`'s `OnceLock` against the
    /// `tests/error_response_*.rs` integration tests, which each run in
    /// their own separate process anyway.
    ///
    /// Only covers the production-mode (default, unset) branch - flipping
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
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        // No `error_pages::set()` call anywhere in this test binary, so
        // this exercises (and pins) the built-in default page, not an
        // app-registered override - see `error_pages::default_internal_html`.
        assert_eq!(body, crate::default_internal_html());
        assert!(
            !body.contains("boom"),
            "panic message must not leak: {body}"
        );
    }

    #[tokio::test]
    async fn laravel_style_health_handler_returns_a_status_page() {
        let response = health().await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/html; charset=utf-8"
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("Application up"));
        assert!(body.contains("HTTP request received. Response rendered in"));
        assert!(body.contains("performance.now()"));
    }

    /// Exercises the exact `.fallback_service(ServeDir::new(...)
    /// .not_found_service(...))` pattern `serve()` wires up - against a
    /// `tempfile::tempdir()` rather than the real, hardcoded `"public"`
    /// path (relative to the process's CWD, not something a unit test
    /// should depend on), so this proves the underlying tower-http
    /// integration behaves correctly without needing to touch the real
    /// filesystem convention. Only covers the literal, byte-identical-path
    /// case - see the comment above `fallback_service` in `serve()` for the
    /// percent-encoded-path caveat this doesn't (and can't easily) pin
    /// without depending on tower-http/axum internals more closely than a
    /// unit test here should.
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
            .fallback_service(
                ServeDir::new(dir.path()).not_found_service(tower::service_fn(
                    |_req: axum::extract::Request| async {
                        Ok::<_, std::convert::Infallible>(AppError::NotFound.into_response())
                    },
                )),
            );

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
        // is handled by the route - `fallback_service` is only ever
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

        // A path matching neither a route nor a file now gets Larust's own
        // styled 404 (via `AppError::NotFound`), not `ServeDir`'s bare,
        // empty-body one - the real gap this whole change closes.
        let missing_response = router
            .oneshot(
                Request::get("/does-not-exist.css")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing_response.status(), StatusCode::NOT_FOUND);
        let missing_bytes = axum::body::to_bytes(missing_response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(missing_bytes.to_vec()).unwrap(),
            crate::default_not_found_html()
        );
    }

    /// Guards every test below that reads or sets `LOG_CHANNEL`/
    /// `LARUST_STDIO_DETACHED` - real process-wide state, not per-test
    /// isolated, which `cargo test`'s default parallel execution would
    /// otherwise race (same reasoning, same `OnceLock`-backed `std::sync::
    /// Mutex` pattern, as `dev_reload.rs`'s own `test_lock` - see that
    /// module's doc comment for the fuller explanation of why a bare
    /// `std::sync::Mutex` is fine here specifically because these tests
    /// hold the guard across no `.await` point at all).
    fn env_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    #[test]
    fn defaults_log_channel_to_file_when_stdio_is_detached_and_the_user_never_set_one() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::remove_var("LOG_CHANNEL");
        std::env::set_var(STDIO_DETACHED_ENV, "1");

        let mut config = Config::from_value(&serde_json::json!({})).unwrap();
        assert_eq!(
            config.log_channel, "stdout",
            "sanity check on Config's own default"
        );
        default_log_channel_to_file_if_stdio_detached(&mut config);
        assert_eq!(config.log_channel, "file");

        std::env::remove_var(STDIO_DETACHED_ENV);
    }

    /// The actual reason this checks the real environment instead of
    /// trusting `config.log_channel` - see `default_log_channel_to_file_if_
    /// stdio_detached`'s own doc comment. An explicit `LOG_CHANNEL=stdout`
    /// happens to be the same string `Config`'s own default already
    /// produces, but the user's explicit choice must still win.
    #[test]
    fn does_not_override_an_explicitly_set_log_channel_even_if_it_matches_the_default() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::set_var("LOG_CHANNEL", "stdout");
        std::env::set_var(STDIO_DETACHED_ENV, "1");

        let mut config = Config::from_value(&serde_json::json!({})).unwrap();
        default_log_channel_to_file_if_stdio_detached(&mut config);
        assert_eq!(config.log_channel, "stdout");

        std::env::remove_var("LOG_CHANNEL");
        std::env::remove_var(STDIO_DETACHED_ENV);
    }

    #[test]
    fn does_nothing_on_an_ordinary_boot_where_stdio_was_never_detached() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::remove_var("LOG_CHANNEL");
        std::env::remove_var(STDIO_DETACHED_ENV);

        let mut config = Config::from_value(&serde_json::json!({})).unwrap();
        default_log_channel_to_file_if_stdio_detached(&mut config);
        assert_eq!(
            config.log_channel, "stdout",
            "an ordinary cargo run/xr dev boot must be completely unaffected"
        );
    }

    #[test]
    fn is_supported_session_driver_accepts_exactly_the_two_real_values() {
        assert!(is_supported_session_driver("database"));
        assert!(is_supported_session_driver("file"));
    }

    #[test]
    fn is_supported_session_driver_rejects_a_laravel_value_this_framework_does_not_implement() {
        // Real Laravel `SESSION_DRIVER` values this codebase deliberately
        // doesn't support - see `larust_http::session`'s own module doc
        // comment for why there's still no in-memory ("array") option even
        // now that "file" is real.
        for unsupported in ["array", "redis", "cookie", "memcached", "dynamodb"] {
            assert!(
                !is_supported_session_driver(unsupported),
                "{unsupported:?} must not be treated as a supported driver"
            );
        }
    }

    #[test]
    fn is_supported_session_driver_rejects_an_empty_or_typo_d_value() {
        assert!(!is_supported_session_driver(""));
        assert!(!is_supported_session_driver("Database")); // wrong case
        assert!(!is_supported_session_driver("fiel")); // typo
    }
}
