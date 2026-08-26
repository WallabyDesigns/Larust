use larust_core::AppError;
use sqlx::any::{AnyConnectOptions, AnyPoolOptions};
use sqlx::AnyPool;
use std::future::Future;
use std::str::FromStr;
use std::sync::OnceLock;

static POOL: OnceLock<AnyPool> = OnceLock::new();
static BACKEND: OnceLock<Backend> = OnceLock::new();

/// Which real database engine `DATABASE_URL` selected — set once, inside
/// [`connect`], by reading the URL's own scheme (`sqlite://` vs
/// `mysql://`). `larust-orm` deliberately doesn't ask `sqlx` this
/// (`sqlx::any::AnyKind` is `#[deprecated = "not used or returned by any
/// API"]` in 0.8) — it already knows the answer the moment it parses the
/// URL to connect, so it just remembers it.
///
/// Every other framework crate that needs to emit backend-specific SQL
/// (an `AUTOINCREMENT` vs `AUTO_INCREMENT` `CREATE TABLE`, `INSERT OR
/// IGNORE` vs `INSERT IGNORE`, ...) branches on [`backend`] rather than
/// each reimplementing its own detection — see
/// `larust-permissions`/`larust-sanctum`/`larust-notifications`/
/// `larust-queue`/`larust-cache` for the pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Sqlite,
    MySql,
}

tokio::task_local! {
    /// Set only by `larust_testing::test_transaction` (via
    /// `with_pool_override`) — everywhere else this is simply never set,
    /// and `pool()` falls through to the process-wide `POOL` exactly as
    /// before. Task-local, not process-wide: unlike `POOL` itself, this
    /// doesn't need "first writer wins" semantics, since each test that
    /// uses it sets its own value in its own task, isolated from every
    /// other task doing the same thing concurrently.
    static POOL_OVERRIDE: &'static AnyPool;
}

/// Connects to the database and stores the pool process-wide (same
/// `OnceLock` pattern as `larust-http`'s route-name registry). Call once
/// at startup, after config/`.env` has been loaded.
///
/// Builds an `sqlx::any::AnyPool` (runtime-generic — dispatches to
/// whichever real driver `database_url`'s scheme selects) rather than a
/// concretely-typed `SqlitePool`/`MySqlPool`, so every other framework
/// crate and every `#[derive(Model)]` struct works unchanged regardless
/// of which backend a given app actually runs against — see
/// `docs/ARCHITECTURE.md`'s database-backends section.
///
/// `AnyConnectOptions` is deliberately opaque (just a parsed `Url` —
/// confirmed by reading `sqlx-core`'s own source, it exposes no builder
/// methods for engine-specific settings like SQLite's WAL mode), so the
/// SQLite-only tuning this crate has always applied (WAL journal mode,
/// a 5s busy timeout so brief lock contention waits instead of failing
/// immediately, foreign keys on) can't be set through it directly.
/// `PoolOptions::after_connect` — generic over any backend, called once
/// per new physical connection — is the portable way to apply it: a
/// plain `PRAGMA` statement, only ever run when [`backend`] is actually
/// `Sqlite` (running a SQLite `PRAGMA` against a MySQL connection would
/// error). `create_if_missing(true)`'s equivalent is SQLite's own
/// `?mode=rwc` URL query parameter (confirmed against
/// `sqlx-sqlite`'s own URL parser) — appended automatically here so an
/// app's `.env` doesn't need to change to keep today's "just works on
/// first run" behavior.
pub async fn connect(database_url: &str) -> Result<(), AppError> {
    sqlx::any::install_default_drivers();

    let backend = ensure_backend(database_url)?;

    // `sqlx::any::AnyConnectOptions::from_str` parses `database_url` with
    // the `url` crate's strict RFC 3986 parser (confirmed by reading
    // `sqlx-core`'s source) — unlike the old, SQLite-specific
    // `SqliteConnectOptions::from_str` this crate used before adding
    // MySQL support, which treated everything after `sqlite://` as a raw
    // filesystem path via plain string splitting and never went through a
    // URL parser at all. A Windows *absolute* path handed to `.display()`
    // (backslash separators, a `C:` drive letter right after `sqlite://`)
    // parses under RFC 3986 without erroring, but wrongly — `//` starts an
    // "authority" component, so `C:` gets read as `host:port`-shaped
    // (confirmed empirically: without this fix, the resulting connection
    // fails with SQLite's own "unable to open database file", meaning the
    // driver received a mangled path). Rewriting to `sqlite:///C:/...`
    // (three slashes: an explicitly *empty* authority followed by an
    // absolute path starting with `/`) sidesteps the ambiguity entirely —
    // the standard way any URL scheme represents "no host, absolute path"
    // (`file:///...` follows the identical convention). A *relative*
    // SQLite path (the common case — `sqlite://database/database.sqlite`,
    // this codebase's own scaffold default) is left alone beyond backslash
    // normalization: forcing a leading `/` onto it would silently turn it
    // into an absolute path rooted at the filesystem root, a real
    // behavioral change, not a parsing fix. This regression would
    // otherwise hit every framework crate's own `sqlite://{tempdir}/
    // test.sqlite`-shaped test helper (`larust-testing`, `larust-queue`,
    // `larust-permissions`, ...) on every Windows dev machine, since
    // `tempfile::tempdir()` always returns this exact absolute-path shape.
    let normalized;
    let database_url = match backend {
        Backend::Sqlite => {
            normalized = normalize_sqlite_url(database_url);
            normalized.as_str()
        }
        Backend::MySql => database_url,
    };

    let connect_url = match backend {
        Backend::Sqlite if !database_url.contains('?') => {
            format!("{database_url}?mode=rwc")
        }
        Backend::Sqlite => format!("{database_url}&mode=rwc"),
        Backend::MySql => database_url.to_string(),
    };
    let options =
        AnyConnectOptions::from_str(&connect_url).map_err(|e| AppError::Config(Box::new(e)))?;

    let pool = AnyPoolOptions::new()
        .min_connections(1)
        .max_connections(10)
        .after_connect(move |conn, _meta| {
            Box::pin(async move {
                // `conn.execute(&str)` (the `Executor` trait method,
                // called directly on the connection — matching the exact
                // pattern sqlx's own `tests/any/pool.rs` uses in an
                // `after_connect` closure) accepts multiple `;`-separated
                // statements in one call, unlike `sqlx::query(...)`, which
                // only ever prepares one.
                use sqlx::Executor;
                match backend {
                    Backend::Sqlite => {
                        conn.execute(
                            "PRAGMA journal_mode = WAL; \
                             PRAGMA busy_timeout = 5000; \
                             PRAGMA foreign_keys = ON;",
                        )
                        .await?;
                    }
                    Backend::MySql => {
                        // Every identifier throughout this codebase — the
                        // query builder, `#[derive(Model)]`'s generated
                        // SQL, every ecosystem crate's own queries — is
                        // double-quoted (`"table"`, `"column"`), the
                        // ANSI-SQL/SQLite convention. MySQL's *default*
                        // `sql_mode` instead treats a double-quoted token
                        // as a string literal, not an identifier — so
                        // without this, every one of those queries would
                        // fail outright against a stock MySQL server.
                        // `ANSI_QUOTES` makes MySQL accept the same
                        // double-quote convention SQLite already uses,
                        // avoiding a rewrite of every quoted identifier in
                        // the framework. Appended via `CONCAT` (not a bare
                        // `SET sql_mode = 'ANSI_QUOTES'`) so it adds to
                        // the session's existing modes rather than
                        // silently dropping MySQL's own sane defaults
                        // (`STRICT_TRANS_TABLES`, etc.).
                        conn.execute("SET SESSION sql_mode = CONCAT(@@sql_mode, ',ANSI_QUOTES')")
                            .await?;
                    }
                }
                Ok(())
            })
        })
        .connect_with(options)
        .await
        .map_err(|e| AppError::Internal(Box::new(e)))?;

    POOL.set(pool).map_err(|_| {
        AppError::Internal(Box::new(std::io::Error::other(
            "connect() called more than once",
        )))
    })?;

    Ok(())
}

