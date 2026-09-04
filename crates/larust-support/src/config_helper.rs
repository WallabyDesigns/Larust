use larust_core::Config;

/// Laravel-shaped `config('app.name')` string lookup - a deliberate,
/// isolated exception to this framework's usual compile-checked config
/// access (`app.config().app_name`, still available and still preferred
/// for anything statically known); kept narrow (one match arm per known
/// key) rather than a general dynamic-config system.
pub fn config(key: &str) -> Option<String> {
    lookup(larust_core::config(), key)
}

/// The actual key→value mapping, factored out from `config()` so it's
/// testable against a manually-built `Config` without touching
/// `larust_core::config()`'s process-wide `OnceLock` (only
/// `Application::new()` can populate that, once per process - not
/// practical to exercise per-test-case here).
///
/// Deliberately excludes `mail.username`/`mail.password` - this helper is
/// reachable from a `{{ }}` template interpolation, and a credential is
/// one accidental `{{ config("mail.password") }}` away from being
/// rendered into a page. `app.config().mail_password` (the compile-checked
/// path) stays available for anything that legitimately needs it.
fn lookup(cfg: &Config, key: &str) -> Option<String> {
    match key {
        "app.name" => Some(cfg.app_name.clone()),
        "app.env" => Some(cfg.app_env.clone()),
        "app.url" => Some(cfg.app_url.clone()),
        "app.port" => Some(cfg.app_port.to_string()),
        "app.debug" => Some(cfg.app_debug.to_string()),
        "session.secure_cookie" => Some(cfg.session_secure_cookie.to_string()),
        "mail.driver" => Some(cfg.mail_driver.clone()),
        "mail.host" => Some(cfg.mail_host.clone()),
        "mail.port" => Some(cfg.mail_port.to_string()),
        "mail.from_address" => Some(cfg.mail_from_address.clone()),
        "mail.from_name" => Some(cfg.mail_from_name.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> Config {
        Config {
            app_name: "Test App".to_string(),
            app_env: "testing".to_string(),
            app_port: 9999,
            session_secure_cookie: false,
            app_debug: true,
            app_url: "http://example.test".to_string(),
            api_prefix: "/api".to_string(),
            mail_driver: "log".to_string(),
            mail_host: "smtp.example.test".to_string(),
            mail_port: 2525,
            mail_username: "user".to_string(),
            mail_password: "secret".to_string(),
            mail_encryption: "tls".to_string(),
            mail_from_address: "hello@example.test".to_string(),
            mail_from_name: "Test App".to_string(),
            cache_driver: "database".to_string(),
            queue_driver: "database".to_string(),
        }
    }

    #[test]
    fn returns_known_keys() {
        let cfg = test_config();
        assert_eq!(lookup(&cfg, "app.name"), Some("Test App".to_string()));
        assert_eq!(lookup(&cfg, "app.env"), Some("testing".to_string()));
        assert_eq!(
            lookup(&cfg, "app.url"),
            Some("http://example.test".to_string())
        );
        assert_eq!(lookup(&cfg, "app.port"), Some("9999".to_string()));
        assert_eq!(lookup(&cfg, "app.debug"), Some("true".to_string()));
        assert_eq!(
            lookup(&cfg, "session.secure_cookie"),
            Some("false".to_string())
        );
        assert_eq!(lookup(&cfg, "mail.driver"), Some("log".to_string()));
        assert_eq!(
            lookup(&cfg, "mail.host"),
            Some("smtp.example.test".to_string())
        );
        assert_eq!(lookup(&cfg, "mail.port"), Some("2525".to_string()));
        assert_eq!(
            lookup(&cfg, "mail.from_address"),
            Some("hello@example.test".to_string())
        );
        assert_eq!(lookup(&cfg, "mail.from_name"), Some("Test App".to_string()));
    }

    #[test]
    fn does_not_expose_mail_credentials() {
        let cfg = test_config();
        assert_eq!(lookup(&cfg, "mail.username"), None);
        assert_eq!(lookup(&cfg, "mail.password"), None);
    }

    #[test]
    fn returns_none_for_an_unknown_key() {
        assert_eq!(lookup(&test_config(), "app.nonexistent"), None);
    }
}
