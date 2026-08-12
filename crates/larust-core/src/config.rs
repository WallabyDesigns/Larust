use crate::AppError;
use serde::Deserialize;
use std::path::Path;
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
    /// safe if unset, so a deployment missing both `.env` and
    /// `config/app.toml` never leaks internals by accident. Scaffolded
    /// apps ship `APP_DEBUG=true` in their own `.env` for local dev,
    /// mirroring Laravel's own scaffold convention.
    #[serde(default = "default_app_debug")]
    pub app_debug: bool,
    /// The app's own base URL, for `larust_support::url()`/`asset()` to
    /// build absolute URLs from a relative path. Defaults to
    /// `"http://localhost"` — matching Laravel's own scaffolded default
    /// exactly (no port; most local dev never needs `url()` to be
    /// port-precise). Set `APP_URL` for anything that does.
    #[serde(default = "default_app_url")]
    pub app_url: String,
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
    /// Loads `.env` (if present) then `config/app.toml` (if present),
    /// relative to the current working directory — Larust apps are
    /// expected to run from their project root, matching Laravel's
    /// `artisan serve` convention. `APP_ENV`/`APP_PORT`/
    /// `SESSION_SECURE_COOKIE`/`APP_DEBUG` environment variables each take
    /// final precedence over both files, field by field.
    pub fn load() -> Result<Self, AppError> {
        dotenvy::dotenv().ok();

        let path = Path::new("config/app.toml");
        let raw = if path.exists() {
            std::fs::read_to_string(path).map_err(|source| AppError::Config(Box::new(source)))?
        } else {
            String::new()
        };

        let mut config: Config =
            toml::from_str(&raw).map_err(|source| AppError::Config(Box::new(source)))?;

        if let Ok(env) = std::env::var("APP_ENV") {
            config.app_env = env;
        }
        if let Ok(port) = std::env::var("APP_PORT") {
            config.app_port = port
                .parse()
                .map_err(|source| AppError::Config(Box::new(source)))?;
        }
        if let Ok(secure) = std::env::var("SESSION_SECURE_COOKIE") {
            config.session_secure_cookie = secure
                .parse()
                .map_err(|source| AppError::Config(Box::new(source)))?;
        }
        if let Ok(debug) = std::env::var("APP_DEBUG") {
            config.app_debug = debug
                .parse()
                .map_err(|source| AppError::Config(Box::new(source)))?;
        }
        if let Ok(url) = std::env::var("APP_URL") {
            config.app_url = url;
        }
        if let Ok(driver) = std::env::var("MAIL_DRIVER") {
            config.mail_driver = driver;
        }
        if let Ok(host) = std::env::var("MAIL_HOST") {
            config.mail_host = host;
        }
        if let Ok(port) = std::env::var("MAIL_PORT") {
            config.mail_port = port
                .parse()
                .map_err(|source| AppError::Config(Box::new(source)))?;
        }
        if let Ok(username) = std::env::var("MAIL_USERNAME") {
            config.mail_username = username;
        }
        if let Ok(password) = std::env::var("MAIL_PASSWORD") {
            config.mail_password = password;
        }
        if let Ok(encryption) = std::env::var("MAIL_ENCRYPTION") {
            config.mail_encryption = encryption;
        }
        if let Ok(from_address) = std::env::var("MAIL_FROM_ADDRESS") {
            config.mail_from_address = from_address;
        }
        if let Ok(from_name) = std::env::var("MAIL_FROM_NAME") {
            config.mail_from_name = from_name;
        }
        if config.mail_from_name.is_empty() {
            config.mail_from_name = config.app_name.clone();
        }

        Ok(config)
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
