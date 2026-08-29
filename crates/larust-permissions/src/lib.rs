//! Roles and permissions — Laravel's `spatie/laravel-permission`, the
//! single most commonly-installed third-party Laravel package (see
//! `crates/larust-convert/src/composer.rs`'s `TIER_1` table, which points
//! here for it), narrowed to its core role/permission-assignment shape.
//!
//! **A hybrid design, not a straight port** — worth explaining up front,
//! since it deliberately bends a rule this codebase otherwise holds hard.
//! `larust_auth::Policy`'s own doc comment states the house style plainly:
//! "a typo in an ability name is a compile error, not a silently-always-
//! false runtime lookup." Spatie's package is the opposite by *design* —
//! an admin edits roles and permissions from a settings screen, with no
//! redeploy, which is the entire point of the package existing instead of
//! everyone just writing `Policy` methods. That's genuinely runtime data,
//! not a corner this crate is cutting.
//!
//! So the split is: **names are compile-checked, assignment is not.** An
//! app defines its own permission/role set as a plain Rust type
//! implementing [`PermissionName`]/[`RoleName`] — a typo in
//! `Permission::EditPosts` is caught by `rustc`, same as any other
//! misspelled identifier. What *is* runtime data, stored in SQLite, is
//! purely the assignment graph: which roles exist, which permissions each
//! role carries, and which users have which roles/permissions — exactly
//! the part an admin actually needs to change without a redeploy.
//!
//! Re-exported through `larust_support::permission` (see
//! `crates/larust-support/src/lib.rs`) so generated apps depend only on
//! `larust-support`, never on this crate directly.
//!
//! **`@can(expr)`/`@role(expr)` template directives now exist** (see
//! `larust_view::ast::Node::Can`/`Node::Role`, and their codegen in
//! `larust-macros/src/view.rs`) — a `.blade.xr` template checks
//! `has_permission_to`/`has_role` directly, without a route handler
//! pre-computing a bool and passing it in as a plain context variable.
//! `expr` is a raw Rust expression (`@can(Permission::EditPosts)`, not a
//! quoted string), carrying this crate's own "names are compile-checked"
//! half of its hybrid design all the way into templates — a typo'd
//! variant name is a `rustc` error at the template's own call site, the
//! same guarantee this crate's Rust-side callers already have. Requires a
//! `user: &U` (`U: Authenticatable`) binding in the `view!` context and an
//! async, `Result`-returning call site, checked eagerly at macro-expansion
//! time — the same shape `@wire(...)`'s own `session` requirement already
//! established.
//!
//! ## Deliberately out of scope for this version
//!
//! - **No `role:admin`/`permission:edit-posts` middleware-string
//!   recognition** in `xr convert`. `crates/larust-convert/src/routes.rs`
//!   already blanket-defers every `Route::middleware(...)->group(...)`
//!   call, deliberately (its own doc comment: "exactly the kind of
//!   semantic judgment call this phase avoids") — this crate doesn't
//!   special-case spatie's own aliases within that existing boundary.

use larust_auth::{authorize, Authenticatable};
use larust_core::AppError;
use larust_orm::Backend;
use sqlx::AnyPool;
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

/// A caller-defined permission name — implemented on an app's own enum
/// (or unit structs), the same "typo is a compile error" precedent
/// `larust_auth::Policy`/`larust_queue::Job::JOB_TYPE`/
/// `larust_notifications::Notification::NOTIFICATION_TYPE` already set. A
/// method, not an associated const, because an app's permission set is
/// naturally one enum with many variants, and Rust has no per-variant
/// associated consts:
///
/// ```ignore
/// #[derive(Copy, Clone)]
/// enum Permission { EditPosts, DeleteUsers }
/// impl larust_support::permission::PermissionName for Permission {
///     fn name(&self) -> &'static str {
///         match self {
///             Permission::EditPosts => "edit-posts",
///             Permission::DeleteUsers => "delete-users",
///         }
///     }
/// }
/// ```
pub trait PermissionName: Copy + Send + Sync + 'static {
    fn name(&self) -> &'static str;
}

