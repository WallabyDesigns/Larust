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
    /// contexts over plain HTTP - a custom local dev hostname (e.g. a
    /// `.test` domain resolved via `/etc/hosts`, even one that points at
    /// 127.0.0.1) is not on that list, so the `Secure` cookie is silently
    /// dropped by the browser and sessions/CSRF stop working with no error
    /// surfaced anywhere. Set `SESSION_SECURE_COOKIE=false` for that case.
    #[serde(default = "default_session_secure_cookie")]
    pub session_secure_cookie: bool,
    /// Gates descriptive error pages (the full error message and source
    /// chain, rendered as HTML) and panic details. Defaults to `false` -
    /// safe if unset, so a deployment missing both `.env` and its own
    /// `config/app.rs`'s `APP_DEBUG` handling never leaks internals by
    /// accident. Scaffolded apps ship `APP_DEBUG=true` in their own
    /// `.env` for local dev, mirroring Laravel's own scaffold convention.
    #[serde(default = "default_app_debug")]
    pub app_debug: bool,
    /// The app's own base URL, for `larust_support::url()`/`asset()` to
    /// build absolute URLs from a relative path. Defaults to
    /// `"http://localhost"` - matching Laravel's own scaffolded default
    /// exactly (no port; most local dev never needs `url()` to be
    /// port-precise). Set `APP_URL` for anything that does.
    #[serde(default = "default_app_url")]
    pub app_url: String,
    /// `larust_lang::current_locale`'s own default when no per-request
    /// override is set (see that crate's own doc comment) - Laravel's
    /// `config('app.locale')`/`APP_LOCALE`. Defaults to `"en"`.
    #[serde(default = "default_app_locale")]
    pub app_locale: String,
    /// `larust_lang::t`/`t_with`'s fallback when a key is missing from the
    /// current locale's own translation file - Laravel's
    /// `config('app.fallback_locale')`/`APP_FALLBACK_LOCALE`. Defaults to
    /// `"en"`, same as [`app_locale`](Self::app_locale) - most apps only
    /// ever have one locale until they add a second, at which point this
    /// is usually still the original one.
    #[serde(default = "default_app_fallback_locale")]
    pub app_fallback_locale: String,
    /// Where `routes/api.rs` gets mounted (`main.rs`'s
    /// `.group(&config.api_prefix, ...)` call) - Laravel's own
    /// `routes/api.php` is likewise served under a configurable prefix
    /// (`RouteServiceProvider`'s `apiPrefix`), not a fixed one. Defaults to
    /// `"/api"`.
    #[serde(default = "default_api_prefix")]
    pub api_prefix: String,
    /// `"log"` (default) writes a mail's rendered subject/body to
    /// `tracing::info!` instead of sending it - no network touched, no
    /// SMTP server needed for local dev or `cargo test`, matching
    /// Laravel's own `MAIL_MAILER=log` scaffold default exactly. `"smtp"`
    /// sends for real, using the fields below.
    #[serde(default = "default_mail_driver")]
    pub mail_driver: String,
    #[serde(default = "default_mail_host")]
    pub mail_host: String,
    #[serde(default = "default_mail_port")]
    pub mail_port: u16,
    /// Empty string means "unset" - `Config` has no `Option<T>` field
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
    /// `"database"` (default) stores `larust-cache`'s entries in
    /// `cache_items`, the same SQL-family table it has always used.
    /// `"redis"` stores them in Redis instead - see `larust-cache::store`'s
    /// own doc comment for the split. Same `mail_driver`-shaped "a plain
    /// string picks a runtime code path" convention, not a typed enum:
    /// `larust-core` has no `sqlx`/`redis` dependency and shouldn't need
    /// one just to name a driver.
    #[serde(default = "default_cache_driver")]
    pub cache_driver: String,
    /// `larust-queue`'s own driver toggle - see [`cache_driver`](Self::cache_driver)'s
    /// own doc comment for the shape and reasoning; independent of it
    /// (an app can mix a database-backed cache with a Redis-backed queue,
    /// or vice versa).
    #[serde(default = "default_queue_driver")]
    pub queue_driver: String,
    /// `"stdout"` (default) - everything goes to the terminal via
    /// `tracing_subscriber::fmt`, exactly as before this field existed.
    /// `"file"` writes to `storage/logs/larust.log` instead (rotated - see
    /// [`log_max_size`](Self::log_max_size)); `"stack"` writes to both, the
    /// same "combine channels" meaning Laravel's own `LOG_CHANNEL=stack`
    /// has. Laravel additionally supports `single`/`daily`/`slack`/etc.
    /// channels this doesn't attempt - see `crate::logging`'s own module
    /// doc comment for why size-based rotation was the one strategy
    /// actually built.
    #[serde(default = "default_log_channel")]
    pub log_channel: String,
    /// Empty string means "unset" (same convention as
    /// [`mail_username`](Self::mail_username)) - falls back to this
    /// crate's own historical behavior: `debug,sqlx=warn,tower_sessions=warn`
    /// in `app_env == "local"`, `info` otherwise. Set to a plain level name
    /// (`trace`/`debug`/`info`/`warn`/`error`) to override that for every
    /// crate at once; `sqlx`/`tower_sessions` are still individually capped
    /// at `warn` even then, for the same reason `init_logging`'s own
    /// hardcoded default already did - see `crate::logging`'s doc comment.
    /// `RUST_LOG`, if set, wins over this unconditionally, same as it
    /// already won over the old hardcoded default.
    #[serde(default)]
    pub log_level: String,
    /// Bytes - once `storage/logs/larust.log` reaches this size, it's
    /// rotated to `larust.log.1` (shifting any existing `.1`/`.2`/... down
    /// one) and a fresh, empty file is started. Only consulted when
    /// [`log_channel`](Self::log_channel) is `"file"`/`"stack"`. Defaults
    /// to 10 MiB.
    #[serde(default = "default_log_max_size")]
    pub log_max_size: u64,
    /// How many rotated backups (`larust.log.1` through `larust.log.{N}`)
    /// to keep before the oldest is deleted outright. `0` means "delete
    /// immediately on rotation, keep only the current file." Defaults to
    /// `5`.
    #[serde(default = "default_log_keep_files")]
    pub log_keep_files: u32,
    /// `"web"` (default) - an ordinary server, deployed via `xr deploy`'s
    /// build-and-restart-handoff path. `"app"` - a Tauri desktop build,
    /// where the app's own `Application`/router is spawned in-process and
    /// a native webview points at it locally instead of a browser
    /// connecting over the network. Read by `xr deploy`/`xr new` (from
    /// `.env`, not through this struct - see those crates' own doc
    /// comments for why), not by this crate itself; kept on `Config`
    /// mainly so app code (e.g. a template wanting to render different
    /// chrome for a desktop build) can read `app.config().deploy_type`
    /// like any other setting.
    #[serde(default = "default_deploy_type")]
    pub deploy_type: String,
}

