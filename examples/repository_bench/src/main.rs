//! Cross-backend break/load test for `Repository<T>` — SQLite/MySQL/
//! Postgres via `#[derive(Model)]`'s generated `AnyRepository<T>` impl,
//! and SQL Server via a hand-written `Repository<T>` impl mirroring
//! `larust-mssql/tests/widget_repository.rs`'s own `WidgetRepository`
//! example. Not a permanent `cargo test` suite — a one-shot verification
//! and benchmark tool, run once per backend (see below for why) with
//! real results transcribed into `docs/ARCHITECTURE.md`'s "Data access
//! (`larust-repository`) and its benchmarks" section.
//!
//! **Run once per backend, never all four in one process**:
//! `larust_orm::connect()` is a hard once-per-process singleton (confirmed
//! throughout this codebase's own test suites — a second call always
//! errors "connect() called more than once"), so SQLite/MySQL/Postgres
//! (all three funnel through that same global pool) can only ever use one
//! per process invocation. SQL Server goes through `larust_mssql`'s own,
//! separate global instead, but is still run as its own invocation for
//! symmetry and a clean, comparable set of results.
//!
//! Credentials below are placeholders for a throwaway local Docker
//! container spun up just for this run, not real secrets — substitute
//! whatever your own local instance actually uses.
//!
//! ```text
//! cargo run -p repository_bench -- sqlite
//! cargo run -p repository_bench -- mysql     'mysql://root:<password>@127.0.0.1:3307/larust_bench'
//! cargo run -p repository_bench -- postgres  'postgres://postgres:<password>@127.0.0.1:5433/larust_bench'
//! cargo run -p repository_bench -- mssql     127.0.0.1 1433 sa <password> larust_bench
//! ```
//!
//! Every backend's `bench_items` table is assumed to already exist — this
//! tool only ever reads/writes rows, it doesn't manage schema, matching
//! `larust-repository`'s own "no migrations" stance for non-SQL-family
//! backends. `name` must be wide enough for the large-payload break test's
//! 3900-character string (a real bug hit during this crate's own
//! development: 255-char columns silently truncated on MySQL/SQL Server):
//!
//! ```text
//! -- SQLite / Postgres
//! CREATE TABLE bench_items (id INTEGER PRIMARY KEY AUTOINCREMENT/GENERATED ALWAYS AS IDENTITY,
//!                            name TEXT, value INTEGER);
//! -- MySQL
//! CREATE TABLE bench_items (id BIGINT AUTO_INCREMENT PRIMARY KEY,
//!                            name VARCHAR(4000), value BIGINT);
//! -- SQL Server
//! CREATE TABLE bench_items (id INT IDENTITY PRIMARY KEY,
//!                            name NVARCHAR(4000), value INT);
//! ```

use futures_util::future::join_all;
use larust_support::orm::sqlx;
use larust_support::orm::AnyRepository;
use larust_support::repository::Repository;
use larust_support::{AppError, Model};
use std::time::{Duration, Instant};

#[derive(Model, sqlx::FromRow, Debug, Clone)]
#[table("bench_items")]
pub struct BenchItem {
    #[primary_key]
    pub id: i64,
    pub name: String,
    pub value: i64,
}

/// Hand-written `Repository<BenchItem>` against a real SQL Server
/// connection via `tiberius` — the template every real app copies for
/// its own model (see `larust-mssql`'s own crate doc comment for why no
/// generic version of this can exist), mirrored here from
/// `WidgetRepository` in `larust-mssql/tests/widget_repository.rs`.
struct MssqlBenchRepository;

impl Repository<BenchItem> for MssqlBenchRepository {
    type Filter = String;
    type Id = i64;

