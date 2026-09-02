//! Integration coverage for `sql::introspect`/`sql::mutate` against a real
//! SQLite database — one test function, not several: `larust_orm::connect()`
//! is a process-wide `OnceLock` singleton (errors "connect() called more
//! than once" on a second call), the same convention every DB-touching
//! test file in this codebase already follows (`larust-orm`'s own tests,
//! `examples/repository_bench`).
//!
//! Two tables, deliberately shaped to exercise both PK cases this schema
//! actually has in the real app: `posts_like` (single-column PK, mirrors
//! `demo`'s own `posts`) and `post_tag_like` (composite PK, mirrors
//! `demo`'s own `post_tag`/`role_permissions`).

use serde_json::json;

#[tokio::test]
async fn sql_engine_round_trips_against_real_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    let database_url = format!("sqlite://{}/test.sqlite", dir.path().display());
    larust_orm::connect(&database_url).await.unwrap();
    let pool = larust_orm::pool().unwrap();

    sqlx::query(
        "CREATE TABLE posts_like (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            title TEXT NOT NULL, \
            views INTEGER NOT NULL DEFAULT 0, \
            note TEXT)",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TABLE post_tag_like (post_id INTEGER NOT NULL, tag_id INTEGER NOT NULL, \
         PRIMARY KEY (post_id, tag_id), \
         FOREIGN KEY (post_id) REFERENCES posts_like (id))",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("CREATE INDEX idx_post_tag_like_tag_id ON post_tag_like (tag_id)")
        .execute(pool)
        .await
        .unwrap();

    // list_tables sees both, alphabetical, nothing else.
    let tables = larust_db::sql::introspect::list_tables().await.unwrap();
    assert_eq!(tables, vec!["post_tag_like", "posts_like"]);

    // table_columns: names, not-null flags, inferred kinds.
    let columns = larust_db::sql::introspect::table_columns("posts_like")
        .await
        .unwrap();
    let by_name = |name: &str| columns.iter().find(|c| c.name == name).unwrap();
    // NOT `by_name("id").not_null` — a real SQLite quirk, not a bug here:
    // `INTEGER PRIMARY KEY` makes the column a ROWID alias, and SQLite's
    // own `PRAGMA table_info` reports `notnull = 0` for it regardless,
    // even though the PK constraint itself guarantees non-null.
    assert!(by_name("title").not_null);
    assert!(!by_name("note").not_null);
    assert_eq!(by_name("id").kind, sqlx::any::AnyTypeInfoKind::BigInt);
    assert_eq!(by_name("title").kind, sqlx::any::AnyTypeInfoKind::Text);

    // primary_key_columns: single-column and composite, in order.
    assert_eq!(
        larust_db::sql::introspect::primary_key_columns("posts_like")
            .await
            .unwrap(),
        vec!["id"]
    );
    assert_eq!(
        larust_db::sql::introspect::primary_key_columns("post_tag_like")
            .await
            .unwrap(),
        vec!["post_id", "tag_id"]
    );

    // insert_row -> a real row exists, readable via row_to_json through
    // run_raw.
    larust_db::sql::mutate::insert_row(
        "posts_like",
        &columns,
        &[
            ("title".to_string(), json!("Hello")),
            ("views".to_string(), json!(3)),
        ],
    )
    .await
    .unwrap();
    let rows = larust_db::sql::mutate::run_raw("SELECT * FROM posts_like")
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["title"], json!("Hello"));
    assert_eq!(rows[0]["views"], json!(3));
    assert_eq!(rows[0]["note"], serde_json::Value::Null);

    // update_row identified by the single-column PK.
    let id = rows[0]["id"].clone();
    larust_db::sql::mutate::update_row(
        "posts_like",
        &columns,
        &[("id".to_string(), id.clone())],
        &[("title".to_string(), json!("Updated"))],
    )
    .await
    .unwrap();
    let rows = larust_db::sql::mutate::run_raw("SELECT title FROM posts_like")
        .await
        .unwrap();
    assert_eq!(rows[0]["title"], json!("Updated"));

    // delete_row.
    larust_db::sql::mutate::delete_row("posts_like", &columns, &[("id".to_string(), id)])
        .await
        .unwrap();
    let rows = larust_db::sql::mutate::run_raw("SELECT * FROM posts_like")
        .await
        .unwrap();
    assert!(rows.is_empty());

    // Composite-PK insert/update/delete. `post_tag_like.post_id` has a real
    // FK onto `posts_like.id` (added for the index/FK-introspection
    // coverage below), so a real `posts_like` row must exist first — the
    // id=1 row from earlier was already deleted.
    larust_db::sql::mutate::insert_row(
        "posts_like",
        &columns,
        &[("title".to_string(), json!("For the tag FK"))],
    )
    .await
    .unwrap();
    let fk_post_id = larust_db::sql::mutate::run_raw("SELECT id FROM posts_like")
        .await
        .unwrap()[0]["id"]
        .clone();

    let tag_columns = larust_db::sql::introspect::table_columns("post_tag_like")
        .await
        .unwrap();
    larust_db::sql::mutate::insert_row(
        "post_tag_like",
        &tag_columns,
        &[
            ("post_id".to_string(), fk_post_id.clone()),
            ("tag_id".to_string(), json!(2)),
        ],
    )
    .await
    .unwrap();
    let pk = vec![
        ("post_id".to_string(), fk_post_id),
        ("tag_id".to_string(), json!(2)),
    ];
    // No non-PK columns to update on this table — exercise delete
    // directly, the realistic operation for a pure join-table row.
    larust_db::sql::mutate::delete_row("post_tag_like", &tag_columns, &pk)
        .await
        .unwrap();
    let rows = larust_db::sql::mutate::run_raw("SELECT * FROM post_tag_like")
        .await
        .unwrap();
    assert!(rows.is_empty());

    // run_raw against a non-SELECT statement: zero rows, no error.
    let rows = larust_db::sql::mutate::run_raw("UPDATE posts_like SET views = 0")
        .await
        .unwrap();
    assert!(rows.is_empty());

    // list_indexes: the real index created above shows up, alongside
    // SQLite's own auto-generated index for the composite PRIMARY KEY
    // (`PRAGMA index_list` reports both — a composite key isn't a ROWID
    // alias, so SQLite backs it with a hidden `sqlite_autoindex_*` index;
    // real SQLite behavior, not a bug in this introspection).
    let indexes = larust_db::sql::introspect::list_indexes("post_tag_like")
        .await
        .unwrap();
    assert_eq!(indexes.len(), 2);
    assert!(indexes
        .iter()
        .any(|row| row["name"] == json!("idx_post_tag_like_tag_id")));

    // A table with no indexes of its own returns an empty list, not an
    // error (SQLite's `PRAGMA index_list` on a table with only an implicit
    // ROWID and no explicit index/unique constraint returns zero rows).
    let no_indexes = larust_db::sql::introspect::list_indexes("posts_like")
        .await
        .unwrap();
    assert!(no_indexes.is_empty());

    // list_foreign_keys: the real FK created above shows up.
    let fks = larust_db::sql::introspect::list_foreign_keys("post_tag_like")
        .await
        .unwrap();
    assert_eq!(fks.len(), 1);
    assert_eq!(fks[0]["table"], json!("posts_like"));
    assert_eq!(fks[0]["from"], json!("post_id"));
    assert_eq!(fks[0]["to"], json!("id"));

    let no_fks = larust_db::sql::introspect::list_foreign_keys("posts_like")
        .await
        .unwrap();
    assert!(no_fks.is_empty());
}
