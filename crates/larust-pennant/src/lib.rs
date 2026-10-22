//! Laravel's `laravel/pennant` - feature flags, narrowed to its core:
//! persisted, scope-aware boolean flags, with no process-wide resolver
//! registry.
//!
//! **No `Feature::define(...)` registry, deliberately.** Real Pennant lets
//! an app register a default-value *closure* once (typically in a service
//! provider), and `Feature::active(...)` calls it lazily on first check per
//! scope, persisting the result. A process-wide `OnceLock<HashMap<String,
//! BoxedResolver>>` could reproduce that here, but this framework has
//! already rejected that exact shape once - `larust_queue::JobRegistry`'s
//! own doc comment is explicit that its handler map is "not a process-wide
//! static... built fresh by whatever calls `work()`", specifically because
//! only one thing in one process ever needs it. A feature flag's default
//! has the same shape: decided by the one call site that cares, not a
//! registry every part of the app must agree to populate before anything
//! can check a flag. [`active`] takes its `default` as a plain argument
//! instead - the caller supplies "what if nothing's been decided yet" right
//! where it's checked, same as any other `Option`-shaped default would be.
//!
//! Boolean-only, not Pennant's richer non-boolean "value" flags
//! (`Feature::value('purchase-button', fn () => ...)` returning a string/
//! array) - deliberately out of scope for this version: the vast majority
//! of real Pennant usage is the boolean on/off case this crate covers, and
//! a typed, non-boolean value store is a genuinely different, larger
//! feature (what type? how is it validated? how does a caller declare the
//! type it expects back?) worth its own design, not bolted on speculatively
//! here.
//!
//! Storage is unconditionally SQL (`feature_flags`, this crate's own
//! table - the same "owns a small table" shape `larust-sanctum`/
//! `larust-permissions`/`larust-notifications`/`larust-cache` each
//! already have), not Pennant's own pluggable `array`/`database` driver
//! split: an in-memory driver would silently lose every flag on restart,
//! which defeats the entire point of a *persisted* rollout decision - not
//! a real choice worth offering.
//!
//! Re-exported through `larust_support::pennant` (see `crates/
//! larust-support/src/lib.rs`) so generated apps depend only on
//! `larust-support`, never on this crate directly - the same wiring every
//! other optional Tier-1 shim crate already uses.
//!
//! ## Deliberately out of scope for this version
//!
//! - **No `@feature`/`@endfeature` Blade directive.** Unlike `@can`/
//!   `@role` (gated by `larust-permissions`), `larust-view`'s own
//!   directive set is a fixed list hardcoded in its parser, not a
//!   pluggable registry an optional crate can extend - adding one is a
//!   real, separate change to a core crate, not something this crate can
//!   do on its own. [`active`] is still callable directly from any
//!   controller/view-context-building code in the meantime, same as
//!   Laravel's own `Feature::active(...)` is usable outside Blade too.
//! - **No `pennant:purge`-equivalent "purge everything" operation.**
//!   [`purge`] removes one named flag across every scope (the safe,
//!   common "retire this flag" case); clearing the entire table in one
//!   call is a sharper footgun with no real use case this crate has seen
//!   requested yet.
//! - **No rich/non-boolean values** - see this doc comment's own opening
//!   for why.

use larust_core::AppError;
use larust_orm::Backend;
use sqlx::AnyPool;

/// `None` (an unscoped, "global" flag) is stored as an empty string, not
/// SQL `NULL` - `(name, scope)` is this table's composite primary key, and
/// standard SQL `NULL` never equals itself for uniqueness purposes (two
/// `NULL` scopes for the same `name` wouldn't collide), which would let
/// `activate("x", None)` silently insert a second, un-deduplicated row
/// every time instead of updating the one already there. An empty string
/// is a value no real scope (a user id, an email, a tenant slug) will ever
/// collide with, and keeps the primary key simple and `NOT NULL` like
/// every other string-keyed table in this codebase already is.
fn normalize_scope(scope: Option<&str>) -> &str {
    scope.unwrap_or("")
}