    async fn find(&self, id: Self::Id) -> Result<Option<BenchItem>, AppError> {
        let mut client = larust_mssql::client().await?;
        let stream = client
            .query(
                "SELECT id, name, value FROM bench_items WHERE id = @P1",
                &[&id],
            )
            .await
            .map_err(|e| AppError::Internal(Box::new(e)))?;
        let row = stream
            .into_row()
            .await
            .map_err(|e| AppError::Internal(Box::new(e)))?;
        Ok(row.map(|row| BenchItem {
            id: row.get::<i32, _>("id").unwrap() as i64,
            name: row.get::<&str, _>("name").unwrap().to_string(),
            value: row.get::<i32, _>("value").unwrap() as i64,
        }))
    }

    async fn query(&self, filter: Self::Filter) -> Result<Vec<BenchItem>, AppError> {
        let mut client = larust_mssql::client().await?;
        let sql = format!("SELECT id, name, value FROM bench_items WHERE {filter}");
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
            .map(|row| BenchItem {
                id: row.get::<i32, _>("id").unwrap() as i64,
                name: row.get::<&str, _>("name").unwrap().to_string(),
                value: row.get::<i32, _>("value").unwrap() as i64,
            })
            .collect())
    }

    async fn create(&self, value: BenchItem) -> Result<BenchItem, AppError> {
        let mut client = larust_mssql::client().await?;
        // `OUTPUT INSERTED.id`, not `SCOPE_IDENTITY()` — see
        // `widget_repository.rs`'s own `create()` for why the latter comes
        // back NULL through `tiberius`'s RPC-based `query()`.
        let value_i32 = value.value as i32;
        let stream = client
            .query(
                "INSERT INTO bench_items (name, value) OUTPUT INSERTED.id VALUES (@P1, @P2)",
                &[&value.name, &value_i32],
            )
            .await
            .map_err(|e| AppError::Internal(Box::new(e)))?;
        let row = stream
            .into_row()
            .await
            .map_err(|e| AppError::Internal(Box::new(e)))?
            .expect("OUTPUT INSERTED.id always returns exactly one row");
        let id: i32 = row.get(0).expect("OUTPUT INSERTED.id was NULL");
        Ok(BenchItem {
            id: id as i64,
            name: value.name,
            value: value.value,
        })
    }

    async fn update(&self, id: Self::Id, value: BenchItem) -> Result<BenchItem, AppError> {
        let mut client = larust_mssql::client().await?;
        let value_i32 = value.value as i32;
        client
            .execute(
                "UPDATE bench_items SET name = @P1, value = @P2 WHERE id = @P3",
                &[&value.name, &value_i32, &id],
            )
            .await
            .map_err(|e| AppError::Internal(Box::new(e)))?;
        Ok(BenchItem {
            id,
            name: value.name,
            value: value.value,
        })
    }

    async fn delete(&self, id: Self::Id) -> Result<(), AppError> {
        let mut client = larust_mssql::client().await?;
        client
            .execute("DELETE FROM bench_items WHERE id = @P1", &[&id])
            .await
            .map_err(|e| AppError::Internal(Box::new(e)))?;
        Ok(())
    }
}

/// How many rows the load-test phase creates/finds/updates/deletes.
/// "Thousands of ops," not a full stress-scale benchmark — enough to
/// smooth out per-request overhead noise and expose a real ops/sec
/// figure per backend, not a load-bearing capacity claim.
const LOAD_N: usize = 3000;

/// How many creates run truly concurrently in the break-test phase — high
/// enough to actually contend for connections/locks, low enough to run in
/// well under a second even on the slowest backend tested.
const CONCURRENT_N: usize = 50;

struct BenchResults {
    backend: &'static str,
    missing_id_ok: bool,
    unicode_round_trip_ok: bool,
    large_payload_ok: bool,
    concurrent_writes_ok: bool,
    create_ops_per_sec: f64,
    find_ops_per_sec: f64,
    update_ops_per_sec: f64,
    delete_ops_per_sec: f64,
}

fn ops_per_sec(n: usize, elapsed: Duration) -> f64 {
    n as f64 / elapsed.as_secs_f64()
}

