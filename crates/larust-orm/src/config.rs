//! A typed, Laravel-`config/database.php`-shaped connection config —
//! `default => env('DB_CONNECTION', 'sqlite')` plus named connection
//! blocks — as a real value fed straight into [`crate::connect`], not a
//! `serde_json::Value` an app reads back with a dotted-key lookup the way
//! every other generated `config/*.rs` module in this codebase does (see
//! `larust_convert::config`'s own doc comment for that pattern). A
//! database connection needs to become a real `Backend` + connection
//! string at startup, not stay a loose bag of JSON — that's the whole
//! reason this lives here as its own module instead of folding into
//! either `larust_core::Config` (deliberately small/fixed, zero `sqlx`
//! knowledge, no host/port-shaped fields at all) or the generic
//! `Value`-map generated-config convention.
//!
//! Larust only ever connects to *one* database at a time (a process-wide
//! `AnyPool` singleton, see [`crate::pool()`]) — true Laravel-style
//! simultaneous multi-connection access (`DB::connection('name')`) isn't
//! how this ORM works and isn't attempted here. [`DatabaseConnections`]
//! still models every *named* connection block the way Laravel's config
//! file does (so `DB_CONNECTION` can switch which one is active without
//! a code change), but only the one named by
//! [`DatabaseConnections::default`] is ever actually resolved to a URL.

use larust_core::AppError;
use std::collections::HashMap;

/// Which wire protocol a named connection speaks — distinct from
/// [`crate::Backend`] on purpose: `Backend` is "what `sqlx::Any` is
/// actually talking to," resolved from a URL scheme once `connect()`
/// runs, and every exhaustive `match` on it across this codebase assumes
/// the connection is reachable through `AnyPool`. `Driver::Sqlsrv` names
/// a connection SQL Server can never reach that way at all (no `sqlx`
/// driver exists for it) — collapsing the two enums would force `Backend`
/// to grow a variant nothing durable can do anything with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Driver {
    Sqlite,
    /// Also selected by the `"mariadb"` connection name — MariaDB is
    /// wire-protocol-compatible with MySQL (the same `mysql://` scheme,
    /// the same `sqlx`/`Any` driver), so it's a pure alias here, not a
    /// separate variant needing its own `Backend`/SQL-branching story.
    MySql,
    Pgsql,
    /// Named so a config file can select it, but never connectable via
    /// [`DatabaseConnections::default_connection_url`] — see that
    /// method's own doc comment, and `larust-mssql` for how a SQL-Server-
    /// backed connection actually gets used.
    Sqlsrv,
}

/// One named connection block — Laravel's `config/database.php`
/// `'mysql' => [...]`, narrowed to the settings this framework actually
/// needs (no `unix_socket`/`prefix`/`strict`/`engine` — nothing here
/// consumes them today; add them if a real caller ever needs to).
#[derive(Debug, Clone)]
pub struct ConnectionConfig {
    pub driver: Driver,
    /// Ignored for `Driver::Sqlite` — see [`database`](Self::database)'s
    /// own doc comment.
    pub host: String,
    pub port: u16,
    /// For every driver except `Driver::Sqlite`, a schema/database name.
    /// For `Driver::Sqlite`, a file path (Laravel's own `database.sqlite`
    /// convention) — `host`/`port`/`username`/`password`/`charset` are
    /// all meaningless for a local file and are ignored.
    pub database: String,
    pub username: String,
    pub password: String,
    pub charset: String,
}

impl ConnectionConfig {
    /// Assembles the connection URL [`crate::connect`] already knows how
    /// to consume — the one place in this codebase that turns
    /// `(driver, host, port, database, username, password)` into a
    /// `scheme://user:pass@host:port/db` string; nothing else does this
    /// today (a pre-assembled URL was previously the only input format
    /// `connect()` ever saw). Username/password are percent-encoded since
    /// a real password containing `@`, `:`, `/`, or `%` would otherwise
    /// corrupt `AnyConnectOptions::from_str`'s strict RFC 3986 parsing
    /// (see [`crate::connect`]'s own doc comment on why that parser is
    /// strict, not lenient).
    pub fn to_url(&self) -> Result<String, AppError> {
        match self.driver {
            Driver::Sqlite => Ok(format!("sqlite://{}", self.database)),
            Driver::MySql => Ok(format!(
                "mysql://{}:{}@{}:{}/{}",
                percent_encode_userinfo(&self.username),
                percent_encode_userinfo(&self.password),
                self.host,
                self.port,
                self.database,
            )),
            Driver::Pgsql => Ok(format!(
                "postgres://{}:{}@{}:{}/{}",
                percent_encode_userinfo(&self.username),
                percent_encode_userinfo(&self.password),
                self.host,
                self.port,
                self.database,
            )),
            Driver::Sqlsrv => Err(AppError::Config(Box::new(std::io::Error::other(
                "the \"sqlsrv\" driver isn't connectable via larust_orm::connect() — \
                 sqlx has no SQL Server driver at all; see the larust-mssql crate, \
                 which connects to it separately via larust_repository::Repository",
            )))),
        }
    }
}