/// A caller-defined role name — see [`PermissionName`]'s own doc comment,
/// same shape and same reasoning.
pub trait RoleName: Copy + Send + Sync + 'static {
    fn name(&self) -> &'static str;
}

/// Same lazy self-bootstrap idiom `larust-notifications`'s `ensure_table`
/// establishes — plain `CREATE TABLE IF NOT EXISTS` statements, no
/// migration file and no explicit startup call needed anywhere.
///
/// Memoized, but **not** behind a single process-wide flag — that was a
/// real regression once already (see `larust-notifications`'s own doc
/// comment): `larust_testing::test_transaction` swaps in a *different*
/// `&'static AnyPool` per test (a fresh, never-migrated-by-this-module
/// database), so one global "already ensured" bool would wrongly skip
/// table creation for a database that never actually got it. Keyed by
/// the pool's own memory address instead: production always resolves the
/// same process-wide pool (`larust_orm::pool()`'s own `OnceLock`), so
/// this ensures once for the life of the process; each test's own
/// swapped-in pool is a different address, so it gets its own cache entry
/// and is ensured fresh. Safe to key on the raw address rather than the
/// pool's identity via some other means: a pool is never deallocated
/// before process exit (the process-wide one lives in a `OnceLock` that's
/// never cleared; a test's own lives for that whole test), so no address
/// is ever reused by an unrelated pool while an entry for it is still
/// live in this cache. The short lock below is only ever held for a
/// `HashSet` lookup/insert, never across the `.await` calls that actually
/// touch the database, so concurrent callers first hitting a genuinely
/// new pool can't serialize behind each other here — at worst, more than
/// one of them redundantly runs the (idempotent, `IF NOT EXISTS`)
/// statements once before the cache entry lands, which is still strictly
/// better than every call paying that cost forever.
///
/// `user_id`/`role_id`/`permission_id` are plain `INTEGER`, not typed
/// foreign keys into an app-owned `users` table this crate has no
/// visibility into — the same reasoning `larust-notifications`'s own
/// `notifiable_id` column already uses. `user_permissions` (direct,
/// role-independent grants) covers spatie's own "direct permission" case,
/// for fidelity with the package this is standing in for.
async fn ensure_tables(pool: &AnyPool) -> Result<(), AppError> {
    static ENSURED_POOLS: OnceLock<Mutex<HashSet<usize>>> = OnceLock::new();
    let key = pool as *const AnyPool as usize;
    let cache = ENSURED_POOLS.get_or_init(|| Mutex::new(HashSet::new()));
    if cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains(&key)
    {
        return Ok(());
    }

    let (roles_table, permissions_table) = match larust_orm::backend() {
        Backend::Sqlite => (
            "CREATE TABLE IF NOT EXISTS roles (\
                id INTEGER PRIMARY KEY AUTOINCREMENT, \
                name TEXT NOT NULL UNIQUE\
             )",
            "CREATE TABLE IF NOT EXISTS permissions (\
                id INTEGER PRIMARY KEY AUTOINCREMENT, \
                name TEXT NOT NULL UNIQUE\
             )",
        ),
        Backend::MySql => (
            "CREATE TABLE IF NOT EXISTS roles (\
                id INTEGER PRIMARY KEY AUTO_INCREMENT, \
                name VARCHAR(255) NOT NULL UNIQUE\
             )",
            "CREATE TABLE IF NOT EXISTS permissions (\
                id INTEGER PRIMARY KEY AUTO_INCREMENT, \
                name VARCHAR(255) NOT NULL UNIQUE\
             )",
        ),
        Backend::Postgres => (
            "CREATE TABLE IF NOT EXISTS roles (\
                id INTEGER GENERATED ALWAYS AS IDENTITY PRIMARY KEY, \
                name TEXT NOT NULL UNIQUE\
             )",
            "CREATE TABLE IF NOT EXISTS permissions (\
                id INTEGER GENERATED ALWAYS AS IDENTITY PRIMARY KEY, \
                name TEXT NOT NULL UNIQUE\
             )",
        ),
    };

    for statement in [
        roles_table,
        permissions_table,
        "CREATE TABLE IF NOT EXISTS role_permissions (\
            role_id INTEGER NOT NULL REFERENCES roles(id), \
            permission_id INTEGER NOT NULL REFERENCES permissions(id), \
            PRIMARY KEY (role_id, permission_id)\
         )",
        "CREATE TABLE IF NOT EXISTS user_roles (\
            user_id INTEGER NOT NULL, \
            role_id INTEGER NOT NULL REFERENCES roles(id), \
            PRIMARY KEY (user_id, role_id)\
         )",
        "CREATE TABLE IF NOT EXISTS user_permissions (\
            user_id INTEGER NOT NULL, \
            permission_id INTEGER NOT NULL REFERENCES permissions(id), \
            PRIMARY KEY (user_id, permission_id)\
         )",
    ] {
        sqlx::query(statement)
            .execute(pool)
            .await
            .map_err(|source| AppError::Internal(Box::new(source)))?;
    }

    cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(key);
    Ok(())
}

