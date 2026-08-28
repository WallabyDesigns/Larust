//! The canonical `config/app.rs` template shared by `xr new` (`scaffold.rs`)
//! and `xr convert` (`convert.rs`) — one file per generated app, exposing
//! `pub fn config() -> serde_json::Value`, mirroring the same generated-
//! config-module pattern `larust_convert::config::convert_body` already
//! established for arbitrary (non-bootstrap) config files. Every
//! `larust_core::Config` field is explicit and `env_or`/`env_bool`-backed
//! here (see [`FIELDS`]), since `Config::from_value` no longer applies its
//! own env-var overrides — that capability now lives entirely in this
//! generated code.
//!
//! `xr new` calls [`render_app_config_rs`] with generic literal defaults;
//! `xr convert` calls it with whatever real values `MAPPINGS`
//! (`larust_convert::config`) found in the source Laravel app's own
//! `config/*.php` files, falling back to the same generic defaults for
//! anything not found — either way, every field still resolves through a
//! real `env_or`/`env_bool` call, so `.env` can always override it
//! regardless of what the generated literal default happens to be.

use std::collections::HashMap;

enum FieldKind {
    Str,
    Bool,
    U16,
}

struct Field {
    name: &'static str,
    env_var: &'static str,
    kind: FieldKind,
    /// The same literal `Config` itself falls back to via its own
    /// `#[serde(default = "default_*")]` (see `larust_core::config`'s
    /// `default_*()` functions) — used whenever a caller's own `defaults`
    /// map doesn't supply this field, so "forgot to pass a default for
    /// this one field" degrades to the framework's own sensible default
    /// rather than a silent empty string/`false`/`0`.
    generic_default: &'static str,
}

/// Every `larust_core::Config` field except `mail_from_name`, which gets
/// its own cross-field ("falls back to `app_name` if unset") handling in
/// [`render_app_config_rs`] instead of fitting this uniform shape.
const FIELDS: &[Field] = &[
    Field {
        name: "app_name",
        env_var: "APP_NAME",
        kind: FieldKind::Str,
        generic_default: "\"Larust\"",
    },
    Field {
        name: "app_env",
        env_var: "APP_ENV",
        kind: FieldKind::Str,
        generic_default: "\"local\"",
    },
    Field {
        name: "app_port",
        env_var: "APP_PORT",
        kind: FieldKind::U16,
        generic_default: "8000",
    },
    Field {
        name: "session_secure_cookie",
        env_var: "SESSION_SECURE_COOKIE",
        kind: FieldKind::Bool,
        generic_default: "true",
    },
    Field {
        name: "app_debug",
        env_var: "APP_DEBUG",
        kind: FieldKind::Bool,
        generic_default: "false",
    },
    Field {
        name: "app_url",
        env_var: "APP_URL",
        kind: FieldKind::Str,
        generic_default: "\"http://localhost\"",
    },
    Field {
        name: "api_prefix",
        env_var: "API_PREFIX",
        kind: FieldKind::Str,
        generic_default: "\"/api\"",
    },
    Field {
        name: "mail_driver",
        env_var: "MAIL_DRIVER",
        kind: FieldKind::Str,
        generic_default: "\"log\"",
    },
    Field {
        name: "mail_host",
        env_var: "MAIL_HOST",
        kind: FieldKind::Str,
        generic_default: "\"127.0.0.1\"",
    },
    Field {
        name: "mail_port",
        env_var: "MAIL_PORT",
        kind: FieldKind::U16,
        generic_default: "587",
    },
    Field {
        name: "mail_username",
        env_var: "MAIL_USERNAME",
        kind: FieldKind::Str,
        generic_default: "\"\"",
    },
    Field {
        name: "mail_password",
        env_var: "MAIL_PASSWORD",
        kind: FieldKind::Str,
        generic_default: "\"\"",
    },
    Field {
        name: "mail_encryption",
        env_var: "MAIL_ENCRYPTION",
        kind: FieldKind::Str,
        generic_default: "\"tls\"",
    },
    Field {
        name: "mail_from_address",
        env_var: "MAIL_FROM_ADDRESS",
        kind: FieldKind::Str,
        generic_default: "\"hello@example.com\"",
    },
    Field {
        name: "cache_driver",
        env_var: "CACHE_DRIVER",
        kind: FieldKind::Str,
        generic_default: "\"database\"",
    },
    Field {
        name: "queue_driver",
        env_var: "QUEUE_DRIVER",
        kind: FieldKind::Str,
        generic_default: "\"database\"",
    },
];