/// Same lazy self-bootstrap idiom `larust-sanctum`/`larust-notifications`'s
/// own `ensure_table` establishes - plain `CREATE TABLE IF NOT EXISTS`, no
/// migration file and no explicit startup call needed anywhere.
/// Deliberately **not** memoized behind a `OnceCell`: `larust_testing::
/// test_transaction`/a fresh per-test database means a process-wide
/// completion flag could point at a since-discarded database - the exact
/// regression `larust-notifications`'s own doc comment documents hitting
/// once already. `IF NOT EXISTS` makes re-running this on every call cheap
/// enough that giving up the memoization is the better trade.
async fn ensure_table(pool: &AnyPool) -> Result<(), AppError> {
    let create_table = match larust_orm::backend() {
        Backend::Sqlite => {
            "CREATE TABLE IF NOT EXISTS feature_flags (\
                name TEXT NOT NULL, \
                scope TEXT NOT NULL, \
                active INTEGER NOT NULL, \
                PRIMARY KEY (name, scope)\
             )"
        }
        // `VARCHAR`, not MySQL's own `TEXT` - a `TEXT`/`BLOB` column used
        // in a key specification (this table's composite primary key)
        // needs an explicit length ("BLOB/TEXT column used in key
        // specification without a key length"), the same lesson
        // `larust-cache`'s own `cache_items` table and `larust-sanctum`'s
        // own `token_hash` column already document. `VARCHAR(255)` is a
        // reasonable cap for a flag name or a scope string (a user id, an
        // email, a short slug).
        Backend::MySql => {
            "CREATE TABLE IF NOT EXISTS feature_flags (\
                name VARCHAR(255) NOT NULL, \
                scope VARCHAR(255) NOT NULL, \
                active INTEGER NOT NULL, \
                PRIMARY KEY (name, scope)\
             )"
        }
        // Postgres has native, unbounded `TEXT` and no MySQL-style
        // key-length requirement - same shape as SQLite's own arm.
        Backend::Postgres => {
            "CREATE TABLE IF NOT EXISTS feature_flags (\
                name TEXT NOT NULL, \
                scope TEXT NOT NULL, \
                active INTEGER NOT NULL, \
                PRIMARY KEY (name, scope)\
             )"
        }
    };
    sqlx::query(create_table)
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    Ok(())
}

