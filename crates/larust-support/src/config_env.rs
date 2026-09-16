//! Runtime `env`/`env_bool`/`env_or` helpers for generated `config/*.rs`
//! modules (see `larust_convert::config::convert_body`) - the
//! runtime half of every Laravel `env('VAR')`/`env('VAR', default)` call
//! a config file's own generated `pub fn config() -> serde_json::Value`
//! references. Kept in `larust-support` (not `larust-core`) for the same
//! "app-facing helper, not framework internals" reason `config_helper::
//! config` lives here - this is a distinct, narrower mechanism from that
//! one: `config_helper::config` resolves a *fixed*, hand-curated set of
//! `Config`-struct-backed keys; this module is the raw env-var read a
//! *generated* config file's own arbitrary keys fall back to.

/// `std::env::var(key)`, defaulting to an empty string when unset - the
/// same PHP-`null`-becomes-empty-`String` convention `larust-convert`'s
/// Blade expression translator already uses elsewhere (see
/// `larust_convert`'s `blade::expr::translate_null_branch_ternary`),
/// chosen so a bare `env('VAR')` (no Laravel-side default) composes
/// uniformly wherever a generated config value can appear - directly as
/// a `json!()` value, or concatenated via `format!(...)`.
pub fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_default()
}

/// `env(...)`'s boolean counterpart - parses the env var as a `bool`
/// (`"true"`/`"false"`), falling back to `default` when the variable is
/// unset or fails to parse as a bool (a malformed value shouldn't panic
/// a config read, matching PHP's own tolerant `env('VAR', false)`).
pub fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// `env(...)`'s string-default counterpart - `env('VAR', 'fallback')`.
/// Treats an unset *or* empty-string variable as "use the default",
/// since [`env`] already collapses "unset" to `""` and there's no way to
/// tell the two apart afterward - an accepted imprecision against PHP's
/// own `env()` (which really does distinguish "unset" from "set to
/// empty"), not worth a separate `Option`-returning variant for.
pub fn env_or(key: &str, default: &str) -> String {
    let value = env(key);
    if value.is_empty() {
        default.to_string()
    } else {
        value
    }
}

/// Extracts a bare `u16` port from a URL's authority, when it has one
/// explicitly (`"http://127.0.0.1:1234"` -> `Some(1234)`, `"http://
/// localhost"` -> `None`). Deliberately not a full RFC 3986 parser -
/// strips an optional `scheme://`, takes everything up to the first `/`,
/// `?`, or `#`, then splits the remaining authority on its last `:` -
/// covers the one real shape this is ever used against (see
/// [`app_port_or`]'s own doc comment): a plain `host[:port]` `APP_URL`,
/// never one carrying userinfo or an IPv6 literal authority.
pub fn port_from_url(url: &str) -> Option<u16> {
    let without_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority_end = without_scheme
        .find(['/', '?', '#'])
        .unwrap_or(without_scheme.len());
    let (_, port) = without_scheme[..authority_end].rsplit_once(':')?;
    port.parse().ok()
}

/// Pure precedence logic behind [`app_port_or`], split out so it's
/// unit-testable without real env-var I/O - same "split out the pure
/// logic" precedent `larust-cli`'s own `resolve_app_port`/`dev_config`
/// already establishes for the identical `xr dev`-side problem.
fn resolve_app_port(raw_app_port: &str, raw_app_url: &str, default: u16) -> u16 {
    if let Ok(port) = raw_app_port.parse() {
        return port;
    }
    if let Some(port) = port_from_url(raw_app_url) {
        return port;
    }
    default
}

/// `APP_PORT`'s own resolution chain - one step more lenient than a plain
/// `env_or("APP_PORT", ...).parse().unwrap_or(default)`: prefers
/// `APP_PORT` if it's set to a real `u16`, falls back to a port parsed out
/// of `APP_URL` if *that's* set and has one, and only then `default`.
/// Requested directly, after a real `xr convert`-ed project produced a
/// surprising port: a Laravel project's own `.env` commonly encodes its
/// intended dev port only in `APP_URL` (`APP_URL=http://127.0.0.1:8000`) -
/// Artisan's own `serve` command never actually reads `APP_URL` for its
/// `--port` default, so a real Laravel project never needed the two kept
/// in sync, and `xr convert` only ever carries over the source app's real
/// `.env` values (it never invents an `APP_PORT` line the original didn't
/// have). Without this, such a converted app silently landed on this
/// framework's own generic default port instead of the one its own
/// `APP_URL` already, if only incidentally, documented.
pub fn app_port_or(default: u16) -> u16 {
    resolve_app_port(&env("APP_PORT"), &env("APP_URL"), default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_returns_empty_string_for_an_unset_variable() {
        assert_eq!(env("LARUST_CONFIG_ENV_TEST_UNSET_VAR"), "");
    }

    #[test]
    fn env_bool_falls_back_to_default_when_unset() {
        assert!(env_bool("LARUST_CONFIG_ENV_TEST_UNSET_BOOL", true));
        assert!(!env_bool("LARUST_CONFIG_ENV_TEST_UNSET_BOOL", false));
    }

    #[test]
    fn env_or_falls_back_to_default_when_unset() {
        assert_eq!(
            env_or("LARUST_CONFIG_ENV_TEST_UNSET_STRING", "fallback"),
            "fallback"
        );
    }

    #[test]
    fn port_from_url_extracts_an_explicit_port() {
        assert_eq!(port_from_url("http://127.0.0.1:1234"), Some(1234));
        assert_eq!(port_from_url("https://example.com:8443/path"), Some(8443));
        assert_eq!(port_from_url("127.0.0.1:8000"), Some(8000));
    }

    #[test]
    fn port_from_url_returns_none_without_an_explicit_port() {
        assert_eq!(port_from_url("http://localhost"), None);
        assert_eq!(port_from_url("http://example.com/path"), None);
        assert_eq!(port_from_url(""), None);
    }

    #[test]
    fn port_from_url_ignores_an_invalid_port() {
        assert_eq!(port_from_url("http://localhost:not-a-port"), None);
    }

    #[test]
    fn resolve_app_port_prefers_an_explicit_app_port() {
        assert_eq!(
            resolve_app_port("1234", "http://127.0.0.1:5678", 34187),
            1234
        );
    }

    #[test]
    fn resolve_app_port_falls_back_to_the_url_port_when_app_port_is_unset() {
        assert_eq!(resolve_app_port("", "http://127.0.0.1:5678", 34187), 5678);
    }

    #[test]
    fn resolve_app_port_falls_back_to_the_default_when_neither_is_usable() {
        assert_eq!(resolve_app_port("", "http://localhost", 34187), 34187);
        assert_eq!(resolve_app_port("not-a-port", "not-a-url", 34187), 34187);
    }
}
