use crate::pool::{backend, pool, Backend};
use larust_core::AppError;
use sha2::{Digest, Sha256};
use std::path::Path;

/// Runs every `.sql` file in `migrations_dir` that hasn't already been
/// applied, in filename order (hence the numeric prefix `xr make:migration`
/// generates), tracking progress in a bookkeeping `_migrations` table —
/// the same shape as Laravel's own `migrations` table.
pub async fn run(migrations_dir: &Path) -> Result<(), AppError> {
    let pool = pool()?;

    let create_table = match backend() {
        Backend::Sqlite => {
            "CREATE TABLE IF NOT EXISTS _migrations (\
                name TEXT PRIMARY KEY, \
                checksum TEXT, \
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))\
             )"
        }
        Backend::MySql => {
            "CREATE TABLE IF NOT EXISTS _migrations (\
                name VARCHAR(255) PRIMARY KEY, \
                checksum TEXT, \
                applied_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP\
             )"
        }
        // Postgres has native, unbounded `TEXT` (no MySQL-style
        // `Any`-driver decode gap forcing a `VARCHAR(n)` cap here — see
        // `query_builder.rs`'s doc comment) and the same standard-SQL
        // `TIMESTAMP ... DEFAULT CURRENT_TIMESTAMP` MySQL already uses.
        Backend::Postgres => {
            "CREATE TABLE IF NOT EXISTS _migrations (\
                name TEXT PRIMARY KEY, \
                checksum TEXT, \
                applied_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP\
             )"
        }
    };
    sqlx::query(create_table)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(Box::new(e)))?;

    // Applications created before checksums existed have the old table —
    // upgrade it in place. SQLite-only: the `CREATE TABLE` above already
    // includes `checksum` for a brand-new database, so a fresh MySQL app
    // (MySQL support didn't exist before this column did) never has a
    // pre-existing table missing it — nothing to reconcile, and no need to
    // match MySQL's differently-worded duplicate-column error text at all.
    if backend() == Backend::Sqlite {
        if let Err(error) = sqlx::query("ALTER TABLE _migrations ADD COLUMN checksum TEXT")
            .execute(pool)
            .await
        {
            let duplicate_column = matches!(&error, sqlx::Error::Database(database)
                if database.message().contains("duplicate column name"));
            if !duplicate_column {
                return Err(AppError::Internal(Box::new(error)));
            }
        }
    }

    let mut files: Vec<_> = std::fs::read_dir(migrations_dir)
        .map_err(|e| AppError::Internal(Box::new(e)))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "sql"))
        .collect();
    files.sort();

    for path in files {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();

        let sql = std::fs::read_to_string(&path).map_err(|e| AppError::Internal(Box::new(e)))?;
        let checksum = format!("{:x}", Sha256::digest(sql.as_bytes()));

        let select_applied_sql = match backend() {
            Backend::Sqlite | Backend::MySql => {
                "SELECT name, checksum FROM _migrations WHERE name = ?"
            }
            Backend::Postgres => "SELECT name, checksum FROM _migrations WHERE name = $1",
        };
        let already_applied: Option<(String, Option<String>)> = sqlx::query_as(select_applied_sql)
            .bind(&name)
            .fetch_optional(pool)
            .await
            .map_err(|e| AppError::Internal(Box::new(e)))?;
        if let Some((_, stored_checksum)) = already_applied {
            match stored_checksum {
                Some(stored_checksum) if stored_checksum != checksum => {
                    return Err(AppError::Internal(Box::new(std::io::Error::other(format!(
                        "migration {name} was changed after it was applied; create a new migration instead"
                    )))));
                }
                Some(_) => {}
                // Treat a pre-checksum migration as trusted at upgrade time;
                // subsequent runs detect any modification.
                None => {
                    let update_checksum_sql = match backend() {
                        Backend::Sqlite | Backend::MySql => {
                            "UPDATE _migrations SET checksum = ? WHERE name = ?"
                        }
                        Backend::Postgres => "UPDATE _migrations SET checksum = $1 WHERE name = $2",
                    };
                    sqlx::query(update_checksum_sql)
                        .bind(&checksum)
                        .bind(&name)
                        .execute(pool)
                        .await
                        .map_err(|e| AppError::Internal(Box::new(e)))?;
                }
            }
            continue;
        }

        let mut tx = pool
            .begin()
            .await
            .map_err(|e| AppError::Internal(Box::new(e)))?;
        // `raw_sql` delegates statement parsing to SQLite, so trigger
        // bodies, comments, and string literals containing semicolons work
        // correctly. Splitting on `;` here used to corrupt valid migrations.
        sqlx::raw_sql(&sql)
            .execute(&mut *tx)
            .await
            .map_err(|e| AppError::Internal(Box::new(e)))?;
        let insert_migration_sql = match backend() {
            Backend::Sqlite | Backend::MySql => {
                "INSERT INTO _migrations (name, checksum) VALUES (?, ?)"
            }
            Backend::Postgres => "INSERT INTO _migrations (name, checksum) VALUES ($1, $2)",
        };
        sqlx::query(insert_migration_sql)
            .bind(&name)
            .bind(&checksum)
            .execute(&mut *tx)
            .await
            .map_err(|e| AppError::Internal(Box::new(e)))?;
        tx.commit()
            .await
            .map_err(|e| AppError::Internal(Box::new(e)))?;

        println!("Migrated: {name}");
    }

    Ok(())
}

