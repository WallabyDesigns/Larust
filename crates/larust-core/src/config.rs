use crate::AppError;
use serde::Deserialize;
use std::sync::OnceLock;

static CONFIG: OnceLock<Config> = OnceLock::new();

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default = "default_app_name")]
    pub app_name: String,
    #[serde(default = "default_app_env")]
    pub app_env: String,
    #[serde(default = "default_app_port")]
    pub app_port: u16,
    /// Whether the session cookie carries the `Secure` attribute. Defaults
    /// to `true` (safe over any real deployment). Browsers only treat
    /// loopback addresses and the literal name `localhost` as secure
    /// contexts over plain HTTP — a custom local dev hostname (e.g. a
    /// `.test` domain resolved via `/etc/hosts`, even one that points at
    /// 127.0.0.1) is not on that list, so the `Secure` cookie is silently
    /// dropped by the browser and sessions/CSRF stop working with no error
    /// surfaced anywhere. Set `SESSION_SECURE_COOKIE=false` for that case.
    #[serde(default = "default_session_secure_cookie")]
    pub session_secure_cookie: bool,
    /// Gates descriptive error pages (the full error message and source
    /// chain, rendered as HTML) and panic details. Defaults to `false` —
    /// safe if unset, so a deployment missing both `.env` and its own
    /// `config/app.rs`'s `APP_DEBUG` handling never leaks internals by
    /// accident. Scaffolded apps ship `APP_DEBUG=true` in their own
    /// `.env` for local dev, mirroring Laravel's own scaffold convention.
    #[serde(default = "default_app_debug")]
    pub app_debug: bool,
    /// The app's own base URL, for `larust_support::url()`/`asset()` to
    /// build absolute URLs from a relative path. Defaults to
    /// `"http://localhost"` — matching Laravel's own scaffolded default
    /// exactly (no port; most local dev never needs `url()` to be
    /// port-precise). Set `APP_URL` for anything that does.
    #[serde(default = "default_app_url")]
    pub app_url: String,
    /// Where `routes/api.rs` gets mounted (`main.rs`'s
    /// `.group(&config.api_prefix, ...)` call) — Laravel's own
    /// `routes/api.php` is likewise served under a configurable prefix
    /// (`RouteServiceProvider`'s `apiPrefix`), not a fixed one. Defaults to
    /// `"/api"`.
    #[serde(default = "default_api_prefix")]
    pub api_prefix: String,
    /// `"log"` (default) writes a mail's rendered subject/body to
    /// `tracing::info!` instead of sending it — no network touched, no
    /// SMTP server needed for local dev or `cargo test`, matching
    /// Laravel's own `MAIL_MAILER=log` scaffold default exactly. `"smtp"`
    /// sends for real, using the fields below.
    #[serde(default = "default_mail_driver")]
    pub mail_driver: String,
    #[serde(default = "default_mail_host")]
    pub mail_host: String,
    #[serde(default = "default_mail_port")]
    pub mail_port: u16,
    /// Empty string means "unset" — `Config` has no `Option<T>` field
    /// precedent elsewhere, and the `log` driver (the default) never
    /// reads these anyway.
    #[serde(default = "default_mail_username")]
    pub mail_username: String,
    #[serde(default = "default_mail_password")]
    pub mail_password: String,
    #[serde(default = "default_mail_encryption")]
    pub mail_encryption: String,
    #[serde(default = "default_mail_from_address")]
    pub mail_from_address: String,
    /// Falls back to `app_name` if unset, matching Laravel's own
    /// `MAIL_FROM_NAME="${APP_NAME}"` scaffold default.
    #[serde(default)]
    pub mail_from_name: String,
}

fn default_app_name() -> String {
    "Larust".to_string()
}

fn default_app_env() -> String {
    "local".to_string()
}

fn default_app_port() -> u16 {
    8000
}

fn default_session_secure_cookie() -> bool {
    true
}

fn default_app_debug() -> bool {
    false
}

fn default_app_url() -> String {
    "http://localhost".to_string()
}

fn default_api_prefix() -> String {
    "/api".to_string()
}

fn default_mail_driver() -> String {
    "log".to_string()
}

fn default_mail_host() -> String {
    "127.0.0.1".to_string()
}

fn default_mail_port() -> u16 {
    587
}

fn default_mail_username() -> String {
    String::new()
}

fn default_mail_password() -> String {
    String::new()
}

fn default_mail_encryption() -> String {
    "tls".to_string()
}

fn default_mail_from_address() -> String {
    "hello@example.com".to_string()
}

impl Config {
    /// Builds `Config` from `value` — the `serde_json::Value` an app's own
    /// generated `config/app.rs` (`pub fn config() -> Value`) produces. A
    /// single `serde_json::from_value` call: `Config` already derives
    /// `Deserialize` with a `#[serde(default = ...)]` per field, which
    /// works identically regardless of the source `Deserializer` (this
    /// used to be TOML, read from `config/app.toml` — see this crate's
    /// git history), so no manual field-by-field extraction is needed
    /// here. Env-var override capability (Laravel's own "config file sets
    /// a default, `.env` can override it" behavior) lives entirely in the
    /// generated `config/app.rs`'s own `env_or`/`env_bool` calls now —
    /// this function has no knowledge of environment variables at all,
    /// unlike the TOML-era `load_from` it replaced.
    pub fn from_value(value: &serde_json::Value) -> Result<Self, AppError> {
        serde_json::from_value(value.clone()).map_err(|source| AppError::Config(Box::new(source)))
    }

    /// Stores `self` as the process-wide config (`config()` below reads it
    /// back) — called once, from `Application::new()`, right after
    /// `load()` succeeds. A second call (e.g. `Application::new()` running
    /// more than once in the same process, such as a test suite exercising
    /// several `APP_URL`/`APP_ENV` values) doesn't panic or overwrite —
    /// `OnceLock` can only be set once — but every `url()`/`asset()`/
    /// `larust_support::config()` call afterward keeps resolving against
    /// the *first* call's values, silently wrong rather than reflecting
    /// what the second `Application::new()` actually loaded. Worth
    /// surfacing rather than swallowing outright, matching
    /// `larust_http::route::publish_route_names`'s identical
    /// first-writer-wins tradeoff.
    pub(crate) fn publish(self) {
        if CONFIG.set(self).is_err() {
            tracing::warn!(
                "Application::new() called more than once in this process; \
                 config(), url(), and asset() still use the first call's values"
            );
        }
    }
}

/// Returns the process-wide config `Application::new()` already loaded —
/// the same `OnceLock`-backed idiom `larust_orm::pool()` uses for the
/// connection pool. Unlike `pool()`, this panics rather than returning a
/// `Result` if called before `Application::new()`: every Larust
/// entry point calls `Application::new()` as its first line (there's no
/// analogue to `pool()`'s "forgot to call `connect()` later" scenario —
/// nothing before `Application::new()` could plausibly need config at
/// all), so treating this as a real caller-contract violation (like
/// `abort()`'s own documented panic for an invalid status code) rather
/// than a `Result` every call site would need to unwrap anyway is the
/// better fit here.
///
/// Shares its name with the unrelated, one-argument
/// `larust_support::config(key)` (Laravel's stringly-typed
/// `config('app.name')`) — a `use larust_core::config;` alongside
/// `use larust_support::config;` in the same file is a duplicate-import
/// error. Call this one by its full path (`larust_core::config()`, as
/// every call site in this codebase already does) rather than importing
/// it bare if a file needs both.
pub fn config() -> &'static Config {
    CONFIG
        .get()
        .expect("larust_core::config() called before Application::new()")
}