/// Three different ways to say "insert this row, and if it collides with
/// an existing unique/primary key, silently do nothing instead of
/// erroring": `INSERT OR IGNORE` (SQLite), `INSERT IGNORE` (MySQL), and
/// `INSERT ... ON CONFLICT DO NOTHING` (Postgres — no `INSERT OR IGNORE`
/// syntax at all, and the conflict clause goes at the *end*, not as a
/// prefix verb, unlike the other two). Every one of this crate's 5
/// idempotent-insert call sites builds its SQL through this one function
/// instead of duplicating the three-way branch (and, for Postgres, its own
/// `$N` placeholder numbering — see `larust_orm::placeholder`) five times.
fn insert_ignore_sql(table: &str, columns: &[&str]) -> String {
    let backend = larust_orm::backend();
    let column_list = columns.join(", ");
    let placeholders = (1..=columns.len())
        .map(|n| larust_orm::placeholder(backend, n))
        .collect::<Vec<_>>()
        .join(", ");
    match backend {
        Backend::Sqlite => {
            format!("INSERT OR IGNORE INTO {table} ({column_list}) VALUES ({placeholders})")
        }
        Backend::MySql => {
            format!("INSERT IGNORE INTO {table} ({column_list}) VALUES ({placeholders})")
        }
        Backend::Postgres => format!(
            "INSERT INTO {table} ({column_list}) VALUES ({placeholders}) ON CONFLICT DO NOTHING"
        ),
    }
}

