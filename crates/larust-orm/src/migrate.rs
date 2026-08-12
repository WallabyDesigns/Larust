use crate::pool::pool;
use larust_core::AppError;
use std::path::Path;

/// Runs every `.sql` file in `migrations_dir` that hasn't already been
/// applied, in filename order (hence the numeric prefix `xr make:migration`
/// generates), tracking progress in a bookkeeping `_migrations` table —
/// the same shape as Laravel's own `migrations` table.
pub async fn run(migrations_dir: &Path) -> Result<(), AppError> {
    let pool = pool()?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS _migrations (\
            name TEXT PRIMARY KEY, \
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))\
         )",
    )
    .execute(pool)
    .await
    .map_err(|e| AppError::Internal(Box::new(e)))?;

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

        let already_applied: Option<(String,)> =
            sqlx::query_as("SELECT name FROM _migrations WHERE name = ?")
                .bind(&name)
                .fetch_optional(pool)
                .await
                .map_err(|e| AppError::Internal(Box::new(e)))?;
        if already_applied.is_some() {
            continue;
        }

        let sql = std::fs::read_to_string(&path).map_err(|e| AppError::Internal(Box::new(e)))?;

        let mut tx = pool
            .begin()
            .await
            .map_err(|e| AppError::Internal(Box::new(e)))?;
        for statement in sql.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            sqlx::query(statement)
                .execute(&mut *tx)
                .await
                .map_err(|e| AppError::Internal(Box::new(e)))?;
        }
        sqlx::query("INSERT INTO _migrations (name) VALUES (?)")
            .bind(&name)
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