/// This app's database backend — see [`Backend`]. Panics if called
/// before [`connect`], matching `larust_core::config()`'s own "real
/// caller-contract violation" reasoning (nothing before `connect()`
/// could plausibly need to know the backend either).
pub fn backend() -> Backend {
    *BACKEND
        .get()
        .expect("larust_orm::backend() called before connect()")
}

/// Sets [`BACKEND`] from `database_url`'s scheme, without touching the
/// process-wide [`POOL`] — unlike [`connect`], safe to call more than
/// once per process (every call after the first is a harmless no-op, the
/// same `.ok()`-swallowed idempotency [`connect`] itself already gives
/// `BACKEND`). For `larust_testing::test_transaction`'s own
/// `connect_isolated`, which deliberately builds its own dedicated pool
/// *outside* the process-wide singleton (see that function's own doc
/// comment for why) but still needs `backend()` to resolve correctly for
/// whatever it runs — every framework crate's own bootstrap SQL branches
/// on it.
pub fn ensure_backend(database_url: &str) -> Result<Backend, AppError> {
    let backend = parse_backend(database_url)?;
    BACKEND.set(backend).ok();
    Ok(backend)
}

/// Rewrites a `sqlite:` URL so `sqlx::any::AnyConnectOptions::from_str`'s
/// strict RFC 3986 parsing (via the `url` crate) resolves it the same way
/// the old, lenient, SQLite-specific `SqliteConnectOptions::from_str` this
/// crate used before adding MySQL support always did — see [`connect`]'s
/// own doc comment for the full explanation of *why* this is needed at
/// all (in short: a Windows absolute path's `C:` drive letter, sitting
/// right after `sqlite://`, gets misread as a `host:port`-shaped URL
/// authority component otherwise).
///
/// Two independent fixes, both SQLite-only:
/// - Backslash path separators become forward slashes (accepted by every
///   SQLite/Windows filesystem API too, and never ambiguous with anything
///   URL syntax uses).
/// - A path that looks like a Windows drive-absolute path (`C:/...`) gets
///   an explicit empty authority inserted (`sqlite:///C:/...`, three
///   slashes) — the standard "no host, absolute path" URL convention
///   (`file:///...` follows the identical shape). A *relative* path
///   (`sqlite://database/database.sqlite`, this codebase's own scaffold
///   default) is deliberately left as a two-slash URL: forcing a leading
///   `/` onto it would silently turn it into a path rooted at the
///   filesystem root, a real behavior change, not a parsing fix.
pub fn normalize_sqlite_url(database_url: &str) -> String {
    let forward_slashes = database_url.replace('\\', "/");
    let path = forward_slashes
        .strip_prefix("sqlite://")
        .unwrap_or(&forward_slashes);
    let is_windows_drive_absolute = path.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && path.as_bytes().get(1) == Some(&b':');
    if is_windows_drive_absolute {
        format!("sqlite:///{path}")
    } else {
        forward_slashes
    }
}

