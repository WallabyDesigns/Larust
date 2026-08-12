use larust_support::Model;

// `#[belongs_to_many(...)]` is deliberately repeatable (a model can have
// more than one pivot-table relationship, e.g. tags *and* boards here) —
// clippy's `duplicated_attributes` lint doesn't distinguish "the same
// attribute path repeated with different arguments" (expected, supported)
// from "the exact same attribute repeated verbatim" (a real mistake it's
// designed to catch), so it flags this legitimate case as a false
// positive. See docs/GOTCHAS.md.
#[allow(clippy::duplicated_attributes)]
#[derive(Model, sqlx::FromRow, Debug, PartialEq, Clone)]
#[table("posts")]
#[belongs_to_many(
    Tag,
    through = "post_tag",
    foreign_key = "post_id",
    related_pivot_key = "tag_id"
)]
#[belongs_to_many(
    Board,
    through = "post_board",
    foreign_key = "post_id",
    related_pivot_key = "board_id",
    related_key = "board_id",
    method = "pinned_boards"
)]
pub struct Post {
    #[primary_key]
    pub id: i64,
    pub title: String,
}

#[derive(Model, sqlx::FromRow, Debug, PartialEq, Clone)]
#[table("tags")]
pub struct Tag {
    #[primary_key]
    pub id: i64,
    pub name: String,
}

// Primary key named something other than `id` — exercises the
// `related_key = "..."` override (default only covers the common case
// `Tag` already uses).
#[derive(Model, sqlx::FromRow, Debug, PartialEq, Clone)]
#[table("boards")]
pub struct Board {
    #[primary_key]
    pub board_id: i64,
    pub name: String,
}

#[tokio::test]
async fn belongs_to_many_round_trip_against_real_sqlite() {
    let db_dir = tempfile::tempdir().unwrap();
    let database_url = format!("sqlite://{}/test.sqlite", db_dir.path().display());
    larust_support::orm::connect(&database_url).await.unwrap();

    let migrations_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        migrations_dir.path().join("0001_create_tables.sql"),
        "CREATE TABLE posts (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL); \
         CREATE TABLE tags (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL); \
         CREATE TABLE post_tag (post_id INTEGER NOT NULL, tag_id INTEGER NOT NULL, PRIMARY KEY (post_id, tag_id)); \
         CREATE TABLE boards (board_id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL); \
         CREATE TABLE post_board (post_id INTEGER NOT NULL, board_id INTEGER NOT NULL, PRIMARY KEY (post_id, board_id));",
    )
    .unwrap();
    larust_support::orm::migrate(migrations_dir.path())
        .await
        .unwrap();

    let post = Post::create(NewPost {
        title: "Hello, Larust".to_string(),
    })
    .await
    .unwrap();
    let other_post = Post::create(NewPost {
        title: "Unrelated".to_string(),
    })
    .await
    .unwrap();
    let rust_tag = Tag::create(NewTag {
        name: "rust".to_string(),
    })
    .await
    .unwrap();
    let web_tag = Tag::create(NewTag {
        name: "web".to_string(),
    })
    .await
    .unwrap();

    // Empty before anything is attached.
    assert!(post.tags().await.unwrap().is_empty());

    // attach: adds a pivot row; the accessor reflects it immediately.
    post.attach_tag(rust_tag.id).await.unwrap();
    post.attach_tag(web_tag.id).await.unwrap();
    let mut tags = post.tags().await.unwrap();
    tags.sort_by_key(|t| t.id);
    assert_eq!(tags.len(), 2);
    assert!(tags.iter().any(|t| t.id == rust_tag.id));
    assert!(tags.iter().any(|t| t.id == web_tag.id));

    // attach on an already-attached pair is a no-op, not a UNIQUE-
    // constraint error (INSERT OR IGNORE).
    post.attach_tag(rust_tag.id).await.unwrap();
    assert_eq!(post.tags().await.unwrap().len(), 2);

    // Another post's pivot rows are untouched.
    assert!(other_post.tags().await.unwrap().is_empty());

    // detach: removes exactly one pivot row.
    post.detach_tag(web_tag.id).await.unwrap();
    let tags_after_detach = post.tags().await.unwrap();
    assert_eq!(tags_after_detach.len(), 1);
    assert_eq!(tags_after_detach[0].id, rust_tag.id);

    // sync: replaces the full set in one call.
    post.sync_tags(&[web_tag.id]).await.unwrap();
    let synced = post.tags().await.unwrap();
    assert_eq!(synced.len(), 1);
    assert_eq!(synced[0].id, web_tag.id);

    // sync with an empty slice clears every pivot row for this post.
    post.sync_tags(&[]).await.unwrap();
    assert!(post.tags().await.unwrap().is_empty());

    // Re-establish a known set, then prove `sync_tags` is transactional:
    // a duplicate id in the input violates the pivot table's composite
    // primary key partway through the insert loop, and the whole sync
    // must roll back rather than leaving the post's tags half-deleted.
    post.sync_tags(&[web_tag.id]).await.unwrap();
    let sync_result = post.sync_tags(&[rust_tag.id, rust_tag.id]).await;
    assert!(sync_result.is_err());
    let tags_after_failed_sync = post.tags().await.unwrap();
    assert_eq!(
        tags_after_failed_sync.len(),
        1,
        "a failed sync must leave the original pivot rows untouched, not partially cleared"
    );
    assert_eq!(tags_after_failed_sync[0].id, web_tag.id);

    // belongs_to_many with `related_key`/`method` overrides: Board's
    // primary key is `board_id`, not `id`, and the accessor is named
    // `pinned_boards`, not the default-derived `boards`.
    let board = Board::create(NewBoard {
        name: "Featured".to_string(),
    })
    .await
    .unwrap();
    assert!(post.pinned_boards().await.unwrap().is_empty());
    post.attach_board(board.board_id).await.unwrap();
    let boards = post.pinned_boards().await.unwrap();
    assert_eq!(boards.len(), 1);
    assert_eq!(boards[0].board_id, board.board_id);
    post.detach_board(board.board_id).await.unwrap();
    assert!(post.pinned_boards().await.unwrap().is_empty());
}
