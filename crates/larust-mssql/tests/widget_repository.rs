//! A complete worked example: a hand-written `Repository<Widget>`
//! implementation against a real SQL Server connection via `tiberius` -
//! mirrors `larust-repository`'s own `InMemoryRepository` test in shape
//! (see that crate's `tests/in_memory_repository.rs`), just against a
//! real server instead of a `HashMap`. This is the template a real app
//! copies and adapts for its own model - see `larust_mssql`'s own crate
//! doc comment for why no generic version of this can exist.
//!
//! Requires a real SQL Server instance reachable via the `MSSQL_HOST`/
//! `MSSQL_PORT`/`MSSQL_USERNAME`/`MSSQL_PASSWORD` env vars (defaults:
//! `127.0.0.1`/`1433`/`sa`/`LarustTest123!`) with a `widgets` table
//! already created:
//!
//! ```sql
//! CREATE TABLE widgets (id INT IDENTITY(1,1) PRIMARY KEY, name NVARCHAR(255) NOT NULL);
//! ```
//!
//! Not run by default - `cargo test -p larust-mssql -- --ignored` opts in
//! explicitly, the same "this needs a real server, don't run it in a
//! normal `cargo test`" convention `larust-cli`'s own `dev_e2e.rs` tests
//! already establish.

use larust_core::AppError;
use larust_mssql::{client, connect, MssqlConfig};
use larust_repository::Repository;

#[derive(Debug, Clone, PartialEq)]
struct Widget {
    id: i64,
    name: String,
}

/// The hand-written `Repository<Widget>` implementation itself - real,
/// per-model code an app author writes, not something `larust-mssql`
/// generates. `Filter` is a raw `WHERE`-clause fragment here (deliberately
/// simple, matching `Repository`'s own "opaque to the trait" contract -
/// see that trait's doc comment); a real app might use a small enum of
/// conditions instead, the same design space `larust_orm::QueryBuilder`'s
/// own `Condition` type occupies for the SQL-family side.
struct WidgetRepository;

impl Repository<Widget> for WidgetRepository {
    type Filter = String;
    type Id = i64;

    async fn find(&self, id: Self::Id) -> Result<Option<Widget>, AppError> {
        let mut client = client().await?;
        let stream = client
            .query("SELECT id, name FROM widgets WHERE id = @P1", &[&id])
            .await
            .map_err(|e| AppError::Internal(Box::new(e)))?;
        let row = stream
            .into_row()
            .await
            .map_err(|e| AppError::Internal(Box::new(e)))?;
        Ok(row.map(|row| Widget {
            id: row.get::<i32, _>("id").unwrap() as i64,
            name: row.get::<&str, _>("name").unwrap().to_string(),
        }))
    }

    async fn query(&self, filter: Self::Filter) -> Result<Vec<Widget>, AppError> {
        let mut client = client().await?;
        let sql = format!("SELECT id, name FROM widgets WHERE {filter}");
        let stream = client
            .query(sql, &[])
            .await
            .map_err(|e| AppError::Internal(Box::new(e)))?;
        let rows = stream
            .into_first_result()
            .await
            .map_err(|e| AppError::Internal(Box::new(e)))?;
        Ok(rows
            .into_iter()
            .map(|row| Widget {
                id: row.get::<i32, _>("id").unwrap() as i64,
                name: row.get::<&str, _>("name").unwrap().to_string(),
            })
            .collect())
    }

