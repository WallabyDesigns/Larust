use larust_support::serde_json::{json, Value};

/// Demo's own bootstrap config - the hand-written counterpart of what
/// `xr new`/`xr convert` generate automatically (see
/// `larust_cli::config_template::render_app_config_rs`, the shared
/// template this mirrors). Replaces the old `config/app.toml` +
/// `larust_core::Config::load_from`'s TOML-parsing bootstrap: every field
/// is now explicit and `env_or`/`env_bool`-backed, so `.env` still
/// overrides any of them exactly as before, just via a real function call
/// instead of a parsed file.
pub fn config() -> Value {
    let mut config = json!({});

    config["app_name"] = json!(larust_support::config_env::env_or(
        "APP_NAME",
        "Larust Demo"
    ));

    config["app_env"] = json!(larust_support::config_env::env_or("APP_ENV", "local"));

    config["app_port"] = json!(larust_support::config_env::env_or("APP_PORT", "8000")
        .parse::<u16>()
        .unwrap_or(8000));

    config["session_secure_cookie"] = json!(larust_support::config_env::env_bool(
        "SESSION_SECURE_COOKIE",
        true
    ));

    config["app_debug"] = json!(larust_support::config_env::env_bool("APP_DEBUG", false));

    config["app_url"] = json!(larust_support::config_env::env_or(
        "APP_URL",
        "http://localhost"
    ));

    config["api_prefix"] = json!(larust_support::config_env::env_or("API_PREFIX", "/api"));

    config["mail_driver"] = json!(larust_support::config_env::env_or("MAIL_DRIVER", "log"));

    config["mail_host"] = json!(larust_support::config_env::env_or("MAIL_HOST", "127.0.0.1"));

    config["mail_port"] = json!(larust_support::config_env::env_or("MAIL_PORT", "587")
        .parse::<u16>()
        .unwrap_or(587));

    config["mail_username"] = json!(larust_support::config_env::env_or("MAIL_USERNAME", ""));

    config["mail_password"] = json!(larust_support::config_env::env_or("MAIL_PASSWORD", ""));

    config["mail_encryption"] = json!(larust_support::config_env::env_or("MAIL_ENCRYPTION", "tls"));

    config["mail_from_address"] = json!(larust_support::config_env::env_or(
        "MAIL_FROM_ADDRESS",
        "hello@example.com"
    ));

    let mail_from_name = larust_support::config_env::env("MAIL_FROM_NAME");
    config["mail_from_name"] = json!(if mail_from_name.is_empty() {
        config["app_name"].as_str().unwrap_or_default().to_string()
    } else {
        mail_from_name
    });

    config["cache_driver"] = json!(larust_support::config_env::env_or(
        "CACHE_DRIVER",
        "database"
    ));

    config["queue_driver"] = json!(larust_support::config_env::env_or(
        "QUEUE_DRIVER",
        "database"
    ));

    config
}
