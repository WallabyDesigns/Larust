// Separate test binary - same reasoning as `larust-cache/tests/*.rs`:
// `larust_orm::connect()` can only succeed once per process, and each
// `tests/*.rs` file compiles to its own binary.

async fn connect_test_db() {
    let dir = tempfile::tempdir().unwrap().keep();
    let database_url = format!("sqlite://{}/test.sqlite", dir.display());
    larust_orm::connect(&database_url).await.unwrap();
}

#[tokio::test]
async fn prune_failed_jobs_deletes_only_rows_older_than_the_retention_window() {
    connect_test_db().await;

    // `larust_queue::prune_failed_jobs()` lazily creates `failed_jobs` the
    // same way `dispatch`/`work` do - touch the queue once first so the
    // table exists to insert directly into.
    larust_queue::prune_failed_jobs().await.unwrap();

    let pool = larust_orm::pool().unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    const THIRTY_ONE_DAYS_SECS: i64 = 31 * 24 * 60 * 60;

    sqlx::query(
        "INSERT INTO failed_jobs (job_type, payload, error, failed_at) VALUES (?, ?, ?, ?)",
    )
    .bind("old_job")
    .bind("{}")
    .bind("old failure")
    .bind(now - THIRTY_ONE_DAYS_SECS)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO failed_jobs (job_type, payload, error, failed_at) VALUES (?, ?, ?, ?)",
    )
    .bind("recent_job")
    .bind("{}")
    .bind("recent failure")
    .bind(now)
    .execute(pool)
    .await
    .unwrap();

    larust_queue::prune_failed_jobs().await.unwrap();

    let remaining: Vec<(String,)> = sqlx::query_as("SELECT job_type FROM failed_jobs")
        .fetch_all(pool)
        .await
        .unwrap();
    assert_eq!(
        remaining,
        vec![("recent_job".to_string(),)],
        "only the row older than the retention window should have been pruned"
    );
}