/// Drops every table in the connected database (including `_migrations`
/// itself) and reapplies every migration in `migrations_dir` from scratch —
/// Laravel's `migrate:fresh`. Forward-only, like [`run`]: there is no
/// `down()`/rollback anywhere in this codebase, so a Laravel-style
/// `migrate:refresh` (rollback + reapply) has nothing to build on; `fresh`
/// needs no rollback and is what this framework can actually offer.
///
/// Foreign keys are disabled for the drop pass (SQLite: `PRAGMA
/// foreign_keys`; MySQL: `FOREIGN_KEY_CHECKS`) so table order doesn't
/// matter; Postgres instead drops each table `CASCADE`, since it has no
/// equivalent session-wide toggle.
///
/// The PRAGMA/`SET` and every `DROP TABLE` run on one connection acquired
/// from the pool (not `pool` itself as the executor) and held for the whole
/// pass — both are session/connection-scoped settings, and `Pool::execute`
/// is free to hand different calls different physical connections from the
/// pool, which silently drops the toggle before the next `DROP TABLE` sees
/// it (a real bug caught live: the very first run failed with a genuine
/// SQLite "FOREIGN KEY constraint failed" because the `OFF` PRAGMA had
/// landed on a connection the drop loop never actually used).
///
/// **`sessions` is deliberately never dropped**, unlike `_migrations` (which
/// *is*, on purpose — see above). Unlike every other table here, `sessions`
/// isn't tracked by `migrations_dir` at all: `larust_http::session`'s store
/// creates it once, with `CREATE TABLE IF NOT EXISTS`, the moment
/// `Router::with_sessions` boots — not something `run()`'s replay above can
/// recreate. Caught live: dropping it broke the *already-running* server's
/// own session middleware immediately (every request start failing with
/// "no such table: sessions", including the dashboard's own login), with no
/// way to recover short of restarting the process — since nothing re-runs
/// that one-time `CREATE TABLE IF NOT EXISTS` again until the next boot.
/// Treated as framework session-store plumbing, not app data to reset.
pub async fn fresh(migrations_dir: &Path) -> Result<(), AppError> {
    let pool = pool()?;
    let tables: Vec<String> = crate::introspect::table_names()
        .await?
        .into_iter()
        .filter(|table| table != "sessions")
        .collect();
    let mut conn = pool
        .acquire()
        .await
        .map_err(|e| AppError::Internal(Box::new(e)))?;

    if backend() == Backend::Sqlite {
        sqlx::raw_sql("PRAGMA foreign_keys = OFF")
            .execute(&mut *conn)
            .await
            .map_err(|e| AppError::Internal(Box::new(e)))?;
    } else if backend() == Backend::MySql {
        sqlx::raw_sql("SET FOREIGN_KEY_CHECKS = 0")
            .execute(&mut *conn)
            .await
            .map_err(|e| AppError::Internal(Box::new(e)))?;
    }

    for table in &tables {
        let sql = if backend() == Backend::Postgres {
            format!("DROP TABLE \"{table}\" CASCADE")
        } else {
            format!("DROP TABLE \"{table}\"")
        };
        sqlx::raw_sql(&sql)
            .execute(&mut *conn)
            .await
            .map_err(|e| AppError::Internal(Box::new(e)))?;
    }

    if backend() == Backend::Sqlite {
        sqlx::raw_sql("PRAGMA foreign_keys = ON")
            .execute(&mut *conn)
            .await
            .map_err(|e| AppError::Internal(Box::new(e)))?;
    } else if backend() == Backend::MySql {
        sqlx::raw_sql("SET FOREIGN_KEY_CHECKS = 1")
            .execute(&mut *conn)
            .await
            .map_err(|e| AppError::Internal(Box::new(e)))?;
    }

    drop(conn);
    run(migrations_dir).await
}