fn default_app_name() -> String {
    "Larust".to_string()
}

fn default_app_env() -> String {
    "local".to_string()
}

// 34187 - a small "WALBY" easter egg (a nod to Wallaby Designs), and
// the practical reason: far less likely to already be taken by something
// else on a dev machine than the extremely common 8000/3000/5000 range.
fn default_app_port() -> u16 {
    34187
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

fn default_app_locale() -> String {
    "en".to_string()
}

fn default_app_fallback_locale() -> String {
    "en".to_string()
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

fn default_cache_driver() -> String {
    "database".to_string()
}

fn default_queue_driver() -> String {
    "database".to_string()
}

fn default_deploy_type() -> String {
    "web".to_string()
}

fn default_log_channel() -> String {
    "stdout".to_string()
}

fn default_log_max_size() -> u64 {
    10 * 1024 * 1024
}

fn default_log_keep_files() -> u32 {
    5
}

impl Config {
    /// Builds `Config` from `value` - the `serde_json::Value` an app's own
    /// generated `config/app.rs` (`pub fn config() -> Value`) produces. A
    /// single `serde_json::from_value` call: `Config` already derives
    /// `Deserialize` with a `#[serde(default = ...)]` per field, which
    /// works identically regardless of the source `Deserializer` (this
    /// used to be TOML, read from `config/app.toml` - see this crate's
    /// git history), so no manual field-by-field extraction is needed
    /// here. Env-var override capability (Laravel's own "config file sets
    /// a default, `.env` can override it" behavior) lives entirely in the
    /// generated `config/app.rs`'s own `env_or`/`env_bool` calls now -
    /// this function has no knowledge of environment variables at all,
    /// unlike the TOML-era `load_from` it replaced.
    pub fn from_value(value: &serde_json::Value) -> Result<Self, AppError> {
        serde_json::from_value(value.clone()).map_err(|source| AppError::Config(Box::new(source)))
    }

    /// Stores `self` as the process-wide config (`config()` below reads it
    /// back) - called once, from `Application::new()`, right after
    /// `load()` succeeds. A second call (e.g. `Application::new()` running
    /// more than once in the same process, such as a test suite exercising
    /// several `APP_URL`/`APP_ENV` values) doesn't panic or overwrite -
    /// `OnceLock` can only be set once - but every `url()`/`asset()`/
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

/// Returns the process-wide config `Application::new()` already loaded -
/// the same `OnceLock`-backed idiom `larust_orm::pool()` uses for the
/// connection pool. Unlike `pool()`, this panics rather than returning a
/// `Result` if called before `Application::new()`: every Larust
/// entry point calls `Application::new()` as its first line (there's no
/// analogue to `pool()`'s "forgot to call `connect()` later" scenario -
/// nothing before `Application::new()` could plausibly need config at
/// all), so treating this as a real caller-contract violation (like
/// `abort()`'s own documented panic for an invalid status code) rather
/// than a `Result` every call site would need to unwrap anyway is the
/// better fit here.
///
/// Shares its name with the unrelated, one-argument
/// `larust_support::config(key)` (Laravel's stringly-typed
/// `config('app.name')`) - a `use larust_core::config;` alongside
/// `use larust_support::config;` in the same file is a duplicate-import
/// error. Call this one by its full path (`larust_core::config()`, as
/// every call site in this codebase already does) rather than importing
/// it bare if a file needs both.
pub fn config() -> &'static Config {
    CONFIG
        .get()
        .expect("larust_core::config() called before Application::new()")
}

/// `config()`'s non-panicking twin, for the rare caller that has a
/// sensible fallback behavior for "no `Application::new()` has run yet"
/// rather than treating it as a contract violation - e.g.
/// `larust_http::session`'s cookie-name derivation, which needs to behave
/// identically whether or not the specific test harness building a router
/// happened to construct an `Application` first.
pub fn try_config() -> Option<&'static Config> {
    CONFIG.get()
}
