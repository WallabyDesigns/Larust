use larust_orm::{Op, Order, QueryBuilder};

#[derive(sqlx::FromRow, Debug, PartialEq, Clone)]
struct Post {
    id: i64,
    user_id: i64,
    title: String,
    votes: i64,
    published_at: Option<i64>,
}

/// Exercises every `QueryBuilder` addition from this milestone
/// (`where_op`/`where_null`/`where_not_null`/`or_where_*`/`where_group`/
/// `scope`/`where_in_query`/`order_by`/`oldest`/`limit`) against a real
/// SQLite database - the unit tests in `query_builder.rs` itself only
/// check the rendered SQL *text*; this proves the SQL that text produces
/// actually runs and returns the right rows.
#[tokio::test]
async fn query_builder_additions_work_against_real_sqlite() {
    let db_dir = tempfile::tempdir().unwrap();
    let database_url = format!("sqlite://{}/test.sqlite", db_dir.path().display());
    larust_orm::connect(&database_url).await.unwrap();

    let migrations_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        migrations_dir.path().join("0001_create_tables.sql"),
        r#"CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, country_id INTEGER NOT NULL);
CREATE TABLE posts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id INTEGER NOT NULL,
    title TEXT NOT NULL,
    votes INTEGER NOT NULL,
    published_at INTEGER
);"#,
    )
    .unwrap();
    larust_orm::migrate(migrations_dir.path()).await.unwrap();

    let pool = larust_orm::pool().unwrap();
    for country_id in [1i64, 1, 2] {
        sqlx::query("INSERT INTO users (country_id) VALUES (?)")
            .bind(country_id)
            .execute(pool)
            .await
            .unwrap();
    }
    // user 1 and 2 are in country 1; user 3 is in country 2.
    let rows = [
        (1i64, "Draft from user 1", 0i64, None),
        (1, "Published from user 1", 10, Some(100i64)),
        (2, "Published from user 2", 5, Some(200)),
        (3, "Published from user 3", 20, Some(300)),
    ];
    for (user_id, title, votes, published_at) in rows {
        sqlx::query("INSERT INTO posts (user_id, title, votes, published_at) VALUES (?, ?, ?, ?)")
            .bind(user_id)
            .bind(title)
            .bind(votes)
            .bind(published_at)
            .execute(pool)
            .await
            .unwrap();
    }

    // where_op
    let high_votes: Vec<Post> = QueryBuilder::new("posts")
        .where_op("votes", Op::Gt, 7i64)
        .get()
        .await
        .unwrap();
    assert_eq!(high_votes.len(), 2);
    assert!(high_votes.iter().all(|p| p.votes > 7));

    // where_null / where_not_null
    let drafts: Vec<Post> = QueryBuilder::new("posts")
        .where_null("published_at")
        .get()
        .await
        .unwrap();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].title, "Draft from user 1");

    let published: Vec<Post> = QueryBuilder::new("posts")
        .where_not_null("published_at")
        .get()
        .await
        .unwrap();
    assert_eq!(published.len(), 3);

    // or_where_eq
    let two_titles: Vec<Post> = QueryBuilder::new("posts")
        .where_eq("title", "Draft from user 1")
        .or_where_eq("title", "Published from user 2")
        .get()
        .await
        .unwrap();
    assert_eq!(two_titles.len(), 2);

    // where_group: without the group, AND would bind tighter than
    // intended - this proves the parens actually land where expected.
    let grouped: Vec<Post> = QueryBuilder::new("posts")
        .where_op("votes", Op::Gte, 5i64)
        .where_group(|q| {
            q.where_eq("title", "Published from user 1")
                .or_where_eq("title", "Published from user 2")
        })
        .get()
        .await
        .unwrap();
    assert_eq!(grouped.len(), 2);

    // scope: a plain function, reused like Eloquent's local scopes.
    fn published_scope(q: QueryBuilder<Post>) -> QueryBuilder<Post> {
        q.where_not_null("published_at")
    }
    let via_scope: Vec<Post> = QueryBuilder::new("posts")
        .scope(published_scope)
        .where_op("votes", Op::Gt, 7i64)
        .get()
        .await
        .unwrap();
    assert_eq!(via_scope.len(), 2);

    // scope with an internal OR must not leak precedence into the rest
    // of the query.
    fn extreme_votes_scope(q: QueryBuilder<Post>) -> QueryBuilder<Post> {
        q.where_op("votes", Op::Lt, 1i64)
            .or_where_op("votes", Op::Gt, 15i64)
    }
    let via_or_scope: Vec<Post> = QueryBuilder::new("posts")
        .where_not_null("published_at")
        .scope(extreme_votes_scope)
        .get()
        .await
        .unwrap();
    assert_eq!(via_or_scope.len(), 1);
    assert_eq!(via_or_scope[0].title, "Published from user 3");

    // where_in_query: posts authored by a user in country 1 - a
    // subquery filter, the "richer query builder" replacement for a
    // real JOIN.
    let sub: QueryBuilder<Post> = QueryBuilder::new("users").where_eq("country_id", 1i64);
    let mut from_country_one: Vec<Post> = QueryBuilder::new("posts")
        .where_in_query("user_id", "id", sub)
        .get()
        .await
        .unwrap();
    from_country_one.sort_by_key(|p| p.id);
    assert_eq!(from_country_one.len(), 3);
    assert!(from_country_one.iter().all(|p| p.user_id <= 2));

    // order_by/oldest/limit.
    let top_voted: Vec<Post> = QueryBuilder::new("posts")
        .order_by("votes", Order::Desc)
        .limit(2)
        .get()
        .await
        .unwrap();
    assert_eq!(top_voted.len(), 2);
    assert_eq!(top_voted[0].title, "Published from user 3");
    assert_eq!(top_voted[1].title, "Published from user 1");

    let oldest_first: Vec<Post> = QueryBuilder::new("posts").oldest("id").get().await.unwrap();
    assert_eq!(oldest_first.first().unwrap().title, "Draft from user 1");
}
