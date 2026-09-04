use larust_support::orm::{ConnectionConfig, DatabaseConnections, Driver};
use std::collections::HashMap;

/// This example blog's own database connections - the hand-written counterpart of what
/// `xr new`/`xr convert` generate automatically (see
/// `larust_cli::config_template::render_database_config_rs`, the shared
/// template this mirrors). Laravel's own `config/database.php` shape:
/// `default => env('DB_CONNECTION', 'sqlite')` plus one named block per
/// driver, each resolving through a real `env_or` call so `.env` can
/// switch backends without a code change.
pub fn config() -> DatabaseConnections {
    let mut connections = HashMap::new();

    connections.insert(
        "sqlite".to_string(),
        ConnectionConfig {
            driver: Driver::Sqlite,
            host: String::new(),
            port: 0,
            database: larust_support::config_env::env_or("DB_DATABASE", "database/database.sqlite"),
            username: String::new(),
            password: String::new(),
            charset: String::new(),
        },
    );

    connections.insert(
        "mysql".to_string(),
        ConnectionConfig {
            driver: Driver::MySql,
            host: larust_support::config_env::env_or("DB_HOST", "127.0.0.1"),
            port: larust_support::config_env::env_or("DB_PORT", "3306")
                .parse::<u16>()
                .unwrap_or(3306),
            database: larust_support::config_env::env_or("DB_DATABASE", "larust"),
            username: larust_support::config_env::env_or("DB_USERNAME", "root"),
            password: larust_support::config_env::env_or("DB_PASSWORD", ""),
            charset: larust_support::config_env::env_or("DB_CHARSET", "utf8mb4"),
        },
    );

    connections.insert(
        "mariadb".to_string(),
        ConnectionConfig {
            driver: Driver::MySql,
            host: larust_support::config_env::env_or("DB_HOST", "127.0.0.1"),
            port: larust_support::config_env::env_or("DB_PORT", "3306")
                .parse::<u16>()
                .unwrap_or(3306),
            database: larust_support::config_env::env_or("DB_DATABASE", "larust"),
            username: larust_support::config_env::env_or("DB_USERNAME", "root"),
            password: larust_support::config_env::env_or("DB_PASSWORD", ""),
            charset: larust_support::config_env::env_or("DB_CHARSET", "utf8mb4"),
        },
    );

    connections.insert(
        "pgsql".to_string(),
        ConnectionConfig {
            driver: Driver::Pgsql,
            host: larust_support::config_env::env_or("DB_HOST", "127.0.0.1"),
            port: larust_support::config_env::env_or("DB_PORT", "5432")
                .parse::<u16>()
                .unwrap_or(5432),
            database: larust_support::config_env::env_or("DB_DATABASE", "larust"),
            username: larust_support::config_env::env_or("DB_USERNAME", "root"),
            password: larust_support::config_env::env_or("DB_PASSWORD", ""),
            charset: larust_support::config_env::env_or("DB_CHARSET", "utf8"),
        },
    );

    connections.insert(
        "sqlsrv".to_string(),
        ConnectionConfig {
            driver: Driver::Sqlsrv,
            host: larust_support::config_env::env_or("DB_HOST", "localhost"),
            port: larust_support::config_env::env_or("DB_PORT", "1433")
                .parse::<u16>()
                .unwrap_or(1433),
            database: larust_support::config_env::env_or("DB_DATABASE", "larust"),
            username: larust_support::config_env::env_or("DB_USERNAME", "root"),
            password: larust_support::config_env::env_or("DB_PASSWORD", ""),
            charset: larust_support::config_env::env_or("DB_CHARSET", "utf8"),
        },
    );

    DatabaseConnections {
        default: larust_support::config_env::env_or("DB_CONNECTION", "sqlite"),
        connections,
    }
}