/// Renders `config/app.rs`'s full content.
///
/// `defaults` supplies one literal Rust default-value expression per
/// [`FIELDS`] entry, in that field's own kind-appropriate syntax — a
/// quoted string (`"\"log\""`) for [`FieldKind::Str`], bare `true`/`false`
/// for [`FieldKind::Bool`], or bare digits (`"8000"`) for [`FieldKind::U16`].
/// A field missing from `defaults` falls back to that field's own
/// [`Field::generic_default`] — the same literal `larust_core::Config`
/// itself would fall back to — not a silent empty string/`false`/`0`, so
/// a caller that only cares about a few fields (e.g. `xr new`'s `app_name`)
/// never has to enumerate the other dozen just to get sensible values.
///
/// `extra` is appended verbatim after the fixed field set — `xr convert`
/// uses this to fold a Laravel config file's own unmapped keys (e.g.
/// `apiurl`) into this same generated module, each already rendered as a
/// `config["key"] = <expr>;` assignment line by
/// `larust_convert::config::convert_body`.
pub fn render_app_config_rs(defaults: &HashMap<&str, String>, extra: &[String]) -> String {
    let mut body = String::new();
    for field in FIELDS {
        let default = defaults
            .get(field.name)
            .map(String::as_str)
            .unwrap_or(field.generic_default);
        let expr = match field.kind {
            FieldKind::Str => format!(
                "larust_support::config_env::env_or({:?}, {default})",
                field.env_var
            ),
            FieldKind::Bool => format!(
                "larust_support::config_env::env_bool({:?}, {default})",
                field.env_var
            ),
            FieldKind::U16 => {
                format!(
                    "larust_support::config_env::env_or({:?}, {default:?}).parse::<u16>().unwrap_or({default})",
                    field.env_var
                )
            }
        };
        body.push_str(&format!(
            "    config[{:?}] = json!({expr});\n\n",
            field.name
        ));
    }
    body.push_str(
        "    let mail_from_name = larust_support::config_env::env(\"MAIL_FROM_NAME\");\n    \
         config[\"mail_from_name\"] = json!(if mail_from_name.is_empty() { \
         config[\"app_name\"].as_str().unwrap_or_default().to_string() } else { mail_from_name });\n\n",
    );
    for line in extra {
        body.push_str(line);
        body.push_str("\n\n");
    }
    format!(
        "use larust_support::serde_json::{{json, Value}};\n\npub fn config() -> Value {{\n    let mut config = json!({{}});\n\n{body}    config\n}}\n"
    )
}

struct ConnectionSpec {
    /// The key this connection is registered under in
    /// `DatabaseConnections::connections` — also `DB_CONNECTION`'s
    /// expected value to select it.
    name: &'static str,
    /// The `larust_orm::config::Driver` variant literal to embed.
    driver_variant: &'static str,
    default_host: &'static str,
    default_port: &'static str,
    default_charset: &'static str,
}

