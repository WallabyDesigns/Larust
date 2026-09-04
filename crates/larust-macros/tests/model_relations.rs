use larust_support::Model;

#[derive(Model, sqlx::FromRow, Debug, PartialEq, Clone)]
#[table("users")]
#[has_many(Post, foreign_key = "user_id")]
#[has_one(Profile, foreign_key = "user_id")]
pub struct User {
    #[primary_key]
    pub id: i64,
    pub name: String,
}

#[derive(Model, sqlx::FromRow, Debug, PartialEq, Clone)]
#[table("posts")]
#[belongs_to(User, foreign_key = "user_id")]
pub struct Post {
    #[primary_key]
    pub id: i64,
    pub user_id: i64,
    pub title: String,
}

#[derive(Model, sqlx::FromRow, Debug, PartialEq, Clone)]
#[table("profiles")]
#[belongs_to(User, foreign_key = "user_id", method = "owner")]
pub struct Profile {
    #[primary_key]
    pub id: i64,
    pub user_id: i64,
    pub bio: String,
}

// A `belongs_to` whose related struct's primary key is *not* named `id` -
// exercises the `related_key = "..."` override (the default only covers
// the common case every other struct in this file already uses).
#[derive(Model, sqlx::FromRow, Debug, PartialEq, Clone)]
#[table("categories")]
pub struct Category {
    #[primary_key]
    pub category_id: i64,
    pub name: String,
}

#[derive(Model, sqlx::FromRow, Debug, PartialEq, Clone)]
#[table("tags")]
#[belongs_to(Category, foreign_key = "category_id", related_key = "category_id")]
pub struct Tag {
    #[primary_key]
    pub id: i64,
    pub category_id: i64,
    pub label: String,
}

// A relationship where the SQL column name is a Rust keyword (`type`) on
// *both* sides at once: `Kind`'s own primary key, and `Widget`'s foreign
// key referencing it. Rust can only spell this field as `r#type`, but the
// SQL column is plainly named `type` - every generated query must use the
// clean `"type"` string, and every generated field access must use the
// raw identifier `r#type`, and those two must never get crossed (a real
// bug caught during review: see docs/GOTCHAS.md).
#[derive(Model, sqlx::FromRow, Debug, PartialEq, Clone)]
#[table("kinds")]
#[has_many(Widget, foreign_key = "type")]
pub struct Kind {
    #[primary_key]
    pub r#type: i64,
    pub label: String,
}

#[derive(Model, sqlx::FromRow, Debug, PartialEq, Clone)]
#[table("widgets")]
#[belongs_to(Kind, foreign_key = "type", related_key = "type")]
pub struct Widget {
    #[primary_key]
    pub id: i64,
    pub r#type: i64,
    pub name: String,
}

