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

        let already_applied: Option<(String, Option<String>)> =
            sqlx::query_as("SELECT name, checksum FROM _migrations WHERE name = ?")
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
                    sqlx::query("UPDATE _migrations SET checksum = ? WHERE name = ?")
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
        sqlx::query("INSERT INTO _migrations (name, checksum) VALUES (?, ?)")
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