/// Every named connection Laravel's own `config/database.php` offers,
/// mirrored here — `mariadb` is a real, separate entry (its own name, so
/// `DB_CONNECTION=mariadb` resolves) even though it shares `Driver::MySql`
/// with `mysql` (see `larust_orm::config::Driver::MySql`'s own doc
/// comment on why that's a deliberate alias, not a gap). `sqlite` isn't
/// in this table — it has no host/port/username/password/charset at all,
/// so [`render_database_config_rs`] handles it as its own special case.
const CONNECTIONS: &[ConnectionSpec] = &[
    ConnectionSpec {
        name: "mysql",
        driver_variant: "MySql",
        default_host: "127.0.0.1",
        default_port: "3306",
        default_charset: "utf8mb4",
    },
    ConnectionSpec {
        name: "mariadb",
        driver_variant: "MySql",
        default_host: "127.0.0.1",
        default_port: "3306",
        default_charset: "utf8mb4",
    },
    ConnectionSpec {
        name: "pgsql",
        driver_variant: "Pgsql",
        default_host: "127.0.0.1",
        default_port: "5432",
        default_charset: "utf8",
    },
    ConnectionSpec {
        name: "sqlsrv",
        driver_variant: "Sqlsrv",
        default_host: "localhost",
        default_port: "1433",
        default_charset: "utf8",
    },
];

