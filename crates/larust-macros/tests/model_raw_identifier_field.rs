// A separate test binary from `model.rs`/other `tests/*.rs` files so its
// own `connect()` call doesn't collide with their process-wide `OnceLock`
// pool — each file under `tests/` is its own process.

use larust_support::Model;

#[derive(Model, sqlx::FromRow, Debug, PartialEq)]
#[table("widgets")]
pub struct Widget {
    #[primary_key]
    pub id: i64,
    /// A raw identifier — `type` is a Rust keyword, and a plausible real
    /// column name (e.g. for a polymorphic-style table).
    pub r#type: String,
}

#[tokio::test]
async fn model_with_raw_identifier_field_compiles_and_round_trips() {
    let db_dir = tempfile::tempdir().unwrap();
    let database_url = format!("sqlite://{}/test.sqlite", db_dir.path().display());
    larust_support::orm::connect(&database_url).await.unwrap();
    sqlx::query("CREATE TABLE widgets (id INTEGER PRIMARY KEY AUTOINCREMENT, type TEXT NOT NULL)")
        .execute(larust_support::orm::pool().unwrap())
        .await
        .unwrap();

    assert_eq!(Widget::TYPE, "type");

    let created = Widget::create(NewWidget {
        r#type: "gadget".to_string(),
    })
    .await
    .unwrap();
    assert_eq!(created.r#type, "gadget");

    let found = Widget::find(created.id).await.unwrap();
    assert_eq!(found, Some(created));
}