async fn run_suite<R>(repo: &R, backend: &'static str) -> BenchResults
where
    R: Repository<BenchItem, Id = i64> + Sync,
{
    eprintln!("--- {backend}: break tests ---");

    // A nonexistent id must come back `Ok(None)`, never an error.
    let missing_id_ok = matches!(repo.find(999_999_999).await, Ok(None));
    eprintln!("  missing id -> Ok(None): {missing_id_ok}");

    // Unicode/special characters must round-trip byte-for-byte — a real,
    // not hypothetical, class of bug (mismatched charset/collation on a
    // freshly created table is a classic MySQL footgun in particular).
    let unicode_name = "日本語 🦀 Ñoño Zürich \"quoted\" O'Brien".to_string();
    let unicode_round_trip_ok = match repo
        .create(BenchItem {
            id: 0,
            name: unicode_name.clone(),
            value: 42,
        })
        .await
    {
        Ok(created) => {
            let refetched = repo.find(created.id).await.ok().flatten();
            let ok = refetched.as_ref().map(|r| &r.name) == Some(&unicode_name);
            let _ = repo.delete(created.id).await;
            ok
        }
        Err(error) => {
            eprintln!("  unicode create failed: {error}");
            false
        }
    };
    eprintln!("  unicode round trip: {unicode_round_trip_ok}");

    // A large-ish payload (near the VARCHAR(4000) cap this codebase's own
    // MySQL tables already use elsewhere, e.g. larust-queue's `payload`
    // column) must be accepted, not silently truncated or rejected.
    let large_name = "x".repeat(3900);
    let large_payload_ok = match repo
        .create(BenchItem {
            id: 0,
            name: large_name.clone(),
            value: 1,
        })
        .await
    {
        Ok(created) => {
            let refetched = repo.find(created.id).await.ok().flatten();
            let ok = refetched.as_ref().map(|r| r.name.len()) == Some(large_name.len());
            let _ = repo.delete(created.id).await;
            ok
        }
        Err(error) => {
            eprintln!("  large payload create failed: {error}");
            false
        }
    };
    eprintln!("  large payload (3900 chars) round trip: {large_payload_ok}");

    // Concurrent creates must all succeed with distinct ids -- no lost
    // writes, no deadlock, no silently-shared id.
    let futures = (0..CONCURRENT_N).map(|i| {
        repo.create(BenchItem {
            id: 0,
            name: format!("concurrent-{i}"),
            value: i as i64,
        })
    });
    let results = join_all(futures).await;
    let mut concurrent_ids: Vec<i64> = Vec::with_capacity(CONCURRENT_N);
    let mut all_succeeded = true;
    for result in results {
        match result {
            Ok(item) => concurrent_ids.push(item.id),
            Err(error) => {
                eprintln!("  concurrent create failed: {error}");
                all_succeeded = false;
            }
        }
    }
    let unique_count = {
        let mut sorted = concurrent_ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        sorted.len()
    };
    let concurrent_writes_ok = all_succeeded && unique_count == CONCURRENT_N;
    for id in concurrent_ids {
        let _ = repo.delete(id).await;
    }
    eprintln!("  {CONCURRENT_N} concurrent creates, all unique ids: {concurrent_writes_ok}");

    eprintln!("--- {backend}: load test ({LOAD_N} rows) ---");

    let start = Instant::now();
    let mut ids = Vec::with_capacity(LOAD_N);
    for i in 0..LOAD_N {
        let item = repo
            .create(BenchItem {
                id: 0,
                name: format!("load-{i}"),
                value: i as i64,
            })
            .await
            .expect("load-phase create should not fail");
        ids.push(item.id);
    }
    let create_elapsed = start.elapsed();
    eprintln!(
        "  create: {create_elapsed:?} ({:.0} ops/sec)",
        ops_per_sec(LOAD_N, create_elapsed)
    );

    let start = Instant::now();
    for &id in &ids {
        repo.find(id)
            .await
            .expect("load-phase find should not fail");
    }
    let find_elapsed = start.elapsed();
    eprintln!(
        "  find:   {find_elapsed:?} ({:.0} ops/sec)",
        ops_per_sec(LOAD_N, find_elapsed)
    );

    let start = Instant::now();
    for &id in &ids {
        repo.update(
            id,
            BenchItem {
                id: 0,
                name: "updated".to_string(),
                value: 0,
            },
        )
        .await
        .expect("load-phase update should not fail");
    }
    let update_elapsed = start.elapsed();
    eprintln!(
        "  update: {update_elapsed:?} ({:.0} ops/sec)",
        ops_per_sec(LOAD_N, update_elapsed)
    );

    let start = Instant::now();
    for &id in &ids {
        repo.delete(id)
            .await
            .expect("load-phase delete should not fail");
    }
    let delete_elapsed = start.elapsed();
    eprintln!(
        "  delete: {delete_elapsed:?} ({:.0} ops/sec)",
        ops_per_sec(LOAD_N, delete_elapsed)
    );

    BenchResults {
        backend,
        missing_id_ok,
        unicode_round_trip_ok,
        large_payload_ok,
        concurrent_writes_ok,
        create_ops_per_sec: ops_per_sec(LOAD_N, create_elapsed),
        find_ops_per_sec: ops_per_sec(LOAD_N, find_elapsed),
        update_ops_per_sec: ops_per_sec(LOAD_N, update_elapsed),
        delete_ops_per_sec: ops_per_sec(LOAD_N, delete_elapsed),
    }
}

fn print_results(results: &BenchResults) {
    println!();
    println!("=== {} ===", results.backend);
    println!(
        "correctness: missing_id={} unicode={} large_payload={} concurrent_writes={}",
        results.missing_id_ok,
        results.unicode_round_trip_ok,
        results.large_payload_ok,
        results.concurrent_writes_ok
    );
    println!(
        "| {} | {:.0} | {:.0} | {:.0} | {:.0} |",
        results.backend,
        results.create_ops_per_sec,
        results.find_ops_per_sec,
        results.update_ops_per_sec,
        results.delete_ops_per_sec
    );
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let backend = args.get(1).map(String::as_str).unwrap_or("");

    let results = match backend {
        "sqlite" => {
            let dir = tempfile::tempdir().unwrap().keep();
            let database_url = format!("sqlite://{}/bench.sqlite", dir.display());
            larust_support::orm::connect(&database_url).await.unwrap();
            larust_support::orm::sqlx::query(
                "CREATE TABLE IF NOT EXISTS bench_items (\
                    id INTEGER PRIMARY KEY AUTOINCREMENT, \
                    name TEXT NOT NULL, \
                    value INTEGER NOT NULL\
                 )",
            )
            .execute(larust_support::orm::pool().unwrap())
            .await
            .unwrap();
            let repo = AnyRepository::<BenchItem>::new();
            run_suite(&repo, "SQLite").await
        }
        "mysql" | "postgres" => {
            let url = args.get(2).expect("usage: mysql|postgres <DATABASE_URL>");
            larust_support::orm::connect(url).await.unwrap();
            let repo = AnyRepository::<BenchItem>::new();
            let label = if backend == "mysql" {
                "MySQL"
            } else {
                "Postgres"
            };
            run_suite(&repo, label).await
        }
        "mssql" => {
            let host = args
                .get(2)
                .cloned()
                .unwrap_or_else(|| "127.0.0.1".to_string());
            let port: u16 = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(1433);
            let username = args.get(4).cloned().unwrap_or_else(|| "sa".to_string());
            let password = args
                .get(5)
                .cloned()
                .expect("usage: mssql <host> <port> <user> <password> <database>");
            let database = args.get(6).cloned().unwrap_or_else(|| "master".to_string());
            larust_mssql::connect(&larust_mssql::MssqlConfig {
                host,
                port,
                database,
                username,
                password,
            })
            .await
            .unwrap();
            let repo = MssqlBenchRepository;
            run_suite(&repo, "SQL Server").await
        }
        other => {
            eprintln!("unknown backend {other:?} -- expected sqlite|mysql|postgres|mssql");
            std::process::exit(1);
        }
    };

    print_results(&results);
}