/// Renders `config/database.rs`'s full content — Laravel's own
/// `config/database.php` (`default => env('DB_CONNECTION', 'sqlite')`
/// plus named connection blocks), as a real typed `DatabaseConnections`
/// value rather than the `serde_json::Value` every other generated config
/// module returns (see this module's own doc comment for why database
/// config is the one deliberate exception). Every field still resolves
/// through a real `env_or` call — same "a deployment can override any of
/// them without a code change" guarantee [`render_app_config_rs`] gives.
///
/// Unlike [`render_app_config_rs`], this takes no `defaults` — a source
/// Laravel app's own `config/database.php` essentially never customizes
/// these defaults in practice (real per-app values live in `.env`, which
/// `env_or` already reads at runtime regardless of what literal default
/// this function bakes in), so `xr new` and `xr convert` both call this
/// the same way.
pub fn render_database_config_rs() -> String {
    let mut body = String::new();

    body.push_str("    connections.insert(\n");
    body.push_str("        \"sqlite\".to_string(),\n");
    body.push_str("        ConnectionConfig {\n");
    body.push_str("            driver: Driver::Sqlite,\n");
    body.push_str("            host: String::new(),\n");
    body.push_str("            port: 0,\n");
    body.push_str(
        "            database: larust_support::config_env::env_or(\"DB_DATABASE\", \"database/database.sqlite\"),\n",
    );
    body.push_str("            username: String::new(),\n");
    body.push_str("            password: String::new(),\n");
    body.push_str("            charset: String::new(),\n");
    body.push_str("        },\n    );\n\n");

    for spec in CONNECTIONS {
        body.push_str(&format!(
            "    connections.insert(\n        {:?}.to_string(),\n        ConnectionConfig {{\n",
            spec.name
        ));
        body.push_str(&format!(
            "            driver: Driver::{},\n",
            spec.driver_variant
        ));
        body.push_str(&format!(
            "            host: larust_support::config_env::env_or(\"DB_HOST\", {:?}),\n",
            spec.default_host
        ));
        body.push_str(&format!(
            "            port: larust_support::config_env::env_or(\"DB_PORT\", {:?}).parse::<u16>().unwrap_or({}),\n",
            spec.default_port, spec.default_port
        ));
        body.push_str(
            "            database: larust_support::config_env::env_or(\"DB_DATABASE\", \"larust\"),\n",
        );
        body.push_str(
            "            username: larust_support::config_env::env_or(\"DB_USERNAME\", \"root\"),\n",
        );
        body.push_str(
            "            password: larust_support::config_env::env_or(\"DB_PASSWORD\", \"\"),\n",
        );
        body.push_str(&format!(
            "            charset: larust_support::config_env::env_or(\"DB_CHARSET\", {:?}),\n",
            spec.default_charset
        ));
        body.push_str("        },\n    );\n\n");
    }

    format!(
        "use larust_support::orm::{{ConnectionConfig, DatabaseConnections, Driver}};\n\
         use std::collections::HashMap;\n\n\
         pub fn config() -> DatabaseConnections {{\n\
         \x20   let mut connections = HashMap::new();\n\n\
         {body}\
         \x20   DatabaseConnections {{\n\
         \x20       default: larust_support::config_env::env_or(\"DB_CONNECTION\", \"sqlite\"),\n\
         \x20       connections,\n\
         \x20   }}\n\
         }}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_config_declares_every_laravel_named_connection() {
        let code = render_database_config_rs();
        for name in ["sqlite", "mysql", "mariadb", "pgsql", "sqlsrv"] {
            assert!(
                code.contains(&format!("{name:?}.to_string()")),
                "missing connection {name:?} in generated code:\n{code}"
            );
        }
        assert!(code.contains("Driver::Sqlite"));
        assert!(code.contains("Driver::MySql"));
        assert!(code.contains("Driver::Pgsql"));
        assert!(code.contains("Driver::Sqlsrv"));
        assert!(code.contains(
            r#"default: larust_support::config_env::env_or("DB_CONNECTION", "sqlite"),"#
        ));
        assert!(syn::parse_str::<syn::File>(&code).is_ok());
    }

    #[test]
    fn database_config_uses_driver_specific_default_ports() {
        let code = render_database_config_rs();
        assert!(code.contains(r#"env_or("DB_PORT", "3306").parse::<u16>().unwrap_or(3306)"#));
        assert!(code.contains(r#"env_or("DB_PORT", "5432").parse::<u16>().unwrap_or(5432)"#));
        assert!(code.contains(r#"env_or("DB_PORT", "1433").parse::<u16>().unwrap_or(1433)"#));
    }

    #[test]
    fn renders_every_field_with_its_own_env_var_and_default() {
        let mut defaults = HashMap::new();
        defaults.insert("app_name", "\"blog\"".to_string());
        defaults.insert("app_port", "8000".to_string());
        defaults.insert("session_secure_cookie", "true".to_string());
        let code = render_app_config_rs(&defaults, &[]);
        assert!(code.contains(
            r#"config["app_name"] = json!(larust_support::config_env::env_or("APP_NAME", "blog"));"#
        ));
        assert!(code.contains(
            r#"config["app_port"] = json!(larust_support::config_env::env_or("APP_PORT", "8000").parse::<u16>().unwrap_or(8000));"#
        ));
        assert!(code.contains(
            r#"config["session_secure_cookie"] = json!(larust_support::config_env::env_bool("SESSION_SECURE_COOKIE", true));"#
        ));
        assert!(syn::parse_str::<syn::File>(&code).is_ok());
    }

    #[test]
    fn a_field_with_no_supplied_default_falls_back_to_configs_own_generic_default() {
        let code = render_app_config_rs(&HashMap::new(), &[]);
        assert!(code.contains(r#"json!(larust_support::config_env::env_or("APP_NAME", "Larust"))"#));
        assert!(code.contains(r#"json!(larust_support::config_env::env_bool("APP_DEBUG", false))"#));
        assert!(
            code.contains(r#"json!(larust_support::config_env::env_or("MAIL_HOST", "127.0.0.1"))"#)
        );
        assert!(syn::parse_str::<syn::File>(&code).is_ok());
    }

    #[test]
    fn mail_from_name_falls_back_to_app_name_when_unset() {
        let code = render_app_config_rs(&HashMap::new(), &[]);
        assert!(code
            .contains("let mail_from_name = larust_support::config_env::env(\"MAIL_FROM_NAME\");"));
        assert!(code.contains(r#"config["app_name"].as_str().unwrap_or_default().to_string()"#));
    }

    #[test]
    fn extra_lines_are_appended_after_the_fixed_fields() {
        let code = render_app_config_rs(
            &HashMap::new(),
            &[r#"config["apiurl"] = json!("https://example.com");"#.to_string()],
        );
        assert!(code.contains(r#"config["apiurl"] = json!("https://example.com");"#));
        assert!(syn::parse_str::<syn::File>(&code).is_ok());
    }
}
