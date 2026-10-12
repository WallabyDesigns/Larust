//! End-to-end proof that `#[has_many_through(...)]`'s generated
//! `{method}_query()`/`{method}()` actually run the right SQL against a
//! real SQLite database - mirrors `model.rs`'s own
//! `model_crud_round_trip_against_real_sqlite` in shape.
//!
//! `Country` has many `Post` through `User` - Laravel's own canonical
//! `hasManyThrough` example.

use larust_support::Model;

#[derive(Model, sqlx::FromRow, Debug, PartialEq, Clone)]
#[table("countries")]
#[has_many_through(Post, through = User, first_key = "country_id", second_key = "user_id")]
pub struct Country {
    #[primary_key]
    pub id: i64,
    pub name: String,
}

#[derive(Model, sqlx::FromRow, Debug, PartialEq, Clone)]
#[table("users")]
pub struct User {
    #[primary_key]
    pub id: i64,
    pub country_id: i64,
    pub name: String,
}

#[derive(Model, sqlx::FromRow, Debug, PartialEq, Clone)]
#[table("posts")]
pub struct Post {
    #[primary_key]
    pub id: i64,
    pub user_id: i64,
    pub title: String,
}

#[tokio::test]
async fn has_many_through_fetches_posts_across_the_through_table_against_real_sqlite() {
    let db_dir = tempfile::tempdir().unwrap();
    let database_url = format!("sqlite://{}/test.sqlite", db_dir.path().display());
    larust_support::orm::connect(&database_url).await.unwrap();

    let migrations_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        migrations_dir.path().join("0001_create_tables.sql"),
        "CREATE TABLE countries (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL); \
         CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, country_id INTEGER NOT NULL, name TEXT NOT NULL); \
         CREATE TABLE posts (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id INTEGER NOT NULL, title TEXT NOT NULL);",
    )
    .unwrap();
    larust_support::orm::migrate(migrations_dir.path())
        .await
        .unwrap();

    let usa = Country::create(NewCountry {
        name: "USA".to_string(),
    })
    .await
    .unwrap();
    let canada = Country::create(NewCountry {
        name: "Canada".to_string(),
    })
    .await
    .unwrap();

    let alice = User::create(NewUser {
        country_id: usa.id,
        name: "Alice".to_string(),
    })
    .await
    .unwrap();
    let bob = User::create(NewUser {
        country_id: usa.id,
        name: "Bob".to_string(),
    })
    .await
    .unwrap();
    let carol = User::create(NewUser {
        country_id: canada.id,
        name: "Carol".to_string(),
    })
    .await
    .unwrap();

    Post::create(NewPost {
        user_id: alice.id,
        title: "Alice's post".to_string(),
    })
    .await
    .unwrap();
    Post::create(NewPost {
        user_id: bob.id,
        title: "Bob's post".to_string(),
    })
    .await
    .unwrap();
    Post::create(NewPost {
        user_id: carol.id,
        title: "Carol's post".to_string(),
    })
    .await
    .unwrap();

    // USA has two users, each with one post - both must come back,
    // Canada's post must not.
    let mut usa_posts = usa.posts().await.unwrap();
    usa_posts.sort_by_key(|p| p.id);
    assert_eq!(usa_posts.len(), 2);
    assert_eq!(usa_posts[0].title, "Alice's post");
    assert_eq!(usa_posts[1].title, "Bob's post");

    let canada_posts = canada.posts().await.unwrap();
    assert_eq!(canada_posts.len(), 1);
    assert_eq!(canada_posts[0].title, "Carol's post");

    // A country with no users at all gets an empty result, not an error.
    let empty_country = Country::create(NewCountry {
        name: "Nowhere".to_string(),
    })
    .await
    .unwrap();
    assert!(empty_country.posts().await.unwrap().is_empty());

    // The `_query()` form composes with the rest of QueryBuilder's own
    // chain - exactly the point of using `where_in_query` instead of a
    // hand-written join.
    let latest_usa_post = usa
        .posts_query()
        .latest(Post::ID)
        .limit(1)
        .get()
        .await
        .unwrap();
    assert_eq!(latest_usa_post.len(), 1);
    assert_eq!(latest_usa_post[0].title, "Bob's post");
}
