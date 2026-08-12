use crate::{debug, dev_reload, error, AppError, Config};
use axum::http::{header, HeaderValue};
use axum::response::Response;
use axum::Router;
use std::any::Any;
use std::net::SocketAddr;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;
use tracing_subscriber::EnvFilter;

pub struct Application {
    config: Config,
    router: Router,
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
        let config = Config::load()?;
        init_logging(&config);
        debug::set(config.app_debug);
        config.clone().publish();

        Ok(Self {
            config,
            router: Router::new(),
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

    /// Binds to `config.app_port` on localhost and serves until the process
    /// is terminated.
    pub async fn serve(self) -> Result<(), AppError> {
        let addr = SocketAddr::from(([127, 0, 0, 1], self.config.app_port));
        tracing::info!(%addr, app = %self.config.app_name, env = %self.config.app_env, "starting server");

        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|source| AppError::Internal(Box::new(source)))?;

        // Set only on the child process `xr dev` spawns itself — never on a
        // plain `cargo run`, and never touched by any generated app code.
        // `.route(...)` panics on an exact-path collision with a route the
        // app already registered — acceptable here given how unlikely a
        // real app is to independently choose the `__larust_dev` path, but
        // worth knowing if this route's name ever needs to change.
        let router = if std::env::var_os("LARUST_DEV_RELOAD").is_some() {
            self.router
                .route("/__larust_dev", axum::routing::get(dev_reload::handler))
        } else {
            self.router
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
        let router = router.fallback_service(ServeDir::new("public"));

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

        let router = router.layer(CatchPanicLayer::custom(handle_panic));

        axum::serve(listener, router)
            .await
            .map_err(|source| AppError::Internal(Box::new(source)))?;

        Ok(())
    }
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