/// Every named connection block plus which one is active — Laravel's
/// `config/database.php` in full: `'default' => env('DB_CONNECTION',
/// 'sqlite')` and `'connections' => [...]`.
#[derive(Debug, Clone)]
pub struct DatabaseConnections {
    pub default: String,
    pub connections: HashMap<String, ConnectionConfig>,
}

impl DatabaseConnections {
    /// Resolves [`default`](Self::default) against
    /// [`connections`](Self::connections) and assembles its URL — the
    /// single call a generated `main.rs` makes to get whatever
    /// [`crate::connect`] needs, regardless of which driver `DB_CONNECTION`
    /// actually named.
    pub fn default_connection_url(&self) -> Result<String, AppError> {
        let connection = self.connections.get(&self.default).ok_or_else(|| {
            AppError::Config(Box::new(std::io::Error::other(format!(
                "DB_CONNECTION={:?} does not match any configured connection \
                 (expected one of: sqlite, mysql, mariadb, pgsql, sqlsrv)",
                self.default
            ))))
        })?;
        connection.to_url()
    }
}

/// Percent-encodes `value` for safe use in a URL's userinfo component
/// (`scheme://user:pass@host/...`). Small and hand-rolled rather than a
/// new dependency — the same "just write the few lines this needs"
/// precedent `larust-sanctum`'s own hex encoding already sets in this
/// codebase.
fn percent_encode_userinfo(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection(driver: Driver) -> ConnectionConfig {
        ConnectionConfig {
            driver,
            host: "127.0.0.1".to_string(),
            port: 5432,
            database: "app".to_string(),
            username: "root".to_string(),
            password: "secret".to_string(),
            charset: "utf8".to_string(),
        }
    }

    #[test]
    fn sqlite_url_uses_the_database_field_as_a_bare_path() {
        let config = ConnectionConfig {
            driver: Driver::Sqlite,
            database: "database/database.sqlite".to_string(),
            ..connection(Driver::Sqlite)
        };
        assert_eq!(
            config.to_url().unwrap(),
            "sqlite://database/database.sqlite"
        );
    }

    #[test]
    fn mysql_url_assembles_every_part() {
        let config = connection(Driver::MySql);
        assert_eq!(
            config.to_url().unwrap(),
            "mysql://root:secret@127.0.0.1:5432/app"
        );
    }

    #[test]
    fn pgsql_url_assembles_every_part() {
        let config = connection(Driver::Pgsql);
        assert_eq!(
            config.to_url().unwrap(),
            "postgres://root:secret@127.0.0.1:5432/app"
        );
    }

    #[test]
    fn sqlsrv_is_named_but_not_connectable_this_way() {
        assert!(connection(Driver::Sqlsrv).to_url().is_err());
    }

    #[test]
    fn a_password_with_url_unsafe_characters_is_percent_encoded() {
        let config = ConnectionConfig {
            password: "p@ss:w/rd%".to_string(),
            ..connection(Driver::Pgsql)
        };
        assert_eq!(
            config.to_url().unwrap(),
            "postgres://root:p%40ss%3Aw%2Frd%25@127.0.0.1:5432/app"
        );
    }

    #[test]
    fn default_connection_url_resolves_the_named_default() {
        let mut connections = HashMap::new();
        connections.insert("pgsql".to_string(), connection(Driver::Pgsql));
        connections.insert("sqlite".to_string(), connection(Driver::Sqlite));
        let db = DatabaseConnections {
            default: "pgsql".to_string(),
            connections,
        };
        assert_eq!(
            db.default_connection_url().unwrap(),
            "postgres://root:secret@127.0.0.1:5432/app"
        );
    }

    #[test]
    fn default_connection_url_errors_when_default_names_nothing_configured() {
        let db = DatabaseConnections {
            default: "oracle".to_string(),
            connections: HashMap::new(),
        };
        assert!(db.default_connection_url().is_err());
    }
}