fn parse_backend(database_url: &str) -> Result<Backend, AppError> {
    if database_url.starts_with("sqlite:") {
        Ok(Backend::Sqlite)
    } else if database_url.starts_with("mysql:") {
        Ok(Backend::MySql)
    } else {
        Err(AppError::Config(Box::new(std::io::Error::other(format!(
            "unsupported DATABASE_URL scheme in {database_url:?} — expected \
             \"sqlite:\" or \"mysql:\""
        )))))
    }
}

/// Returns the pool every `#[derive(Model)]` method and `QueryBuilder`
/// call resolves its connection through — this single resolution point
/// (not a parameter threaded through every generated method) is what
/// makes `with_pool_override` below work at all. Checks the task-local
/// override first (set only inside `larust_testing::test_transaction`),
/// then falls back to the process-wide pool. Errors (rather than panics)
/// if neither is set — a misconfigured startup order is a real
/// possibility (e.g. a route handler running before `main` finishes
/// wiring up the database), not a truly unreachable state.
pub fn pool() -> Result<&'static AnyPool, AppError> {
    if let Ok(overridden) = POOL_OVERRIDE.try_with(|pool| *pool) {
        return Ok(overridden);
    }

    POOL.get().ok_or_else(|| {
        AppError::Internal(Box::new(std::io::Error::other(
            "database not connected; call larust_orm::connect() \
             (via larust_support::orm::connect) at startup before serving requests",
        )))
    })
}

/// Runs `fut` with `pool` resolved by every `pool()` call made from
/// within it — and from anything it directly `.await`s, since a
/// `tokio::task_local!` is visible throughout one task's execution. A
/// future `fut` hands off to `tokio::spawn` as a *separate* detached
/// task would **not** see this override (spawned tasks don't inherit
/// their parent's task-locals) — confirmed nothing in `larust-orm`'s or
/// `larust-macros`' generated code does that (`grep` for `tokio::spawn`/
/// `join_all` in both turns up nothing), so this is safe for every
/// existing `#[derive(Model)]`/`QueryBuilder` call path today.
///
/// Used by `larust_testing::test_transaction`; not meant for application
/// code — there is deliberately no equivalent re-exported through
/// `larust_support::orm`.
pub async fn with_pool_override<F: Future>(pool: &'static AnyPool, fut: F) -> F::Output {
    POOL_OVERRIDE.scope(pool, fut).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_backend_recognizes_sqlite() {
        assert_eq!(
            parse_backend("sqlite://database/database.sqlite").unwrap(),
            Backend::Sqlite
        );
    }

    #[test]
    fn parse_backend_recognizes_mysql() {
        assert_eq!(
            parse_backend("mysql://root@127.0.0.1/app").unwrap(),
            Backend::MySql
        );
    }

    #[test]
    fn normalize_sqlite_url_leaves_a_relative_path_alone() {
        assert_eq!(
            normalize_sqlite_url("sqlite://database/database.sqlite"),
            "sqlite://database/database.sqlite"
        );
    }

    #[test]
    fn normalize_sqlite_url_converts_backslashes_in_a_windows_absolute_path() {
        assert_eq!(
            normalize_sqlite_url(r"sqlite://C:\Users\me\AppData\Local\Temp\abc\test.sqlite"),
            "sqlite:///C:/Users/me/AppData/Local/Temp/abc/test.sqlite"
        );
    }

    #[test]
    fn normalize_sqlite_url_adds_the_third_slash_for_a_drive_absolute_path_already_using_forward_slashes(
    ) {
        assert_eq!(
            normalize_sqlite_url("sqlite://C:/Users/me/test.sqlite"),
            "sqlite:///C:/Users/me/test.sqlite"
        );
    }

    #[test]
    fn normalize_sqlite_url_leaves_a_unix_style_absolute_path_alone() {
        assert_eq!(
            normalize_sqlite_url("sqlite:///var/lib/app/database.sqlite"),
            "sqlite:///var/lib/app/database.sqlite"
        );
    }

    #[test]
    fn parse_backend_rejects_an_unsupported_scheme() {
        assert!(parse_backend("postgres://localhost/app").is_err());
    }
}
