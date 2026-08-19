//! Runtime `env`/`env_bool`/`env_or` helpers for generated `config/*.rs`
//! modules (see `larust_convert::config::convert_body`) — the
//! runtime half of every Laravel `env('VAR')`/`env('VAR', default)` call
//! a config file's own generated `pub fn config() -> serde_json::Value`
//! references. Kept in `larust-support` (not `larust-core`) for the same
//! "app-facing helper, not framework internals" reason `config_helper::
//! config` lives here — this is a distinct, narrower mechanism from that
//! one: `config_helper::config` resolves a *fixed*, hand-curated set of
//! `Config`-struct-backed keys; this module is the raw env-var read a
//! *generated* config file's own arbitrary keys fall back to.

/// `std::env::var(key)`, defaulting to an empty string when unset — the
/// same PHP-`null`-becomes-empty-`String` convention `larust-convert`'s
/// Blade expression translator already uses elsewhere (see
/// `larust_convert`'s `blade::expr::translate_null_branch_ternary`),
/// chosen so a bare `env('VAR')` (no Laravel-side default) composes
/// uniformly wherever a generated config value can appear — directly as
/// a `json!()` value, or concatenated via `format!(...)`.
pub fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_default()
}

/// `env(...)`'s boolean counterpart — parses the env var as a `bool`
/// (`"true"`/`"false"`), falling back to `default` when the variable is
/// unset or fails to parse as a bool (a malformed value shouldn't panic
/// a config read, matching PHP's own tolerant `env('VAR', false)`).
pub fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// `env(...)`'s string-default counterpart — `env('VAR', 'fallback')`.
/// Treats an unset *or* empty-string variable as "use the default",
/// since [`env`] already collapses "unset" to `""` and there's no way to
/// tell the two apart afterward — an accepted imprecision against PHP's
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
}