/// Whether `name` is active for `scope` (`None` for the unscoped, global
/// case - see [`normalize_scope`]'s own doc comment). Returns `default`
/// when nothing has been decided yet for this exact `(name, scope)` pair -
/// the caller's own fallback, not a registered resolver (see this crate's
/// own doc comment for why). Laravel's `Feature::for($user)->active('x')`/
/// plain `Feature::active('x')`.
pub async fn active(name: &str, scope: Option<&str>, default: bool) -> Result<bool, AppError> {
    let pool = larust_orm::pool()?;
    ensure_table(pool).await?;

    let backend = larust_orm::backend();
    let select_sql = format!(
        "SELECT active FROM feature_flags WHERE name = {} AND scope = {}",
        larust_orm::placeholder(backend, 1),
        larust_orm::placeholder(backend, 2),
    );
    let row: Option<(i64,)> = sqlx::query_as(&select_sql)
        .bind(name)
        .bind(normalize_scope(scope))
        .fetch_optional(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    Ok(row.map(|(active,)| active != 0).unwrap_or(default))
}

/// Shared upsert behind [`activate`]/[`deactivate`] - same "one named row
/// per `(name, scope)`, overwrite on conflict" shape `larust-cache`'s own
/// `put` already establishes for `cache_items`, just keyed by a composite
/// `(name, scope)` pair instead of a single `key` column.
async fn set_active(name: &str, scope: Option<&str>, value: bool) -> Result<(), AppError> {
    let pool = larust_orm::pool()?;
    ensure_table(pool).await?;

    let upsert_sql = match larust_orm::backend() {
        Backend::Sqlite => {
            "INSERT INTO feature_flags (name, scope, active) VALUES (?, ?, ?) \
             ON CONFLICT(name, scope) DO UPDATE SET active = excluded.active"
        }
        Backend::MySql => {
            "INSERT INTO feature_flags (name, scope, active) VALUES (?, ?, ?) \
             ON DUPLICATE KEY UPDATE active = VALUES(active)"
        }
        Backend::Postgres => {
            "INSERT INTO feature_flags (name, scope, active) VALUES ($1, $2, $3) \
             ON CONFLICT(name, scope) DO UPDATE SET active = excluded.active"
        }
    };
    sqlx::query(upsert_sql)
        .bind(name)
        .bind(normalize_scope(scope))
        .bind(i64::from(value))
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    Ok(())
}

/// Persists `name` as active for `scope` - every later [`active`] call for
/// this exact pair returns `true` until [`deactivate`]/[`forget`].
/// Laravel's `Feature::activate('x')`.
pub async fn activate(name: &str, scope: Option<&str>) -> Result<(), AppError> {
    set_active(name, scope, true).await
}

/// Persists `name` as inactive for `scope` - every later [`active`] call
/// for this exact pair returns `false` until [`activate`]/[`forget`].
/// Laravel's `Feature::deactivate('x')`.
pub async fn deactivate(name: &str, scope: Option<&str>) -> Result<(), AppError> {
    set_active(name, scope, false).await
}

/// Removes any stored decision for `(name, scope)` - a later [`active`]
/// call for this exact pair falls back to its own `default` argument
/// again, as if nothing had ever been decided. Laravel's
/// `Feature::forget('x')`.
pub async fn forget(name: &str, scope: Option<&str>) -> Result<(), AppError> {
    let pool = larust_orm::pool()?;
    ensure_table(pool).await?;

    let backend = larust_orm::backend();
    let sql = format!(
        "DELETE FROM feature_flags WHERE name = {} AND scope = {}",
        larust_orm::placeholder(backend, 1),
        larust_orm::placeholder(backend, 2),
    );
    sqlx::query(&sql)
        .bind(name)
        .bind(normalize_scope(scope))
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    Ok(())
}

/// Removes every stored decision for `name`, across every scope - for
/// retiring a flag entirely once a rollout is finished, rather than
/// forgetting one scope at a time. Laravel's `Feature::purge('x')` (this
/// crate has no equivalent of purging *every* flag in one call - see this
/// module's own doc comment for why).
pub async fn purge(name: &str) -> Result<(), AppError> {
    let pool = larust_orm::pool()?;
    ensure_table(pool).await?;

    let backend = larust_orm::backend();
    let sql = format!(
        "DELETE FROM feature_flags WHERE name = {}",
        larust_orm::placeholder(backend, 1)
    );
    sqlx::query(&sql)
        .bind(name)
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn connect_test_db() {
        let dir = tempfile::tempdir().unwrap().keep();
        let database_url = format!("sqlite://{}/test.sqlite", dir.display());
        larust_orm::connect(&database_url).await.unwrap();
    }

    /// All scenarios share one test function, not several - `larust_orm::
    /// connect()` sets a process-wide pool exactly once, the same
    /// constraint `larust-sanctum`/`larust-permissions`/
    /// `larust-notifications`'s own test suites document and work around.
    #[tokio::test]
    async fn pennant_crate_behaves_correctly_across_every_scenario() {
        connect_test_db().await;

        // A flag nobody has ever decided on falls back to the caller's
        // own `default`, whichever way it's asked.
        assert!(active("new-api", None, true).await.unwrap());
        assert!(!active("new-api", None, false).await.unwrap());
        assert!(!active("new-api", Some("user:1"), false).await.unwrap());

        // Activating it for one scope persists `true` there, regardless
        // of what `default` the caller supplies on the next check - and
        // leaves every other scope (including the unscoped/global one)
        // completely untouched.
        activate("new-api", Some("user:1")).await.unwrap();
        assert!(active("new-api", Some("user:1"), false).await.unwrap());
        assert!(!active("new-api", None, false).await.unwrap());
        assert!(!active("new-api", Some("user:2"), false).await.unwrap());

        // Deactivating persists `false` the same way - not merely "forget
        // the previous value", a real, explicit decision of its own.
        deactivate("new-api", Some("user:1")).await.unwrap();
        assert!(!active("new-api", Some("user:1"), true).await.unwrap());

        // Re-activating the same `(name, scope)` pair overwrites in
        // place - no duplicate-row/uniqueness error on the second write.
        activate("new-api", Some("user:1")).await.unwrap();
        assert!(active("new-api", Some("user:1"), false).await.unwrap());

        // The unscoped/global flag is entirely independent of any scoped
        // decision under the same name.
        activate("new-api", None).await.unwrap();
        assert!(active("new-api", None, false).await.unwrap());
        assert!(active("new-api", Some("user:1"), false).await.unwrap());
        deactivate("new-api", None).await.unwrap();
        assert!(!active("new-api", None, true).await.unwrap());
        assert!(active("new-api", Some("user:1"), false).await.unwrap());

        // Forgetting one scope reverts only that scope to the caller's
        // own default, leaving every other scope's own stored decision
        // alone.
        forget("new-api", Some("user:1")).await.unwrap();
        assert!(active("new-api", Some("user:1"), true).await.unwrap());
        assert!(!active("new-api", Some("user:1"), false).await.unwrap());

        // Purging a flag clears every scope it was ever set under -
        // including ones `forget` above never touched - but leaves a
        // different, unrelated flag name completely alone.
        activate("new-api", Some("user:2")).await.unwrap();
        activate("other-flag", Some("user:2")).await.unwrap();
        purge("new-api").await.unwrap();
        assert!(!active("new-api", Some("user:2"), false).await.unwrap());
        assert!(!active("new-api", None, false).await.unwrap());
        assert!(active("other-flag", Some("user:2"), false).await.unwrap());
    }
}
