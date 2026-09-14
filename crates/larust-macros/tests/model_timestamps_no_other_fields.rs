// Separate test binary (own process, own pool) - see
// `model_no_insertable_fields.rs`'s own comment on why.
//
// The specific interaction this pins down: `#[timestamps]` on a struct
// with zero *other* insertable fields still needs a real `INSERT INTO ...
// (created_at, updated_at) VALUES (?, ?)`, not the plain `DEFAULT VALUES`
// form `model_no_insertable_fields.rs` proves for a model with no
// timestamps at all - `insertable_names` (the caller-supplied columns) is
// empty in both cases, but `insert_column_names` (which also carries the
// two timestamp columns) isn't, and codegen branches on the latter.

use larust_support::Model;

#[derive(Model, sqlx::FromRow, Debug)]
#[table("pings")]
#[timestamps]
pub struct Ping {
    #[primary_key]
    pub id: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[tokio::test]
async fn timestamps_alone_still_insert_and_update_correctly() {
    let db_dir = tempfile::tempdir().unwrap();
    let database_url = format!("sqlite://{}/test.sqlite", db_dir.path().display());
    larust_support::orm::connect(&database_url).await.unwrap();
    sqlx::query(
        "CREATE TABLE pings (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            created_at INTEGER NOT NULL, \
            updated_at INTEGER NOT NULL\
        )",
    )
    .execute(larust_support::orm::pool().unwrap())
    .await
    .unwrap();

    let created = Ping::create(NewPing {}).await.unwrap();
    assert!(created.created_at > 0);
    assert_eq!(created.created_at, created.updated_at);

    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

    let updated = Ping::update(created.id, NewPing {}).await.unwrap();
    assert_eq!(updated.created_at, created.created_at);
    assert!(updated.updated_at > created.updated_at);
}