    async fn create(&self, value: Widget) -> Result<Widget, AppError> {
        let mut client = client().await?;
        // `OUTPUT INSERTED.id` (SQL Server's own equivalent of Postgres's
        // `RETURNING`), not a follow-up `SELECT SCOPE_IDENTITY()` - found
        // via live testing that the latter comes back NULL here.
        // `tiberius`'s parameterized `execute()`/`query()` run through an
        // RPC call (confirmed by reading its source: both go through
        // `RpcProcId::ExecuteSQL`), and an RPC call is its own scope in
        // SQL Server's own `SCOPE_IDENTITY()` sense - by the time a
        // *separate* follow-up query runs, the INSERT's scope has already
        // closed, so `SCOPE_IDENTITY()` sees nothing. `OUTPUT` sidesteps
        // this entirely by returning the identity value from the same
        // statement, same call, same scope.
        let stream = client
            .query(
                "INSERT INTO widgets (name) OUTPUT INSERTED.id VALUES (@P1)",
                &[&value.name],
            )
            .await
            .map_err(|e| AppError::Internal(Box::new(e)))?;
        let row = stream
            .into_row()
            .await
            .map_err(|e| AppError::Internal(Box::new(e)))?
            .expect("OUTPUT INSERTED.id always returns exactly one row");
        let id: i32 = row.get(0).expect("OUTPUT INSERTED.id was NULL");
        Ok(Widget {
            id: id as i64,
            name: value.name,
        })
    }

    async fn update(&self, id: Self::Id, value: Widget) -> Result<Widget, AppError> {
        let mut client = client().await?;
        client
            .execute(
                "UPDATE widgets SET name = @P1 WHERE id = @P2",
                &[&value.name, &id],
            )
            .await
            .map_err(|e| AppError::Internal(Box::new(e)))?;
        Ok(Widget {
            id,
            name: value.name,
        })
    }

    async fn delete(&self, id: Self::Id) -> Result<(), AppError> {
        let mut client = client().await?;
        client
            .execute("DELETE FROM widgets WHERE id = @P1", &[&id])
            .await
            .map_err(|e| AppError::Internal(Box::new(e)))?;
        Ok(())
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[tokio::test]
#[ignore = "needs a real local SQL Server instance -- see this file's own doc comment"]
async fn widget_repository_supports_the_full_crud_round_trip_against_real_sql_server() {
    let config = MssqlConfig {
        host: env_or("MSSQL_HOST", "127.0.0.1"),
        port: env_or("MSSQL_PORT", "1433").parse().unwrap(),
        database: env_or("MSSQL_DATABASE", "master"),
        username: env_or("MSSQL_USERNAME", "sa"),
        password: env_or("MSSQL_PASSWORD", "LarustTest123!"),
    };
    connect(&config).await.unwrap();

    // Fresh table every run -- this test owns its own schema, the same
    // "own scratch table, drop and recreate" shape
    // `larust-macros`' own live-server tests use for their migrations.
    {
        let mut c = client().await.unwrap();
        let _ = c
            .execute("DROP TABLE IF EXISTS widgets", &[])
            .await
            .unwrap();
        c.execute(
            "CREATE TABLE widgets (id INT IDENTITY(1,1) PRIMARY KEY, name NVARCHAR(255) NOT NULL)",
            &[],
        )
        .await
        .unwrap();
    }

    let repo = WidgetRepository;

    let created = repo
        .create(Widget {
            id: 0,
            name: "First widget".to_string(),
        })
        .await
        .unwrap();
    assert_eq!(created.id, 1);
    assert_eq!(created.name, "First widget");

    let found = repo.find(created.id).await.unwrap();
    assert_eq!(found, Some(created.clone()));

    repo.create(Widget {
        id: 0,
        name: "Second widget".to_string(),
    })
    .await
    .unwrap();

    let matches = repo.query("name LIKE 'First%'".to_string()).await.unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].name, "First widget");

    let updated = repo
        .update(
            created.id,
            Widget {
                id: 0,
                name: "First widget, edited".to_string(),
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.name, "First widget, edited");
    assert_eq!(repo.find(created.id).await.unwrap(), Some(updated));

    repo.delete(created.id).await.unwrap();
    assert_eq!(repo.find(created.id).await.unwrap(), None);

    println!("ALL LARUST-MSSQL LIVE CHECKS PASSED");
}
