use std::io::Write;

#[derive(sqlx::FromRow, Debug, PartialEq)]
struct Post {
    id: i64,
    title: String,
}

#[tokio::test]
async fn connect_migrate_and_query_builder_round_trip() {
    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("test.sqlite");
    let database_url = format!("sqlite://{}", db_path.display());

    larust_orm::connect(&database_url).await.unwrap();

    let migrations_dir = tempfile::tempdir().unwrap();
    let migration_file = migrations_dir.path().join("0001_create_posts.sql");
    let mut f = std::fs::File::create(&migration_file).unwrap();
    write!(
        f,
        "CREATE TABLE posts (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL);"
    )
    .unwrap();
    drop(f);

    larust_orm::migrate(migrations_dir.path()).await.unwrap();
    // Running twice must be a no-op (idempotent), not fail on "table exists".
    larust_orm::migrate(migrations_dir.path()).await.unwrap();

    // SQL scripts, rather than a hand-rolled `split(';')`, preserve trigger
    // bodies containing internal semicolons.
    let trigger_file = migrations_dir.path().join("0002_create_post_audit.sql");
    std::fs::write(
        &trigger_file,
        r#"CREATE TABLE post_audit (post_id INTEGER NOT NULL);
CREATE TRIGGER audit_post AFTER INSERT ON posts BEGIN
    INSERT INTO post_audit (post_id) VALUES (NEW.id);
END;"#,
    )
    .unwrap();
    larust_orm::migrate(migrations_dir.path()).await.unwrap();

    let pool = larust_orm::pool().unwrap();
    sqlx::query("INSERT INTO posts (title) VALUES (?)")
        .bind("First post")
        .execute(pool)
        .await
        .unwrap();

    let audit_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM post_audit")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(audit_count.0, 1);
    sqlx::query("INSERT INTO posts (title) VALUES (?)")
        .bind("Second post")
        .execute(pool)
        .await
        .unwrap();

    let all: Vec<Post> = larust_orm::QueryBuilder::new("posts").get().await.unwrap();
    assert_eq!(all.len(), 2);

    let found: Option<Post> = larust_orm::QueryBuilder::new("posts")
        .where_eq("title", "Second post")
        .first()
        .await
        .unwrap();
    assert_eq!(
        found,
        Some(Post {
            id: 2,
            title: "Second post".to_string()
        })
    );

    let none: Option<Post> = larust_orm::QueryBuilder::new("posts")
        .where_eq("title", "Nonexistent")
        .first()
        .await
        .unwrap();
    assert_eq!(none, None);

    let latest: Vec<Post> = larust_orm::QueryBuilder::new("posts")
        .latest("id")
        .paginate(1)
        .await
        .unwrap();
    assert_eq!(
        latest,
        vec![Post {
            id: 2,
            title: "Second post".to_string()
        }]
    );

    sqlx::query("INSERT INTO posts (title) VALUES (?)")
        .bind("Third post")
        .execute(pool)
        .await
        .unwrap();

    // where_in: fetches exactly the requested rows, in one query — the
    // primitive relationship batch loaders (`load_*`, in larust-macros)
    // are built on.
    let mut in_results: Vec<Post> = larust_orm::QueryBuilder::new("posts")
        .where_in("id", vec![1i64, 3])
        .get()
        .await
        .unwrap();
    in_results.sort_by_key(|p| p.id);
    assert_eq!(
        in_results,
        vec![
            Post {
                id: 1,
                title: "First post".to_string()
            },
            Post {
                id: 3,
                title: "Third post".to_string()
            },
        ]
    );

    // An empty id list must yield zero rows, not a SQL syntax error
    // ("id" IN () is invalid in SQLite).
    let empty: Vec<Post> = larust_orm::QueryBuilder::new("posts")
        .where_in("id", Vec::<i64>::new())
        .get()
        .await
        .unwrap();
    assert!(empty.is_empty());

    // Migration contents are immutable after application. A modified file
    // must be replaced by a new migration, never silently ignored.
    std::fs::write(
        &migration_file,
        "CREATE TABLE posts (id INTEGER PRIMARY KEY);",
    )
    .unwrap();
    assert!(larust_orm::migrate(migrations_dir.path()).await.is_err());
}