async fn role_id(pool: &AnyPool, role: &str) -> Result<Option<i64>, AppError> {
    let sql = format!(
        "SELECT id FROM roles WHERE name = {}",
        larust_orm::placeholder(larust_orm::backend(), 1)
    );
    let row: Option<(i64,)> = sqlx::query_as(&sql)
        .bind(role)
        .fetch_optional(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    Ok(row.map(|(id,)| id))
}

async fn permission_id(pool: &AnyPool, permission: &str) -> Result<Option<i64>, AppError> {
    let sql = format!(
        "SELECT id FROM permissions WHERE name = {}",
        larust_orm::placeholder(larust_orm::backend(), 1)
    );
    let row: Option<(i64,)> = sqlx::query_as(&sql)
        .bind(permission)
        .fetch_optional(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    Ok(row.map(|(id,)| id))
}

/// Creates `role` if it doesn't already exist — idempotent (`INSERT OR
/// IGNORE`), matching the `Role::firstOrCreate`-style call a spatie-backed
/// app's own seeder would make. Typically called once, at app boot or
/// from a seeder, for every variant of the app's `RoleName` enum.
pub async fn create_role(role: impl RoleName) -> Result<(), AppError> {
    let pool = larust_orm::pool()?;
    ensure_tables(pool).await?;
    sqlx::query(&insert_ignore_sql("roles", &["name"]))
        .bind(role.name())
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    Ok(())
}

/// Creates `permission` if it doesn't already exist — see [`create_role`],
/// same idempotent shape.
pub async fn create_permission(permission: impl PermissionName) -> Result<(), AppError> {
    let pool = larust_orm::pool()?;
    ensure_tables(pool).await?;
    sqlx::query(&insert_ignore_sql("permissions", &["name"]))
        .bind(permission.name())
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    Ok(())
}

/// Grants `role` the ability to do `permission` — every user later
/// assigned `role` inherits it (see [`has_permission_to`]). `NotFound` if
/// either `role` or `permission` hasn't been [`create_role`]/
/// [`create_permission`]d yet — deliberately not auto-created from this
/// call, so a typo'd name here fails loudly instead of silently seeding a
/// stray row nothing else references.
pub async fn grant_role_permission(
    role: impl RoleName,
    permission: impl PermissionName,
) -> Result<(), AppError> {
    let pool = larust_orm::pool()?;
    ensure_tables(pool).await?;

    let Some(role_id) = role_id(pool, role.name()).await? else {
        return Err(AppError::NotFound);
    };
    let Some(permission_id) = permission_id(pool, permission.name()).await? else {
        return Err(AppError::NotFound);
    };

    sqlx::query(&insert_ignore_sql(
        "role_permissions",
        &["role_id", "permission_id"],
    ))
    .bind(role_id)
    .bind(permission_id)
    .execute(pool)
    .await
    .map_err(|source| AppError::Internal(Box::new(source)))?;
    Ok(())
}

/// Assigns `role` to `user` — Laravel's `$user->assignRole('admin')`.
/// `NotFound` if `role` hasn't been [`create_role`]d yet, same reasoning
/// as [`grant_role_permission`].
pub async fn assign_role<U: Authenticatable>(
    user: &U,
    role: impl RoleName,
) -> Result<(), AppError> {
    let pool = larust_orm::pool()?;
    ensure_tables(pool).await?;

    let Some(role_id) = role_id(pool, role.name()).await? else {
        return Err(AppError::NotFound);
    };

    sqlx::query(&insert_ignore_sql("user_roles", &["user_id", "role_id"]))
        .bind(user.auth_id())
        .bind(role_id)
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    Ok(())
}

/// Removes `role` from `user`, if they had it — a legal no-op if they
/// didn't (same "no meaningful double-removal error" reasoning
/// `larust-notifications`'s `mark_as_read` already applies to marking an
/// already-read notification read again). A nonexistent *role name* is
/// still `NotFound`, distinct from "existing role, not currently assigned".
pub async fn remove_role<U: Authenticatable>(
    user: &U,
    role: impl RoleName,
) -> Result<(), AppError> {
    let pool = larust_orm::pool()?;
    ensure_tables(pool).await?;

    let Some(role_id) = role_id(pool, role.name()).await? else {
        return Err(AppError::NotFound);
    };

    let backend = larust_orm::backend();
    let delete_sql = format!(
        "DELETE FROM user_roles WHERE user_id = {} AND role_id = {}",
        larust_orm::placeholder(backend, 1),
        larust_orm::placeholder(backend, 2),
    );
    sqlx::query(&delete_sql)
        .bind(user.auth_id())
        .bind(role_id)
        .execute(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    Ok(())
}

/// `true` if `user` currently has `role` assigned. An unrecognized role
/// name (never [`create_role`]d) simply reads as `false`, not an error —
/// unlike the write-side functions above, a read has no "which row did
/// you mean" ambiguity to fail loudly about.
pub async fn has_role<U: Authenticatable>(user: &U, role: impl RoleName) -> Result<bool, AppError> {
    let pool = larust_orm::pool()?;
    ensure_tables(pool).await?;

    // `(i64,)`, not `(bool,)`: `SELECT EXISTS(...)` returns a 0/1 integer
    // on both backends, and sqlx's `Any` driver — unlike the concrete
    // `Sqlite`/`MySql` types — doesn't coerce an integer column into
    // `bool` (confirmed empirically: decoding straight into `bool` here
    // fails with "Rust type `bool` is not compatible with SQL type
    // `BIGINT`" through `Any`), so the `!= 0` conversion is done by hand.
    let backend = larust_orm::backend();
    let sql = format!(
        "SELECT EXISTS(\
            SELECT 1 FROM user_roles ur \
            JOIN roles r ON r.id = ur.role_id \
            WHERE ur.user_id = {} AND r.name = {}\
         )",
        larust_orm::placeholder(backend, 1),
        larust_orm::placeholder(backend, 2),
    );
    let (exists,): (i64,) = sqlx::query_as(&sql)
        .bind(user.auth_id())
        .bind(role.name())
        .fetch_one(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    Ok(exists != 0)
}

/// Grants `permission` to `user` directly, bypassing roles entirely —
/// Laravel's `$user->givePermissionTo('edit-posts')`. `NotFound` if
/// `permission` hasn't been [`create_permission`]d yet.
pub async fn give_permission_to<U: Authenticatable>(
    user: &U,
    permission: impl PermissionName,
) -> Result<(), AppError> {
    let pool = larust_orm::pool()?;
    ensure_tables(pool).await?;

    let Some(permission_id) = permission_id(pool, permission.name()).await? else {
        return Err(AppError::NotFound);
    };

    sqlx::query(&insert_ignore_sql(
        "user_permissions",
        &["user_id", "permission_id"],
    ))
    .bind(user.auth_id())
    .bind(permission_id)
    .execute(pool)
    .await
    .map_err(|source| AppError::Internal(Box::new(source)))?;
    Ok(())
}

/// `true` if `user` has `permission` — granted directly via
/// [`give_permission_to`], *or* inherited through any role assigned via
/// [`assign_role`] that was in turn granted it via
/// [`grant_role_permission`]. One query, not two round trips. Same
/// "unrecognized name reads as `false`" reasoning as [`has_role`].
pub async fn has_permission_to<U: Authenticatable>(
    user: &U,
    permission: impl PermissionName,
) -> Result<bool, AppError> {
    let pool = larust_orm::pool()?;
    ensure_tables(pool).await?;

    // `(i64,)`, not `(bool,)` — see `has_role`'s own comment on why.
    let backend = larust_orm::backend();
    let sql = format!(
        "SELECT EXISTS(\
            SELECT 1 FROM user_permissions up \
            JOIN permissions p ON p.id = up.permission_id \
            WHERE up.user_id = {} AND p.name = {}\
         ) OR EXISTS(\
            SELECT 1 FROM user_roles ur \
            JOIN role_permissions rp ON rp.role_id = ur.role_id \
            JOIN permissions p ON p.id = rp.permission_id \
            WHERE ur.user_id = {} AND p.name = {}\
         )",
        larust_orm::placeholder(backend, 1),
        larust_orm::placeholder(backend, 2),
        larust_orm::placeholder(backend, 3),
        larust_orm::placeholder(backend, 4),
    );
    let (exists,): (i64,) = sqlx::query_as(&sql)
        .bind(user.auth_id())
        .bind(permission.name())
        .bind(user.auth_id())
        .bind(permission.name())
        .fetch_one(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    Ok(exists != 0)
}

/// [`has_permission_to`], converted into a 403 on failure — the same
/// "reuse the primitive, don't reinvent the check" pattern
/// `larust_auth::Policy`'s own `authorize_*` sugar methods already use
/// for `larust_auth::authorize`.
pub async fn authorize_permission<U: Authenticatable>(
    user: &U,
    permission: impl PermissionName,
) -> Result<(), AppError> {
    authorize(has_permission_to(user, permission).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestUser {
        id: i64,
    }

    impl Authenticatable for TestUser {
        fn auth_id(&self) -> i64 {
            self.id
        }

        async fn find_for_auth(_id: i64) -> Result<Option<Self>, AppError> {
            unreachable!("not exercised by these tests")
        }
    }

    #[derive(Copy, Clone)]
    enum Role {
        Admin,
        Editor,
        /// Deliberately never [`create_role`]d — exercises the `NotFound`
        /// path on every write-side function that takes a role.
        Uncreated,
    }

    impl RoleName for Role {
        fn name(&self) -> &'static str {
            match self {
                Role::Admin => "admin",
                Role::Editor => "editor",
                Role::Uncreated => "uncreated",
            }
        }
    }

    #[derive(Copy, Clone)]
    enum Permission {
        EditPosts,
        DeleteUsers,
    }

    impl PermissionName for Permission {
        fn name(&self) -> &'static str {
            match self {
                Permission::EditPosts => "edit-posts",
                Permission::DeleteUsers => "delete-users",
            }
        }
    }

    async fn connect_test_db() {
        let dir = tempfile::tempdir().unwrap().keep();
        let database_url = format!("sqlite://{}/test.sqlite", dir.display());
        larust_orm::connect(&database_url).await.unwrap();
    }

    /// All scenarios share one test function, not several — `larust_orm::
    /// connect()` sets a process-wide pool exactly once (a second call in
    /// the same test binary errors with "connect() called more than
    /// once"), the same constraint `larust-notifications`'s own test
    /// suite documents and works around. Each scenario uses its own,
    /// disjoint user ids so they can't interfere with each other despite
    /// sharing one table/connection.
    #[tokio::test]
    async fn permissions_crate_behaves_correctly_across_every_scenario() {
        connect_test_db().await;

        create_role(Role::Admin).await.unwrap();
        create_role(Role::Editor).await.unwrap();
        create_permission(Permission::EditPosts).await.unwrap();
        create_permission(Permission::DeleteUsers).await.unwrap();
        // create_* is idempotent — calling it again for the same name is
        // a no-op, not an error.
        create_role(Role::Admin).await.unwrap();

        // A role's permissions are inherited by anyone assigned that role.
        grant_role_permission(Role::Editor, Permission::EditPosts)
            .await
            .unwrap();
        let alice = TestUser { id: 1 };
        assert!(!has_role(&alice, Role::Editor).await.unwrap());
        assert!(!has_permission_to(&alice, Permission::EditPosts)
            .await
            .unwrap());
        assign_role(&alice, Role::Editor).await.unwrap();
        assert!(has_role(&alice, Role::Editor).await.unwrap());
        assert!(has_permission_to(&alice, Permission::EditPosts)
            .await
            .unwrap());
        // Alice was never granted delete-users, directly or via a role.
        assert!(!has_permission_to(&alice, Permission::DeleteUsers)
            .await
            .unwrap());

        // A direct grant works independently of any role.
        let bob = TestUser { id: 2 };
        assert!(!has_permission_to(&bob, Permission::DeleteUsers)
            .await
            .unwrap());
        give_permission_to(&bob, Permission::DeleteUsers)
            .await
            .unwrap();
        assert!(has_permission_to(&bob, Permission::DeleteUsers)
            .await
            .unwrap());
        assert!(!has_role(&bob, Role::Admin).await.unwrap());

        // Removing a role revokes the permissions it carried.
        remove_role(&alice, Role::Editor).await.unwrap();
        assert!(!has_role(&alice, Role::Editor).await.unwrap());
        assert!(!has_permission_to(&alice, Permission::EditPosts)
            .await
            .unwrap());
        // Removing a role the user never had is a legal no-op.
        remove_role(&alice, Role::Admin).await.unwrap();

        // authorize_permission mirrors has_permission_to as a 403.
        let carol = TestUser { id: 3 };
        assign_role(&carol, Role::Admin).await.unwrap();
        grant_role_permission(Role::Admin, Permission::DeleteUsers)
            .await
            .unwrap();
        authorize_permission(&carol, Permission::DeleteUsers)
            .await
            .unwrap();
        match authorize_permission(&carol, Permission::EditPosts).await {
            Err(AppError::Http { status, .. }) => {
                assert_eq!(status, larust_core::axum::http::StatusCode::FORBIDDEN);
            }
            Err(other) => panic!("expected AppError::Http{{FORBIDDEN, ..}}, got {other:?}"),
            Ok(()) => panic!("expected AppError::Http{{FORBIDDEN, ..}}, got Ok(())"),
        }

        // Assigning/granting a nonexistent name fails loudly (NotFound)
        // rather than silently seeding a stray row.
        let dave = TestUser { id: 4 };
        assert!(matches!(
            assign_role(&dave, Role::Uncreated).await,
            Err(AppError::NotFound)
        ));
        assert!(matches!(
            remove_role(&dave, Role::Uncreated).await,
            Err(AppError::NotFound)
        ));
        assert!(matches!(
            grant_role_permission(Role::Uncreated, Permission::EditPosts).await,
            Err(AppError::NotFound)
        ));
        // A read against an unrecognized role/permission name is `false`,
        // not an error — no write-side ambiguity to fail loudly about.
        assert!(!has_role(&dave, Role::Uncreated).await.unwrap());
    }
}