#[tokio::test]
async fn relationships_round_trip_against_real_sqlite() {
    let db_dir = tempfile::tempdir().unwrap();
    let database_url = format!("sqlite://{}/test.sqlite", db_dir.path().display());
    larust_support::orm::connect(&database_url).await.unwrap();

    let migrations_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        migrations_dir.path().join("0001_create_tables.sql"),
        "CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL); \
         CREATE TABLE posts (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id INTEGER NOT NULL, title TEXT NOT NULL); \
         CREATE TABLE profiles (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id INTEGER NOT NULL, bio TEXT NOT NULL); \
         CREATE TABLE categories (category_id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL); \
         CREATE TABLE tags (id INTEGER PRIMARY KEY AUTOINCREMENT, category_id INTEGER NOT NULL, label TEXT NOT NULL); \
         CREATE TABLE kinds (\"type\" INTEGER PRIMARY KEY AUTOINCREMENT, label TEXT NOT NULL); \
         CREATE TABLE widgets (id INTEGER PRIMARY KEY AUTOINCREMENT, \"type\" INTEGER NOT NULL, name TEXT NOT NULL);",
    )
    .unwrap();
    larust_support::orm::migrate(migrations_dir.path())
        .await
        .unwrap();

    let alice = User::create(NewUser {
        name: "Alice".to_string(),
    })
    .await
    .unwrap();
    let bob = User::create(NewUser {
        name: "Bob".to_string(),
    })
    .await
    .unwrap();

    let post1 = Post::create(NewPost {
        user_id: alice.id,
        title: "First".to_string(),
    })
    .await
    .unwrap();
    let post2 = Post::create(NewPost {
        user_id: alice.id,
        title: "Second".to_string(),
    })
    .await
    .unwrap();
    Post::create(NewPost {
        user_id: bob.id,
        title: "Bob's post".to_string(),
    })
    .await
    .unwrap();

    // has_many: only Alice's posts, not Bob's.
    let alice_posts = alice.posts().await.unwrap();
    assert_eq!(alice_posts.len(), 2);
    assert!(alice_posts.iter().any(|p| p.id == post1.id));
    assert!(alice_posts.iter().any(|p| p.id == post2.id));

    let bob_posts = bob.posts().await.unwrap();
    assert_eq!(bob_posts.len(), 1);

    // belongs_to: resolves the owning user.
    let post_author = post1.user().await.unwrap();
    assert_eq!(post_author.map(|u| u.id), Some(alice.id));

    // has_one: None before a related row exists.
    assert!(alice.profile().await.unwrap().is_none());

    // has_one: Some after one is created; still None for a user with no
    // profile at all.
    let profile = Profile::create(NewProfile {
        user_id: alice.id,
        bio: "Hi, I'm Alice".to_string(),
    })
    .await
    .unwrap();
    let found_profile = alice.profile().await.unwrap();
    assert_eq!(found_profile.map(|p| p.id), Some(profile.id));
    assert!(bob.profile().await.unwrap().is_none());

    // belongs_to with a `method = "..."` override - the generated method is
    // named `owner`, not the default-derived `user`.
    let owner = profile.owner().await.unwrap();
    assert_eq!(owner.map(|u| u.id), Some(alice.id));

    // --- Batch (eager) loading: same data as the per-instance methods
    // above, but every relationship kind fetched in one query instead of
    // one query per row. ---

    let posts_by_user = User::load_posts(&[alice.clone(), bob.clone()])
        .await
        .unwrap();
    assert_eq!(posts_by_user.get(&alice.id).map(Vec::len), Some(2));
    assert_eq!(posts_by_user.get(&bob.id).map(Vec::len), Some(1));

    let profile_by_user = User::load_profile(&[alice.clone(), bob.clone()])
        .await
        .unwrap();
    assert_eq!(
        profile_by_user.get(&alice.id).map(|p| p.id),
        Some(profile.id)
    );
    assert_eq!(profile_by_user.get(&bob.id), None);

    let author_by_post = Post::load_user(&[post1.clone(), post2.clone()])
        .await
        .unwrap();
    // Both posts belong to the same author, so the map has one entry, not
    // two - grouping by the *related* row's id, not the input row's.
    assert_eq!(author_by_post.len(), 1);
    assert_eq!(author_by_post.get(&alice.id).map(|u| u.id), Some(alice.id));

    let owner_by_profile = Profile::load_owner(&[profile]).await.unwrap();
    assert_eq!(
        owner_by_profile.get(&alice.id).map(|u| u.id),
        Some(alice.id)
    );

    // An empty input slice must not error - "col" IN () is invalid SQL in
    // SQLite, so `where_in` (which every batch loader is built on) has to
    // guard against it internally.
    let empty = User::load_posts(&[]).await.unwrap();
    assert!(empty.is_empty());

    // related_key override: Category's primary key is `category_id`, not
    // `id` - Tag::load_category must group by that, not the "id" default.
    let books = Category::create(NewCategory {
        name: "Books".to_string(),
    })
    .await
    .unwrap();
    let tag1 = Tag::create(NewTag {
        category_id: books.category_id,
        label: "fiction".to_string(),
    })
    .await
    .unwrap();
    let tag2 = Tag::create(NewTag {
        category_id: books.category_id,
        label: "non-fiction".to_string(),
    })
    .await
    .unwrap();

    let category_for_tag1 = tag1.category().await.unwrap();
    assert_eq!(
        category_for_tag1.map(|c| c.category_id),
        Some(books.category_id)
    );

    let categories_by_tag = Tag::load_category(&[tag1, tag2]).await.unwrap();
    assert_eq!(categories_by_tag.len(), 1);
    assert_eq!(
        categories_by_tag
            .get(&books.category_id)
            .map(|c| c.name.clone()),
        Some("Books".to_string())
    );

    // Keyword-shaped column name (`type`) on both the foreign key
    // (`has_many`/`belongs_to`) and the related-row primary key
    // (`related_key`) - the exact scenario a prior version of this macro
    // got wrong (see docs/GOTCHAS.md).
    let widget_kind = Kind::create(NewKind {
        label: "Gadget".to_string(),
    })
    .await
    .unwrap();
    let widget1 = Widget::create(NewWidget {
        r#type: widget_kind.r#type,
        name: "Widget A".to_string(),
    })
    .await
    .unwrap();
    let widget2 = Widget::create(NewWidget {
        r#type: widget_kind.r#type,
        name: "Widget B".to_string(),
    })
    .await
    .unwrap();

    // belongs_to, per-instance: field access to `r#type` on both sides.
    let kind_for_widget = widget1.kind().await.unwrap();
    assert_eq!(kind_for_widget.map(|k| k.r#type), Some(widget_kind.r#type));

    // has_many, per-instance and batch: the SQL column stays "type", not
    // "r#type" - if it didn't, this query would silently match nothing
    // (SQLite treats an unmatched double-quoted identifier as a string
    // literal rather than erroring) instead of finding both widgets.
    let widgets_for_kind = widget_kind.widgets().await.unwrap();
    assert_eq!(widgets_for_kind.len(), 2);

    let widgets_by_kind = Kind::load_widgets(std::slice::from_ref(&widget_kind))
        .await
        .unwrap();
    assert_eq!(
        widgets_by_kind.get(&widget_kind.r#type).map(Vec::len),
        Some(2)
    );

    // belongs_to, batch: `related_key = "type"` must group by Kind's own
    // `r#type` field, using the clean "type" string for SQL.
    let kinds_by_widget = Widget::load_kind(&[widget1, widget2]).await.unwrap();
    assert_eq!(kinds_by_widget.len(), 1);
    assert_eq!(
        kinds_by_widget
            .get(&widget_kind.r#type)
            .map(|k| k.label.clone()),
        Some("Gadget".to_string())
    );
}
